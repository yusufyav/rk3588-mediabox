//! The fan: what the kernel is doing with it, and the curve it is given at boot.
//!
//! This module never sets a duty. The vendor RK3588 kernel's `pwm-fan` driver
//! drives the fan from two properties of its device-tree node --
//! `cooling-levels` and `rockchip,temp-trips` -- and it keeps doing so whether
//! or not this daemon is running. A userspace loop beside it would be a second
//! governor with its own failure modes (a daemon that crashed with the fan at
//! 20 % is a SoC that cooks), so a chosen curve is instead written into a
//! device-tree overlay that the boot loader applies, and it takes effect on the
//! next boot. Reading is all this module does to the running fan.
//!
//! Three things are found rather than assumed, because they are numbered by
//! probe order and the Plus and the Ultra do not number them alike:
//!
//! * the fan is the hwmon device whose `name` is `pwmfan`;
//! * its device-tree node is wherever that device's `of_node` points;
//! * the SoC temperature is the thermal zone whose `type` is `soc-thermal`.
//!
//! The overlay is produced here, as bytes, rather than by writing a source file
//! and running `dtc` on it. It contains two arrays of integers that have
//! already passed [`FanCurve::validate`] and the path of a node that was read
//! out of sysfs, and nothing that came from a request reaches a command line
//! because there is no command line.
//!
//! What the boot loader does with a broken overlay is the reason that matters.
//! Armbian's boot script applies every `user_overlays` entry in turn and, if
//! any one of them fails to apply, throws the whole lot away and boots the
//! board's plain device tree -- the HDMI crossbar and the Plus carrier fix go
//! with it. A file that is merely absent is skipped. So the file is either the
//! whole new overlay or the old one, never half of either: it is written beside
//! the old one, flushed, and renamed over it.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mediabox_core::{FanBoardFix, FanCurve, FanCurveError, FanStatus};
use serde_json::{Value, json};

/// The `user_overlays` entry, and file name, of the curve this module writes.
pub const CURVE_OVERLAY: &str = "mediabox-fan-curve";

/// The Plus's carrier correction. Installed by `mediabox-fan-setup`, never by
/// this daemon, and never removed by a curve reset.
pub const PLUS_FIX_OVERLAY: &str = "mediabox-fan-opi5plus-50hz";

/// The carrier the Plus's correction sets: 20 ms, 50 Hz -- the Ultra's own.
pub const PLUS_FIX_PERIOD_NS: u64 = 20_000_000;

/// Who drives the duty. There is only ever this one.
pub const CONTROL_BACKEND: &str = "kernel-pwm-fan";

/// Where each input lives. Absolute on the appliance; under a temporary
/// directory in the tests, so a test never reads this machine's own fan.
#[derive(Debug, Clone)]
pub struct FanPaths {
    /// `/sys`: `class/hwmon`, `class/thermal` and `firmware/devicetree/base`.
    pub sys: PathBuf,
    /// `/proc/device-tree/model`.
    pub model: PathBuf,
    /// `/boot/armbianEnv.txt`, read to see whether the curve is loaded at all.
    pub boot_env: PathBuf,
    /// `/boot/overlay-user`, where the curve overlay is written.
    pub overlay_dir: PathBuf,
    /// The curve last saved, and in which boot.
    pub state: PathBuf,
    /// `/proc/sys/kernel/random/boot_id`: a saved curve is pending until the
    /// boot it was saved in has ended.
    pub boot_id: PathBuf,
}

impl FanPaths {
    pub fn system() -> Self {
        Self {
            sys: "/sys".into(),
            model: "/proc/device-tree/model".into(),
            boot_env: "/boot/armbianEnv.txt".into(),
            overlay_dir: "/boot/overlay-user".into(),
            state: "/var/lib/mediabox/fan-curve.json".into(),
            boot_id: "/proc/sys/kernel/random/boot_id".into(),
        }
    }

    /// The same layout under `root`.
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            sys: root.join("sys"),
            model: root.join("proc/device-tree/model"),
            boot_env: root.join("boot/armbianEnv.txt"),
            overlay_dir: root.join("boot/overlay-user"),
            state: root.join("var/lib/mediabox/fan-curve.json"),
            boot_id: root.join("proc/sys/kernel/random/boot_id"),
        }
    }

    fn overlay_file(&self) -> PathBuf {
        self.overlay_dir.join(format!("{CURVE_OVERLAY}.dtbo"))
    }
}

/// Why a curve could not be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanError {
    Invalid(FanCurveError),
    Unavailable(String),
    BootConfig(String),
    Write(String),
}

impl FanError {
    /// The control plane's error code for it.
    pub fn code(&self) -> &'static str {
        match self {
            FanError::Invalid(_) => "FAN_CURVE_INVALID",
            FanError::Unavailable(_) => "FAN_UNAVAILABLE",
            FanError::BootConfig(_) => "FAN_BOOT_CONFIG",
            FanError::Write(_) => "FAN_WRITE_ERROR",
        }
    }
}

impl std::fmt::Display for FanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FanError::Invalid(error) => write!(f, "{error}"),
            FanError::Unavailable(why) | FanError::BootConfig(why) | FanError::Write(why) => {
                write!(f, "{why}")
            }
        }
    }
}

/// The fan and the file that carries its next curve. Saving and resetting are
/// serialised, so two requests cannot interleave their writes.
pub struct FanController {
    paths: FanPaths,
    writing: Mutex<()>,
}

impl FanController {
    pub fn new(paths: FanPaths) -> Self {
        Self {
            paths,
            writing: Mutex::new(()),
        }
    }

    pub fn system() -> Self {
        Self::new(FanPaths::system())
    }

    pub fn status(&self) -> FanStatus {
        status(&self.paths)
    }

