//! A board, built out of directories, so that discovery can be asked about
//! hardware that is not here.
//!
//! Every fixture below is a shape of RK3588 this product either runs on or will
//! have to: one HDMI transmitter, three display outputs, a DRM device that is
//! not `card0`, sound cards that came up in a different order. The point of the
//! module is that the *same* discovery code answers all of them, because none
//! of the answers is written down anywhere.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

pub struct Board {
    pub dir: TempDir,
}

impl Board {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("temporary directory");
        for path in [
            "sys/class/drm",
            "sys/class/sound",
            "sys/devices/platform",
            "dev/dri",
            "var/lib/mediabox",
        ] {
            fs::create_dir_all(dir.path().join(path)).unwrap();
        }
        fs::create_dir_all(dir.path().join("sys/firmware/devicetree/base/aliases")).unwrap();
        Self { dir }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    fn sys(&self, rest: &str) -> PathBuf {
        self.dir.path().join("sys").join(rest)
    }

    fn write(&self, path: PathBuf, contents: impl AsRef<[u8]>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A device-tree node with a phandle, at `<name>` under the tree root.
    pub fn dt_node(&self, name: &str, phandle: u32) -> &Self {
        let node = self.sys(&format!("firmware/devicetree/base/{name}"));
        fs::create_dir_all(&node).unwrap();
        fs::write(node.join("phandle"), phandle.to_be_bytes()).unwrap();
        fs::write(node.join("status"), b"okay\0").unwrap();
        self
    }

    pub fn dt_alias(&self, alias: &str, target: &str) -> &Self {
        self.write(
            self.sys(&format!("firmware/devicetree/base/aliases/{alias}")),
            format!("{target}\0"),
        );
        self
    }

    /// A platform device bound to `driver`, optionally tied to a device-tree
    /// node.
    pub fn platform_device(&self, name: &str, driver: &str, of_node: Option<&str>) -> &Self {
        let device = self.sys(&format!("devices/platform/{name}"));
        fs::create_dir_all(&device).unwrap();
        let drivers = self.sys(&format!("bus/platform/drivers/{driver}"));
        fs::create_dir_all(&drivers).unwrap();
        let _ = symlink(&drivers, device.join("driver"));
        if let Some(node) = of_node {
            let target = self.sys(&format!("firmware/devicetree/base/{node}"));
            fs::create_dir_all(&target).unwrap();
            let _ = symlink(&target, device.join("of_node"));
        }
        self
    }

    /// A DRM device exported by a platform device.
    pub fn drm_card(&self, card: &str, parent: &str) -> &Self {
        let dir = self.sys(&format!("devices/platform/{parent}/drm/{card}"));
        fs::create_dir_all(&dir).unwrap();
        let _ = symlink(
            self.sys(&format!("devices/platform/{parent}")),
            dir.join("device"),
        );
        let _ = symlink(&dir, self.sys(&format!("class/drm/{card}")));
        fs::create_dir_all(self.dir.path().join("dev/dri")).unwrap();
        self.write(self.dir.path().join(format!("dev/dri/{card}")), b"");
        self
    }

    /// A connector on a DRM device. `modes` empty means the sink offered none.
    pub fn connector(
        &self,
        card: &str,
        parent: &str,
        name: &str,
        connected: bool,
        modes: &[&str],
        edid: &[u8],
    ) -> &Self {
        let full = format!("{card}-{name}");
        let dir = self.sys(&format!("devices/platform/{parent}/drm/{card}/{full}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("status"),
            if connected {
                "connected\n"
            } else {
                "disconnected\n"
            },
        )
        .unwrap();
        fs::write(
            dir.join("enabled"),
            if connected { "enabled\n" } else { "disabled\n" },
        )
        .unwrap();
        fs::write(dir.join("modes"), modes.join("\n")).unwrap();
        fs::write(dir.join("edid"), edid).unwrap();
        let _ = symlink(&dir, self.sys(&format!("class/drm/{full}")));
        self
    }

    /// An HDMI or DisplayPort transmitter: a platform device on a `hdmi@`/`dp@`
    /// node, with the hotplug state the driver publishes.
    pub fn transmitter(
        &self,
        device: &str,
        of_node: &str,
        phandle: u32,
        cable: Option<bool>,
    ) -> &Self {
        self.dt_node(of_node, phandle);
        self.platform_device(device, "display-transmitter", Some(of_node));
        if let Some(present) = cable {
            let kind = if of_node.starts_with("dp@") {
                "DP"
            } else {
                "HDMI"
            };
            let extcon = self.sys(&format!("devices/platform/{device}/extcon/extcon0"));
            fs::create_dir_all(&extcon).unwrap();
            fs::write(extcon.join("name"), format!("{device}\n")).unwrap();
            fs::write(
                extcon.join("state"),
                format!("{kind}={}\n", u8::from(present)),
            )
            .unwrap();
        }
        self
    }

    pub fn cec(&self, transmitter: &str, name: &str) -> &Self {
        fs::create_dir_all(self.sys(&format!("devices/platform/{transmitter}/{name}"))).unwrap();
        self.write(self.dir.path().join(format!("dev/{name}")), b"");
        self
    }

    /// A sound card whose device-tree node names `codec_phandle` as its codec.
    pub fn sound_card(
        &self,
        index: u32,
        id: &str,
        of_node: &str,
        codec_phandle: Option<u32>,
    ) -> &Self {
        self.dt_node(of_node, 0x8000 + index);
        // What ALSA reports as the card's driver, and what alsa-lib keys
        // `/usr/share/alsa/cards/<driver>.conf` off.
        let driver = of_node
            .trim_end_matches("-sound")
            .replace("hdmi", "rockchip-hdmi")
            .replace("dp", "rockchip-dp")
            .replace("es8388", "rockchip-es8388");
        self.write(
            self.sys(&format!(
                "firmware/devicetree/base/{of_node}/rockchip,card-name"
            )),
            format!("{driver}\0"),
        );
        if let Some(codec) = codec_phandle {
            self.write(
                self.sys(&format!(
                    "firmware/devicetree/base/{of_node}/rockchip,codec"
                )),
                codec.to_be_bytes(),
            );
        }
        let platform = of_node.to_string();
        self.platform_device(&platform, "rk-hdmi-sound", Some(of_node));
        let card = self.sys(&format!("devices/platform/{platform}/sound/card{index}"));
        fs::create_dir_all(card.join("pcmC0D0p")).unwrap();
        fs::write(card.join("id"), format!("{id}\n")).unwrap();
        fs::write(card.join("number"), format!("{index}\n")).unwrap();
        let _ = symlink(
            self.sys(&format!("devices/platform/{platform}")),
            card.join("device"),
        );
        let _ = symlink(&card, self.sys(&format!("class/sound/card{index}")));
        self
    }

    /// What a person chose, remembered across boots.
    pub fn remember_output(&self, selector: &str) -> &Self {
        self.write(
            self.dir.path().join("var/lib/mediabox/display-output"),
            format!("{selector}\n"),
        );
        self
    }
}

/// A minimal but genuine EDID base block, so the sink-identity path is
/// exercised against real bytes rather than against a mock.
pub fn edid(manufacturer: [u8; 3], product: u16, serial: u32, name: &str) -> Vec<u8> {
    let mut block = vec![0u8; 128];
    block[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    let letter = |c: u8| u16::from(c - b'A' + 1) & 0x1F;
    let packed =
        (letter(manufacturer[0]) << 10) | (letter(manufacturer[1]) << 5) | letter(manufacturer[2]);
    block[8..10].copy_from_slice(&packed.to_be_bytes());
    block[10..12].copy_from_slice(&product.to_le_bytes());
    block[12..16].copy_from_slice(&serial.to_le_bytes());
    // Descriptor 0: the monitor name.
    let at = 54;
    block[at + 3] = 0xFC;
    for (i, byte) in name.bytes().take(13).enumerate() {
        block[at + 5 + i] = byte;
    }
    if name.len() < 13 {
        block[at + 5 + name.len()] = 0x0A;
    }
    // And the checksum a real one carries: a base block that does not sum to
    // zero is not an EDID, and the parser says so.
    let sum = block[..127].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    block[127] = 0u8.wrapping_sub(sum);
    block
}

/// The same base block with a CTA-861 extension whose HDMI VSDB gives this
/// input `physical_address` -- which is what the transmitter's CEC adapter is
/// given when this sink is on its socket.
pub fn edid_with_address(
    manufacturer: [u8; 3],
    product: u16,
    serial: u32,
    name: &str,
    physical_address: u16,
) -> Vec<u8> {
    let mut bytes = edid(manufacturer, product, serial, name);
    bytes[126] = 1;
    let sum = bytes[..127].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    bytes[127] = 0u8.wrapping_sub(sum);
    let mut cta = vec![0u8; 128];
    cta[0] = 0x02;
    cta[1] = 0x03;
    let [a, b] = physical_address.to_be_bytes();
    cta[4..12].copy_from_slice(&[0x67, 0x03, 0x0C, 0x00, a, b, 0x00, 0x3C]);
    cta[2] = 12;
    let sum = cta[..127].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    cta[127] = 0u8.wrapping_sub(sum);
    bytes.extend(cta);
    bytes
}

impl Board {
    /// `/sys/class/drm/<card>/dev`: the device's major:minor.
    pub fn drm_dev(&self, card: &str, numbers: &str) -> &Self {
        let path = self.sys(&format!("class/drm/{card}"));
        let target = fs::canonicalize(&path).unwrap_or(path);
        fs::write(target.join("dev"), format!("{numbers}\n")).unwrap();
        self
    }

    /// `/sys/kernel/debug/dri/<minor>`, with its `name` and `summary`.
    pub fn dri_debugfs(&self, minor: u32, name: &str, summary: Option<&str>) -> &Self {
        let dir = self.sys(&format!("kernel/debug/dri/{minor}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("name"), format!("{name}\n")).unwrap();
        if let Some(summary) = summary {
            fs::write(dir.join("summary"), summary).unwrap();
        }
        self
    }

    /// A CEC adapter's debugfs status: its physical address, `f.f.f.f` for
    /// none.
    pub fn cec_status(&self, adapter: &str, physical_address: &str) -> &Self {
        let dir = self.sys(&format!("kernel/debug/cec/{adapter}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("status"),
            format!("enabled: 1\nconfigured: 1\nphys_addr: {physical_address}\n"),
        )
        .unwrap();
        self
    }

    /// The PHY a transmitter consumes, as the device link, the PHY's
    /// device-tree clock name and the clock framework's debugfs publish it.
    pub fn phy(
        &self,
        transmitter: &str,
        phy_device: &str,
        clock: &str,
        rate_hz: u64,
        enabled: bool,
    ) -> &Self {
        let phy_name = format!("phy-{phy_device}.0");
        let phy_dir = self.sys(&format!("devices/platform/{phy_device}/phy/{phy_name}"));
        fs::create_dir_all(&phy_dir).unwrap();
        let node = self.sys(&format!("firmware/devicetree/base/hdmiphy@{phy_device}"));
        fs::create_dir_all(&node).unwrap();
        fs::write(node.join("clock-output-names"), format!("{clock}\0")).unwrap();
        let _ = symlink(&node, self.sys(&format!("devices/platform/{phy_device}/of_node")));
        let link = self.sys(&format!("devices/virtual/devlink/phy:{phy_name}--platform:{transmitter}"));
        fs::create_dir_all(&link).unwrap();
        let _ = symlink(&phy_dir, link.join("supplier"));
        let _ = symlink(
            &link,
            self.sys(&format!("devices/platform/{transmitter}/supplier:phy:{phy_name}")),
        );
        let clk = self.sys(&format!("kernel/debug/clk/{clock}"));
        fs::create_dir_all(&clk).unwrap();
        fs::write(clk.join("clk_rate"), format!("{rate_hz}\n")).unwrap();
        fs::write(clk.join("clk_enable_count"), if enabled { "2\n" } else { "0\n" }).unwrap();
        self
    }
}

/// One HDMI transmitter, one connector, one sound card, one CEC adapter.
///
/// The board this product was written on. `HDMI-A-1` is the only output there
/// is and its sound card is called `rockchiphdmi1` — the *1* comes from the
/// transmitter's device-tree alias, not from the connector's number, which is
/// exactly the trap the other fixture springs.
pub fn one_hdmi() -> Board {
    let board = Board::new();
    board.platform_device("display-subsystem", "rockchip-drm", None);
    board.platform_device("fdab0000.npu", "RKNPU", None);
    board.drm_card("card0", "display-subsystem");
    board.drm_card("card1", "fdab0000.npu");
    board.connector(
        "card0",
        "display-subsystem",
        "HDMI-A-1",
        true,
        &["3840x2160", "1920x1080"],
        &edid(*b"SNY", 0x0101, 0x0000_2A2A, "BRAVIA"),
    );
    board.connector("card0", "display-subsystem", "Writeback-1", false, &[], &[]);
    board.render_node("renderD128", "display-subsystem");
    board.render_node("renderD129", "fdab0000.npu");
    board.transmitter("fdea0000.hdmi", "hdmi@fdea0000", 0x100, Some(true));
    board.dt_alias("hdmi1", "/hdmi@fdea0000");
    board.cec("fdea0000.hdmi", "cec0");
    board.sound_card(0, "rockchiphdmi1", "hdmi1-sound", Some(0x100));
    board.sound_card(2, "rockchipes8388", "es8388-sound", None);
    board
}

/// Two HDMI transmitters and DisplayPort, with the sound cards the other way up.
///
/// `HDMI-A-1` here is the transmitter the other board calls `hdmi0`, so its
/// sound card is `rockchiphdmi0`. Any product that remembers "the HDMI card is
/// rockchiphdmi1" is on the wrong television on this board — which is the whole
/// reason the audio endpoint is derived from the selected output.
pub fn three_outputs(connected: &[&str]) -> Board {
    let board = Board::new();
    board.platform_device("display-subsystem", "rockchip-drm", None);
    board.platform_device("fdab0000.npu", "RKNPU", None);
    board.drm_card("card0", "display-subsystem");
    board.drm_card("card1", "fdab0000.npu");
    board.render_node("renderD128", "display-subsystem");
    board.render_node("renderD129", "fdab0000.npu");
    for (name, sink) in [
        ("HDMI-A-1", *b"SNY"),
        ("HDMI-A-2", *b"LGD"),
        ("DP-1", *b"DEL"),
    ] {
        let live = connected.contains(&name);
        let block = if live {
            edid(sink, 0x0202, 0x0000_3B3B, name)
        } else {
            Vec::new()
        };
        let modes: &[&str] = if live {
            &["3840x2160", "1920x1080"]
        } else {
            &[]
        };
        board.connector("card0", "display-subsystem", name, live, modes, &block);
    }
    board.connector("card0", "display-subsystem", "Writeback-1", false, &[], &[]);
    board.transmitter(
        "fde80000.hdmi",
        "hdmi@fde80000",
        0x100,
        Some(connected.contains(&"HDMI-A-1")),
    );
    board.transmitter(
        "fdea0000.hdmi",
        "hdmi@fdea0000",
        0x101,
        Some(connected.contains(&"HDMI-A-2")),
    );
    board.transmitter(
        "fde50000.dp",
        "dp@fde50000",
        0x102,
        Some(connected.contains(&"DP-1")),
    );
    board.dt_alias("hdmi0", "/hdmi@fde80000");
    board.dt_alias("hdmi1", "/hdmi@fdea0000");
    board.dt_alias("dp0", "/dp@fde50000");
    board.cec("fde80000.hdmi", "cec0");
    board.cec("fdea0000.hdmi", "cec1");
    // DisplayPort has no CEC adapter, which is the point of the fixture.
    board.sound_card(0, "rockchipdp0", "dp0-sound", Some(0x102));
    board.sound_card(1, "rockchiphdmi0", "hdmi0-sound", Some(0x100));
    board.sound_card(2, "rockchiphdmi1", "hdmi1-sound", Some(0x101));
    board.sound_card(4, "rockchipes8388", "es8388-sound", None);
    board
}

impl Board {
    pub fn render_node(&self, name: &str, parent: &str) -> &Self {
        let dir = self.sys(&format!("devices/platform/{parent}/drm/{name}"));
        fs::create_dir_all(&dir).unwrap();
        let _ = symlink(
            self.sys(&format!("devices/platform/{parent}")),
            dir.join("device"),
        );
        let _ = symlink(&dir, self.sys(&format!("class/drm/{name}")));
        self.write(self.dir.path().join(format!("dev/dri/{name}")), b"");
        self
    }
}