    /// Save a curve for the next boot.
    ///
    /// The overlay first and the record of it second, so a curve reported as
    /// saved is one whose file is on disk. Saving the curve that is already
    /// saved writes nothing, and so does not make a reboot pending.
    pub fn set(&self, curve: FanCurve) -> Result<FanStatus, FanError> {
        // Checked here whatever the caller checked: this is the function that
        // writes the boot configuration.
        curve.validate().map_err(FanError::Invalid)?;
        let _writing = self.writing.lock().expect("fan write mutex");

        let fan = discover(&self.paths.sys).ok_or_else(|| {
            FanError::Unavailable("bu kartta pwm-fan denetimli bir fan bulunamadı".into())
        })?;
        let node = fan
            .node
            .ok_or_else(|| FanError::Unavailable("fanın device-tree düğümü okunamadı".into()))?;
        if !boot_loads_curve(&self.paths) {
            return Err(FanError::BootConfig(format!(
                "önyükleme yapılandırması {CURVE_OVERLAY} katmanını yüklemiyor; \
                 kurulum betiği bu kartta çalıştırılmalı"
            )));
        }

        let blob = curve_overlay(&node, &curve);
        let saved = read_state(&self.paths.state);
        let on_disk = fs::read(self.paths.overlay_file()).ok();
        if on_disk.as_deref() == Some(blob.as_slice())
            && saved.as_ref().and_then(|state| state.curve.as_ref()) == Some(&curve)
        {
            return Ok(self.status());
        }

        write_atomically(&self.paths.overlay_file(), &blob)
            .map_err(|error| FanError::Write(format!("fan eğrisi yazılamadı: {error}")))?;
        write_state(&self.paths, Some(&curve)).map_err(|error| {
            FanError::Write(format!("fan eğrisi yazıldı ama kaydı tutulamadı: {error}"))
        })?;
        eprintln!(
            "mediaboxd.fan curve saved profile={:?} levels={:?}",
            curve.profile,
            curve.cooling_levels()
        );
        Ok(self.status())
    }

    /// Go back to this product's default curve from the next boot on
    /// ([`FanCurve::product_default`]).
    ///
    /// It is saved like any other curve: the same overlay file, nothing else.
    /// The Plus's carrier fix is a different file with a different job and
    /// is not touched, and neither is `user_overlays`.
    pub fn reset(&self) -> Result<FanStatus, FanError> {
        let status = self.set(FanCurve::product_default())?;
        eprintln!("mediaboxd.fan curve reset to the product default");
        Ok(status)
    }

    /// Give a board that has never had a curve saved this product's default.
    ///
    /// A clean install has no saved curve, and without this it would run
    /// whatever its vendor device tree says -- a different policy on every
    /// board, and on the golden board whatever was last saved there. A saved
    /// curve, the default or anybody's own, is never replaced. Takes effect
    /// on the next boot, like every curve.
    pub fn seed_default(&self) -> Result<Option<FanStatus>, FanError> {
        if read_state(&self.paths.state).is_some_and(|state| state.curve.is_some()) {
            return Ok(None);
        }
        let status = self.set(FanCurve::product_default())?;
        eprintln!("mediaboxd.fan no saved curve: product default written for the next boot");
        Ok(Some(status))
    }
}

// ------------------------------------------------------------------ reading

/// The fan as discovery found it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fan {
    hwmon: PathBuf,
    /// The node's path inside the device tree, e.g. `/pwm-fan`.
    node: Option<String>,
}

/// The one `pwmfan` hwmon device, whatever number it was given.
fn discover(sys: &Path) -> Option<Fan> {
    let base = sys.join("firmware/devicetree/base");
    let mut devices: Vec<PathBuf> = fs::read_dir(sys.join("class/hwmon"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| read_trimmed(&path.join("name")).as_deref() == Some("pwmfan"))
        .collect();
    devices.sort();
    let hwmon = devices.into_iter().next()?;
    let node = node_of(&hwmon, &base).or_else(|| node_by_compatible(&base));
    Some(Fan { hwmon, node })
}

/// Where the device's `of_node` link lands, as a path inside the tree.
fn node_of(hwmon: &Path, base: &Path) -> Option<String> {
    let base = base.canonicalize().ok()?;
    [hwmon.join("of_node"), hwmon.join("device/of_node")]
        .iter()
        .filter_map(|link| link.canonicalize().ok())
        .find_map(|real| {
            let inside = real.strip_prefix(&base).ok()?;
            node_path(inside)
        })
}

/// A top-level node compatible with `pwm-fan`, for a kernel that does not
/// link the hwmon device to its node.
fn node_by_compatible(base: &Path) -> Option<String> {
    let mut nodes: Vec<PathBuf> = fs::read_dir(base)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            fs::read(path.join("compatible")).is_ok_and(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .any(|entry| entry == b"pwm-fan")
            })
        })
        .collect();
    nodes.sort();
    node_path(nodes.first()?.strip_prefix(base).ok()?)
}

/// `/a/b` from a relative path, if every component is a plain node name. The
/// result is written into the overlay's `target-path`.
fn node_path(inside: &Path) -> Option<String> {
    let mut path = String::new();
    for part in inside.components() {
        let std::path::Component::Normal(name) = part else {
            return None;
        };
        let name = name.to_str()?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b",._+@-".contains(&b))
        {
            return None;
        }
        path.push('/');
        path.push_str(name);
    }
    (!path.is_empty()).then_some(path)
}

/// The SoC's own thermal zone, whatever number it was given.
fn soc_temperature(sys: &Path) -> Option<f64> {
    let mut zones: Vec<PathBuf> = fs::read_dir(sys.join("class/thermal"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| read_trimmed(&path.join("type")).as_deref() == Some("soc-thermal"))
        .collect();
    zones.sort();
    let millicelsius: i64 = read_trimmed(&zones.first()?.join("temp"))?.parse().ok()?;
    Some(millicelsius as f64 / 1000.0)
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(
        text.trim_matches(|c: char| c.is_whitespace() || c == '\0')
            .to_string(),
    )
}

/// A device-tree property as big-endian cells.
fn cells(bytes: &[u8]) -> Option<Vec<u32>> {
    bytes.len().is_multiple_of(4).then(|| {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|cell| u32::from_be_bytes(*cell))
            .collect()
    })
}

/// The period out of a `pwms` specifier: `<&controller channel period flags>`,
/// or `<&controller channel period>` for a controller with two cells. Either
/// way it is the third cell.
pub fn pwm_period_ns(pwms: &[u8]) -> Option<u64> {
    let cells = cells(pwms)?;
    matches!(cells.len(), 3 | 4).then(|| u64::from(cells[2]))
}

/// Hertz from a period in nanoseconds.
pub fn frequency_hz(period_ns: u64) -> Option<f64> {
    (period_ns > 0).then(|| 1_000_000_000.0 / period_ns as f64)
}

/// Whether this board needs the carrier correction. The Plus and nothing
/// else -- in particular not the Ultra, whose own tree already runs at 50 Hz.
pub fn needs_plus_fix(model: &str) -> bool {
    model
        .trim_matches('\0')
        .trim()
        .ends_with("Orange Pi 5 Plus")
}

/// Whether `user_overlays` names the curve.
fn boot_loads_curve(paths: &FanPaths) -> bool {
    paths.overlay_dir.is_dir()
        && user_overlays(&paths.boot_env)
            .iter()
            .any(|entry| entry == CURVE_OVERLAY)
}

fn user_overlays(boot_env: &Path) -> Vec<String> {
    fs::read_to_string(boot_env)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("user_overlays=").map(str::to_owned))
        })
        .map(|list| list.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Everything there is to know about the fan, read from the machine now.
///
/// The one reading of it: the settings screen, the control client and the
/// diagnostics snapshot all come through here.
pub fn status(paths: &FanPaths) -> FanStatus {
    let board = read_trimmed(&paths.model).filter(|model| !model.is_empty());
    let temperature_c = soc_temperature(&paths.sys);
    let Some(fan) = discover(&paths.sys) else {
        return FanStatus {
            board,
            temperature_c,
            control_backend: CONTROL_BACKEND.into(),
            error: Some("bu kartta pwm-fan denetimli bir fan bulunamadı".into()),
            ..Default::default()
        };
    };

    let pwm: Option<u32> = read_trimmed(&fan.hwmon.join("pwm1")).and_then(|v| v.parse().ok());
    let pwm_enable: Option<u32> =
        read_trimmed(&fan.hwmon.join("pwm1_enable")).and_then(|v| v.parse().ok());
    let rpm_file = fan.hwmon.join("fan1_input");
    let rpm_available = rpm_file.is_file();
    let rpm = rpm_available
        .then(|| read_trimmed(&rpm_file).and_then(|v| v.parse().ok()))
        .flatten();

    let node = fan
        .node
        .as_ref()
        .map(|node| paths.sys.join("firmware/devicetree/base").join(&node[1..]));
    let property = |name: &str| {
        node.as_ref()
            .and_then(|node| fs::read(node.join(name)).ok())
    };
    let pwm_period_ns = property("pwms").as_deref().and_then(pwm_period_ns);
    let curve = match (
        property("cooling-levels").as_deref().and_then(cells),
        property("rockchip,temp-trips").as_deref().and_then(cells),
    ) {
        (Some(levels), Some(trips)) if trips.len() % 2 == 0 => {
            let trips: Vec<(u32, u32)> = trips
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[t, s]| (t, s))
                .collect();
            FanCurve::from_device_tree(&levels, &trips)
        }
        _ => None,
    };

    let board_fix = match (board.as_deref().is_some_and(needs_plus_fix), pwm_period_ns) {
        (false, _) => FanBoardFix::NotNeeded,
        (true, Some(PLUS_FIX_PERIOD_NS)) => FanBoardFix::Active,
        (true, _) => FanBoardFix::Missing,
    };

    let saved = read_state(&paths.state);
    let this_boot = read_trimmed(&paths.boot_id);
    let saved_this_boot = saved
        .as_ref()
        .is_some_and(|state| this_boot.is_some() && state.boot_id == this_boot);
    let configured = saved.and_then(|state| state.curve);
    // The tree holds trips, not points. When they are the trips of the curve
    // that was saved, that curve is what is running, with the points it was
    // made from.
    let running = |configured: &FanCurve| curve.as_ref().is_some_and(|c| c.same_kernel(configured));
    let curve = match &configured {
        Some(configured) if running(configured) => Some(configured.clone()),
        _ => curve,
    };
    let running = |configured: &FanCurve| curve.as_ref().is_some_and(|c| c.same_kernel(configured));
    let pending_reboot = saved_this_boot
        && match &configured {
            Some(configured) => !running(configured),
            // A reset in this boot: the running tree may still carry the
            // removed curve, and cannot say what the board's own one is.
            None => true,
        };

    let mut warnings = Vec::new();
    if board_fix == FanBoardFix::Missing {
        warnings.push(format!(
            "bu kartın 50 Hz PWM düzeltmesi etkin değil ({PLUS_FIX_OVERLAY})"
        ));
    }
    if let Some(configured) = &configured
        && !pending_reboot
        && !running(configured)
    {
        warnings.push("kaydedilen fan eğrisi bu açılışta etkin değil".into());
    }

    FanStatus {
        available: true,
        board,
        temperature_c,
        pwm,
        pwm_percent: pwm.map(|duty| (f64::from(duty) * 1000.0 / 255.0).round() / 10.0),
        pwm_enable,
        pwm_period_ns,
        pwm_frequency_hz: pwm_period_ns.and_then(frequency_hz),
        rpm_available,
        rpm,
        control_backend: CONTROL_BACKEND.into(),
        curve,
        configured,
        pending_reboot,
        board_fix,
        boot_config_ready: boot_loads_curve(paths),
        warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        error: None,
    }
}

/// The diagnostics snapshot's fan section: the same reading, in the snapshot's
/// own spelling.
pub fn diagnostics(status: &FanStatus) -> Value {
    json!({
        "available": status.available,
        "temperatureC": status.temperature_c,
        "pwm": status.pwm,
        "pwmPercent": status.pwm_percent,
        "periodNs": status.pwm_period_ns,
        "frequencyHz": status.pwm_frequency_hz,
        "rpmAvailable": status.rpm_available,
        "backend": status.control_backend,
        "profile": status.curve.as_ref().map(|curve| curve.profile),
        "coolingLevels": status.curve.as_ref().map(FanCurve::cooling_levels),
        "boardFix": status.board_fix,
        "pendingReboot": status.pending_reboot,
        "warning": status.warning,
        "error": status.error,
    })
}

// ---------------------------------------------------------- the saved curve

#[derive(Debug, Clone, PartialEq)]
struct Saved {
    curve: Option<FanCurve>,
    boot_id: Option<String>,
}

fn read_state(path: &Path) -> Option<Saved> {
    let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let curve = match value.get("curve") {
        None | Some(Value::Null) => None,
        // A record that no longer passes the rules is not reported as the
        // configured curve.
        Some(curve) => Some(
            serde_json::from_value::<FanCurve>(curve.clone())
                .ok()
                .filter(|curve| curve.validate().is_ok())?,
        ),
    };
    let boot_id = value
        .get("boot_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(Saved { curve, boot_id })
}

fn write_state(paths: &FanPaths, curve: Option<&FanCurve>) -> io::Result<()> {
    if let Some(parent) = paths.state.parent() {
        fs::create_dir_all(parent)?;
    }
    let record = json!({
        "curve": curve,
        "boot_id": read_trimmed(&paths.boot_id),
    });
    write_atomically(&paths.state, format!("{record:#}\n").as_bytes())
}

/// The new contents beside the old, flushed, then renamed over it: whoever
/// reads the file -- the boot loader included -- sees one or the other.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let temporary = dir.join(format!(".{}.next", name.to_string_lossy()));
    let written = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        // Readable by the boot loader and by anybody inspecting /boot, as the
        // installer's own overlays are; the daemon's umask would say 0640.
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written?;
    sync_dir(dir);
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(dir) = fs::File::open(dir) {
        let _ = dir.sync_all();
    }
}

// ------------------------------------------------------------- the overlay

/// The curve as a compiled device-tree overlay, equivalent to
///
/// ```text
/// /dts-v1/;
/// /plugin/;
/// / {
///     fragment@0 {
///         target-path = "<node>";
///         __overlay__ {
///             cooling-levels = <0 p1 p2 p3 p4 p5>;
///             rockchip,temp-trips = <t1 1 t2 2 t3 3 t4 4 t5 5>;
///         };
///     };
/// };
/// ```
///
/// `target-path` rather than a label: it needs no `__fixups__`, so the overlay
/// depends on nothing in the base tree but the node it changes. The same curve
/// always produces the same bytes.
pub fn curve_overlay(node: &str, curve: &FanCurve) -> Vec<u8> {
    let trips: Vec<u32> = curve
        .temp_trips()
        .into_iter()
        .flat_map(|(millicelsius, state)| [millicelsius, state])
        .collect();
    let mut tree = Fdt::default();
    tree.begin("");
    tree.begin("fragment@0");
    tree.property("target-path", &nul_terminated(node));
    tree.begin("__overlay__");
    tree.property("cooling-levels", &be_cells(&curve.cooling_levels()));
    tree.property("rockchip,temp-trips", &be_cells(&trips));
    tree.end();
    tree.end();
    tree.end();
    tree.finish()
}

fn nul_terminated(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn be_cells(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}

/// A flattened device tree, version 17, as the devicetree specification lays
/// it out: header, an empty memory reservation map, the structure block and
/// the strings block.
#[derive(Default)]
struct Fdt {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_END: u32 = 9;
const FDT_HEADER_LEN: usize = 40;
const FDT_RESERVE_LEN: usize = 16;

impl Fdt {
    fn token(&mut self, token: u32) {
        self.structure.extend_from_slice(&token.to_be_bytes());
    }

    fn pad(&mut self) {
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }

    fn begin(&mut self, name: &str) {
        self.token(FDT_BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
    }

    fn end(&mut self) {
        self.token(FDT_END_NODE);
    }

    fn property(&mut self, name: &str, value: &[u8]) {
        let offset = self.string_offset(name);
        self.token(FDT_PROP);
        self.structure
            .extend_from_slice(&(value.len() as u32).to_be_bytes());
        self.structure.extend_from_slice(&offset.to_be_bytes());
        self.structure.extend_from_slice(value);
        self.pad();
    }

    fn string_offset(&mut self, name: &str) -> u32 {
        let mut at = 0;
        for entry in self.strings.split(|byte| *byte == 0) {
            if entry == name.as_bytes() {
                return at as u32;
            }
            at += entry.len() + 1;
        }
        let offset = self.strings.len() as u32;
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        offset
    }

    fn finish(mut self) -> Vec<u8> {
        self.token(FDT_END);
        let structure_at = FDT_HEADER_LEN + FDT_RESERVE_LEN;
        let strings_at = structure_at + self.structure.len();
        let total = strings_at + self.strings.len();
        let header = [
            FDT_MAGIC,
            total as u32,
            structure_at as u32,
            strings_at as u32,
            FDT_HEADER_LEN as u32,
            17, // version
            16, // last compatible version
            0,  // boot cpu
            self.strings.len() as u32,
            self.structure.len() as u32,
        ];
        let mut blob = Vec::with_capacity(total);
        for field in header {
            blob.extend_from_slice(&field.to_be_bytes());
        }
        blob.extend_from_slice(&[0; FDT_RESERVE_LEN]);
        blob.extend_from_slice(&self.structure);
        blob.extend_from_slice(&self.strings);
        blob
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::{FanPoint, FanProfile};
    use std::os::unix::fs::symlink;

    /// The Plus's `pwms` as it was read off the running board, before and
    /// after the carrier correction.
    const PLUS_PWMS_20KHZ: [u8; 16] = [
        0x00, 0x00, 0x01, 0xcd, 0, 0, 0, 0, 0x00, 0x00, 0xc3, 0x50, 0, 0, 0, 0,
    ];
    const PLUS_PWMS_50HZ: [u8; 16] = [
        0x00, 0x00, 0x01, 0xcd, 0, 0, 0, 0, 0x01, 0x31, 0x2d, 0x00, 0, 0, 0, 0,
    ];

    /// A board, as sysfs, procfs and /boot present one.
    struct Board {
        dir: tempfile::TempDir,
        paths: FanPaths,
    }

    impl Board {
        /// `hwmon` and `zone` are the numbers the kernel happened to hand out;
        /// nothing here may depend on them.
        fn new(model: &str, hwmon: u32, zone: u32, pwms: &[u8]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let paths = FanPaths::under(dir.path());
            let base = paths.sys.join("firmware/devicetree/base");
            let node = base.join("pwm-fan");
            fs::create_dir_all(&node).unwrap();
            fs::write(node.join("compatible"), b"pwm-fan\0").unwrap();
            fs::write(node.join("pwms"), pwms).unwrap();
            // The vendor tree: the board's own curve, in steps.
            let vendor = FanCurve::board();
            fs::write(
                node.join("cooling-levels"),
                be_cells(&vendor.cooling_levels()),
            )
            .unwrap();
            let trips: Vec<u32> = vendor
                .temp_trips()
                .into_iter()
                .flat_map(|(t, s)| [t, s])
                .collect();
            fs::write(node.join("rockchip,temp-trips"), be_cells(&trips)).unwrap();

            // Some other hwmon devices first, so the fan is never the first
            // one read.
            let class = paths.sys.join("class/hwmon");
            fs::create_dir_all(&class).unwrap();
            for (index, name) in [(0, "soc_thermal"), (1, "nvme")] {
                let other = class.join(format!("hwmon{index}"));
                fs::create_dir_all(&other).unwrap();
                fs::write(other.join("name"), format!("{name}\n")).unwrap();
            }
            let fan = paths
                .sys
                .join(format!("devices/platform/pwm-fan/hwmon/hwmon{hwmon}"));
            fs::create_dir_all(&fan).unwrap();
            fs::write(fan.join("name"), "pwmfan\n").unwrap();
            fs::write(fan.join("pwm1"), "50\n").unwrap();
            fs::write(fan.join("pwm1_enable"), "1\n").unwrap();
            symlink(&node, fan.join("of_node")).unwrap();
            symlink(&fan, class.join(format!("hwmon{hwmon}"))).unwrap();

            let thermal = paths.sys.join("class/thermal");
            for (index, kind, temp) in [
                (zone + 1, "bigcore0-thermal", "61000"),
                (zone, "soc-thermal", "52400"),
                (zone + 2, "gpu-thermal", "40000"),
            ] {
                let path = thermal.join(format!("thermal_zone{index}"));
                fs::create_dir_all(&path).unwrap();
                fs::write(path.join("type"), format!("{kind}\n")).unwrap();
                fs::write(path.join("temp"), format!("{temp}\n")).unwrap();
            }

            fs::create_dir_all(paths.model.parent().unwrap()).unwrap();
            fs::write(&paths.model, format!("{model}\0")).unwrap();
            fs::create_dir_all(paths.boot_id.parent().unwrap()).unwrap();
            fs::write(&paths.boot_id, "boot-one\n").unwrap();
            fs::create_dir_all(&paths.overlay_dir).unwrap();
            fs::write(
                &paths.boot_env,
                format!("verbosity=1\nuser_overlays=mediabox-hdmi-any-vp {CURVE_OVERLAY}\n"),
            )
            .unwrap();
            Self { dir, paths }
        }

        fn plus() -> Self {
            Self::new("Orange Pi 5 Plus", 9, 0, &PLUS_PWMS_50HZ)
        }

        fn controller(&self) -> FanController {
            FanController::new(self.paths.clone())
        }

        fn node(&self) -> PathBuf {
            self.paths.sys.join("firmware/devicetree/base/pwm-fan")
        }

        /// As if the board had rebooted with `curve` applied by the boot
        /// loader.
        fn reboot_into(&self, curve: &FanCurve) {
            fs::write(
                self.node().join("cooling-levels"),
                be_cells(&curve.cooling_levels()),
            )
            .unwrap();
            let trips: Vec<u32> = curve
                .temp_trips()
                .into_iter()
                .flat_map(|(t, s)| [t, s])
                .collect();
            fs::write(self.node().join("rockchip,temp-trips"), be_cells(&trips)).unwrap();
            fs::write(&self.paths.boot_id, "boot-two\n").unwrap();
        }
    }

    fn custom() -> FanCurve {
        FanCurve {
            profile: FanProfile::Custom,
            points: vec![
                FanPoint::new(45, 60),
                FanPoint::new(52, 90),
                FanPoint::new(58, 140),
                FanPoint::new(64, 200),
                FanPoint::new(72, 255),
            ],
        }
    }

    // ------------------------------------------------------------ decoding

    #[test]
    fn the_plus_carrier_is_20_khz_before_the_fix_and_50_hz_after() {
        assert_eq!(pwm_period_ns(&PLUS_PWMS_20KHZ), Some(50_000));
        assert_eq!(frequency_hz(50_000), Some(20_000.0));
        assert_eq!(pwm_period_ns(&PLUS_PWMS_50HZ), Some(20_000_000));
        assert_eq!(frequency_hz(20_000_000), Some(50.0));
        assert_eq!(pwm_period_ns(&[0, 0, 1]), None);
        assert_eq!(frequency_hz(0), None);
    }

    #[test]
    fn only_the_plus_takes_the_carrier_fix() {
        assert!(needs_plus_fix("Orange Pi 5 Plus"));
        assert!(needs_plus_fix("Orange Pi 5 Plus\0"));
        assert!(needs_plus_fix("Xunlong Orange Pi 5 Plus"));
        assert!(!needs_plus_fix("Orange Pi 5 Ultra"));
        assert!(!needs_plus_fix("Orange Pi 5"));
        assert!(!needs_plus_fix(""));
    }

    // ----------------------------------------------------------- discovery

    /// hwmon9 and thermal_zone0 on the Plus; other numbers elsewhere. The same
    /// answer either way.
    #[test]
    fn discovery_does_not_depend_on_the_kernels_numbering() {
        for (hwmon, zone) in [(9, 0), (3, 4), (12, 7)] {
            let board = Board::new("Orange Pi 5 Plus", hwmon, zone, &PLUS_PWMS_50HZ);
            let status = board.controller().status();
            assert!(status.available, "hwmon{hwmon}");
            assert_eq!(status.temperature_c, Some(52.4), "thermal_zone{zone}");
            assert_eq!(status.pwm, Some(50));
            assert_eq!(status.pwm_percent, Some(19.6));
            assert_eq!(status.pwm_enable, Some(1));
            assert_eq!(status.pwm_period_ns, Some(20_000_000));
            assert_eq!(status.pwm_frequency_hz, Some(50.0));
            assert_eq!(status.control_backend, CONTROL_BACKEND);
            assert_eq!(status.curve, Some(FanCurve::board()));
            assert_eq!(status.board.as_deref(), Some("Orange Pi 5 Plus"));
        }
    }

    /// No tachometer, no speed -- and nothing that pretends to be one.
    #[test]
    fn a_fan_with_no_tachometer_reports_no_rpm() {
        let board = Board::plus();
        let status = board.controller().status();
        assert!(!status.rpm_available);
        assert_eq!(status.rpm, None);

        let hwmon = discover(&board.paths.sys).unwrap().hwmon;
        fs::write(hwmon.join("fan1_input"), "1830\n").unwrap();
        let status = board.controller().status();
        assert!(status.rpm_available);
        assert_eq!(status.rpm, Some(1830));
    }

    #[test]
    fn the_node_is_found_through_of_node() {
        let board = Board::plus();
        assert_eq!(
            discover(&board.paths.sys).unwrap().node.as_deref(),
            Some("/pwm-fan")
        );
    }

    #[test]
    fn a_board_with_no_fan_says_so_and_saves_nothing() {
        let board = Board::plus();
        let hwmon = discover(&board.paths.sys).unwrap().hwmon;
        fs::write(hwmon.join("name"), "something-else\n").unwrap();
        let controller = board.controller();
        let status = controller.status();
        assert!(!status.available);
        assert!(status.error.is_some());
        assert_eq!(
            controller.set(FanCurve::balanced()).unwrap_err().code(),
            "FAN_UNAVAILABLE"
        );
        assert!(!board.paths.overlay_file().exists());
    }

    // ----------------------------------------------------------- the quirk

    #[test]
    fn the_plus_fix_is_reported_by_what_the_tree_runs() {
        let fixed = Board::new("Orange Pi 5 Plus", 9, 0, &PLUS_PWMS_50HZ);
        assert_eq!(fixed.controller().status().board_fix, FanBoardFix::Active);
        assert_eq!(fixed.controller().status().warning, None);

        let unfixed = Board::new("Orange Pi 5 Plus", 9, 0, &PLUS_PWMS_20KHZ);
        let status = unfixed.controller().status();
        assert_eq!(status.board_fix, FanBoardFix::Missing);
        assert_eq!(status.pwm_frequency_hz, Some(20_000.0));
        assert!(status.warning.unwrap().contains(PLUS_FIX_OVERLAY));

        // The Ultra runs 50 Hz of its own accord and needs nothing.
        let ultra = Board::new("Orange Pi 5 Ultra", 4, 2, &PLUS_PWMS_50HZ);
        assert_eq!(
            ultra.controller().status().board_fix,
            FanBoardFix::NotNeeded
        );
        // And an unknown board is left as it is.
        let other = Board::new("Some Other Board", 5, 1, &PLUS_PWMS_20KHZ);
        assert_eq!(
            other.controller().status().board_fix,
            FanBoardFix::NotNeeded
        );
    }

    // ------------------------------------------------------ saving a curve

    #[test]
    fn a_saved_curve_is_pending_until_the_next_boot_and_then_running() {
        let board = Board::plus();
        let controller = board.controller();
        let curve = custom();
        let status = controller.set(curve.clone()).unwrap();
        assert!(status.pending_reboot);
        assert_eq!(status.configured, Some(curve.clone()));
        // The running tree has not changed.
        assert_eq!(status.curve, Some(FanCurve::board()));
        let written = fs::read(board.paths.overlay_file()).unwrap();
        assert_eq!(written, curve_overlay("/pwm-fan", &curve));
        assert_eq!(
            fs::metadata(board.paths.overlay_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );

        board.reboot_into(&curve);
        let status = controller.status();
        assert!(!status.pending_reboot);
        assert_eq!(status.curve, Some(curve.clone()));
        assert_eq!(status.warning, None);
    }

    /// Saved, rebooted, and the boot loader did not apply it.
    #[test]
    fn a_curve_the_boot_did_not_apply_is_a_warning() {
        let board = Board::plus();
        let controller = board.controller();
        controller.set(custom()).unwrap();
        fs::write(&board.paths.boot_id, "boot-two\n").unwrap();
        let status = controller.status();
        assert!(!status.pending_reboot);
        assert!(status.warning.unwrap().contains("etkin değil"));
    }

    #[test]
    fn saving_the_same_curve_twice_writes_nothing_new() {
        let board = Board::plus();
        let controller = board.controller();
        let curve = FanCurve::preset(FanProfile::Cool).unwrap();
        controller.set(curve.clone()).unwrap();
        board.reboot_into(&curve);
        let status = controller.set(curve).unwrap();
        // Already running: saving it again must not ask for a reboot.
        assert!(!status.pending_reboot);
        let state = read_state(&board.paths.state).unwrap();
        assert_eq!(state.boot_id.as_deref(), Some("boot-one"));
    }

    #[test]
    fn an_unsafe_curve_is_refused_and_nothing_is_written() {
        let board = Board::plus();
        let controller = board.controller();
        let mut curve = custom();
        curve.points[4] = FanPoint::new(95, 20);
        let error = controller.set(curve).unwrap_err();
        assert_eq!(error.code(), "FAN_CURVE_INVALID");
        assert!(!board.paths.overlay_file().exists());
        assert!(!board.paths.state.exists());
    }

    /// A curve the boot loader would never load is not reported as saved.
    #[test]
    fn a_boot_that_does_not_load_the_curve_refuses_to_save_it() {
        let board = Board::plus();
        fs::write(
            &board.paths.boot_env,
            "user_overlays=mediabox-hdmi-any-vp\n",
        )
        .unwrap();
        let controller = board.controller();
        assert!(!controller.status().boot_config_ready);
        assert_eq!(
            controller.set(custom()).unwrap_err().code(),
            "FAN_BOOT_CONFIG"
        );
        assert!(!board.paths.overlay_file().exists());
    }

    /// The failed write leaves the previous overlay exactly as it was.
    #[test]
    fn a_failed_write_keeps_the_previous_curve() {
        let board = Board::plus();
        let controller = board.controller();
        controller.set(custom()).unwrap();
        let before = fs::read(board.paths.overlay_file()).unwrap();
        // A directory where the temporary file would go makes the write fail
        // after validation and before the rename.
        fs::create_dir(
            board
                .paths
                .overlay_dir
                .join(format!(".{CURVE_OVERLAY}.dtbo.next")),
        )
        .unwrap();
        let error = controller
            .set(FanCurve::preset(FanProfile::Cool).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), "FAN_WRITE_ERROR");
        assert_eq!(fs::read(board.paths.overlay_file()).unwrap(), before);
        assert_eq!(controller.status().configured, Some(custom()));
    }

    // ------------------------------------------------------------ resetting

    /// Resetting is saving the product default: the curve overlay is
    /// rewritten, the board's carrier fix and every other overlay and the
    /// boot configuration are not touched.
    #[test]
    fn reset_saves_the_product_default_and_nothing_else() {
        let board = Board::plus();
        let fix = board
            .paths
            .overlay_dir
            .join(format!("{PLUS_FIX_OVERLAY}.dtbo"));
        let hdmi = board.paths.overlay_dir.join("mediabox-hdmi-any-vp.dtbo");
        fs::write(&fix, b"fix").unwrap();
        fs::write(&hdmi, b"hdmi").unwrap();
        let env_before = fs::read(&board.paths.boot_env).unwrap();

        let controller = board.controller();
        controller.set(custom()).unwrap();
        let status = controller.reset().unwrap();
        assert_eq!(status.configured, Some(FanCurve::product_default()));
        assert!(status.pending_reboot);
        assert!(board.paths.overlay_file().exists());
        assert_eq!(fs::read(&fix).unwrap(), b"fix");
        assert_eq!(fs::read(&hdmi).unwrap(), b"hdmi");
        assert_eq!(fs::read(&board.paths.boot_env).unwrap(), env_before);

        board.reboot_into(&FanCurve::product_default());
        let status = controller.status();
        assert!(!status.pending_reboot);
        assert_eq!(status.curve, Some(FanCurve::product_default()));
        assert_eq!(status.board_fix, FanBoardFix::Active);
    }

    /// Two boards installed clean, with nothing saved on either, get the same
    /// curve, byte for byte, and it is this product's, not the vendor's.
    #[test]
    fn a_clean_plus_and_a_clean_ultra_start_from_the_same_default() {
        let plus = Board::plus();
        let ultra = Board::new("RK3588 OPi 5 Ultra", 3, 1, &PLUS_PWMS_50HZ);
        for board in [&plus, &ultra] {
            let controller = board.controller();
            assert_eq!(controller.status().curve, Some(FanCurve::board()));
            let status = controller.seed_default().unwrap().unwrap();
            assert_eq!(status.configured, Some(FanCurve::product_default()));
            assert!(status.pending_reboot);
            board.reboot_into(&FanCurve::product_default());
            let status = controller.status();
            assert_eq!(status.curve, Some(FanCurve::product_default()));
            assert!(!status.pending_reboot);
            assert_eq!(status.warning, None);
        }
        assert_eq!(
            fs::read(plus.paths.overlay_file()).unwrap(),
            fs::read(ultra.paths.overlay_file()).unwrap()
        );
    }

    /// A curve somebody saved is theirs: seeding the default never replaces
    /// it, and it survives the reboot it was saved for.
    #[test]
    fn the_default_never_replaces_a_saved_curve() {
        let board = Board::plus();
        let controller = board.controller();
        controller.set(custom()).unwrap();
        let before = fs::read(board.paths.overlay_file()).unwrap();
        assert!(controller.seed_default().unwrap().is_none());
        assert_eq!(fs::read(board.paths.overlay_file()).unwrap(), before);
        board.reboot_into(&custom());
        assert!(controller.seed_default().unwrap().is_none());
        assert_eq!(controller.status().curve, Some(custom()));
    }

    /// A board whose boot configuration does not load the curve is left
    /// alone: the default is not a reason to touch /boot.
    #[test]
    fn no_default_without_the_boot_entry() {
        let board = Board::plus();
        fs::write(
            &board.paths.boot_env,
            "user_overlays=mediabox-hdmi-any-vp\n",
        )
        .unwrap();
        assert_eq!(
            board.controller().seed_default().unwrap_err().code(),
            "FAN_BOOT_CONFIG"
        );
        assert!(!board.paths.overlay_file().exists());
        assert!(!board.paths.state.exists());
    }

    // ------------------------------------------------------------ the blob

    /// Reads a blob this module wrote back into (node path, property, value)
    /// triples, so the layout is checked without any tool installed.
    fn walk(blob: &[u8]) -> Vec<(String, String, Vec<u8>)> {
        let word = |at: usize| u32::from_be_bytes(blob[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), FDT_MAGIC);
        assert_eq!(word(4) as usize, blob.len());
        let (structure, strings) = (word(8) as usize, word(12) as usize);
        let name_at = |offset: usize| {
            let start = strings + offset;
            let end = blob[start..].iter().position(|b| *b == 0).unwrap() + start;
            String::from_utf8(blob[start..end].to_vec()).unwrap()
        };
        let mut found = Vec::new();
        let mut path: Vec<String> = Vec::new();
        let mut at = structure;
        loop {
            match word(at) {
                FDT_BEGIN_NODE => {
                    let end = blob[at + 4..].iter().position(|b| *b == 0).unwrap() + at + 4;
                    path.push(String::from_utf8(blob[at + 4..end].to_vec()).unwrap());
                    at = (end + 1).next_multiple_of(4);
                }
                FDT_END_NODE => {
                    path.pop();
                    at += 4;
                }
                FDT_PROP => {
                    let len = word(at + 4) as usize;
                    let name = name_at(word(at + 8) as usize);
                    let value = blob[at + 12..at + 12 + len].to_vec();
                    found.push((path.join("/"), name, value));
                    at = (at + 12 + len).next_multiple_of(4);
                }
                FDT_END => break,
                other => panic!("unexpected token {other}"),
            }
        }
        assert!(path.is_empty());
        found
    }

    #[test]
    fn the_overlay_carries_the_curve_and_nothing_else() {
        let curve = custom();
        let blob = curve_overlay("/pwm-fan", &curve);
        // Deterministic.
        assert_eq!(blob, curve_overlay("/pwm-fan", &curve));
        let props = walk(&blob);
        assert_eq!(
            props,
            vec![
                (
                    "/fragment@0".into(),
                    "target-path".into(),
                    b"/pwm-fan\0".to_vec()
                ),
                (
                    "/fragment@0/__overlay__".into(),
                    "cooling-levels".into(),
                    be_cells(&curve.cooling_levels())
                ),
                (
                    "/fragment@0/__overlay__".into(),
                    "rockchip,temp-trips".into(),
                    be_cells(
                        &curve
                            .temp_trips()
                            .into_iter()
                            .flat_map(|(t, s)| [t, s])
                            .collect::<Vec<_>>()
                    )
                ),
            ]
        );
    }

    /// A curve of any length: seven points from 0 °C, given to the kernel as
    /// its line, one trip per degree from where the fan first runs.
    #[test]
    fn an_arbitrary_curve_becomes_its_line_one_degree_at_a_time() {
        let curve = FanCurve {
            profile: FanProfile::Custom,
            points: [
                (0, 0),
                (30, 0),
                (40, 50),
                (48, 80),
                (55, 120),
                (63, 180),
                (72, 255),
            ]
            .iter()
            .map(|&(t, p)| FanPoint::new(t, p))
            .collect(),
        };
        let props = walk(&curve_overlay("/pwm-fan", &curve));
        let levels = curve.cooling_levels();
        // Off, then 50 at 40 °C, then the line up to full at 72 °C.
        assert_eq!(levels[..2], [0, 50]);
        assert_eq!(levels.last(), Some(&255));
        assert_eq!(levels.len(), 1 + (72 - 40 + 1));
        assert_eq!(props[1].2, be_cells(&levels));
        let trips = curve.temp_trips();
        assert_eq!(trips[0], (40000, 1));
        assert_eq!(trips.last(), Some(&(72000, 33)));
        // And the tree reads back as the same fan.
        let read = FanCurve::from_device_tree(&levels, &trips).unwrap();
        assert!(read.same_kernel(&curve));
    }

    #[test]
    fn diagnostics_are_the_same_reading() {
        let board = Board::plus();
        let status = board.controller().status();
        let section = diagnostics(&status);
        assert_eq!(section["pwm"], 50);
        assert_eq!(section["pwmPercent"], 19.6);
        assert_eq!(section["periodNs"], 20_000_000);
        assert_eq!(section["frequencyHz"], 50.0);
        assert_eq!(section["rpmAvailable"], false);
        assert_eq!(section["profile"], "board");
        drop(board.dir);
    }
}
