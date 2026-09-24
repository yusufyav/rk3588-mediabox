//! What this appliance's display hardware actually is, asked of the hardware.
//!
//! The product runs on more than one RK3588 board. One of them has a single
//! HDMI transmitter; another has two, plus DisplayPort. Their sound cards are
//! numbered differently, their CEC adapters are numbered differently, and which
//! DRM device is the display and which is the NPU is not the same on both. None
//! of that is exceptional and none of it is a reason to branch on a board name:
//! it is topology, and topology is readable.
//!
//! So there is one resolver, here, and everything else asks it:
//!
//! ```text
//!         capabilities / topology
//!                    |
//!            platform discovery
//!            /       |       \
//!         DRM      ALSA      CEC
//!            \       |       /
//!             selected output
//! ```
//!
//! The order matters. The display stage chooses an output; the audio endpoint
//! and the CEC adapter are then *derived from that choice*, not chosen
//! separately. Choosing them separately is how a box ends up drawing on one
//! television and talking to another.
//!
//! Overrides exist for every stage and are meant for diagnostics. Auto is the
//! product's answer.

pub mod audio;
mod cta_vics;
pub mod cec;
pub mod debugfs;
pub mod drm;
pub mod dt;
pub mod drm_query;
pub mod edid;
pub mod roots;
pub mod source;
pub mod topology;
pub mod observer;
pub mod output;
pub mod video;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use audio::AudioEndpoint;
pub use cec::CecAdapter;
pub use drm::{Connector, ConnectorKind, Controller, DrmNode, SinkIdentity};
pub use roots::Roots;
pub use video::{ColorFormat, ColorMode, SinkVideo, parse_sink_video};

/// The file, inside the daemon's state directory, that remembers which output
/// a person chose. One line: a connector name, a sink name, or `auto`.
pub const OUTPUT_SELECTION_FILE: &str = "display-output";

/// Manual answers to the three questions discovery asks, for the times when a
/// person needs to force one.
///
/// These are diagnostics and are read from the environment only. Nothing in the
/// product sets them: an appliance that needs `MEDIABOX_KMS_NODE` to come up is
/// an appliance whose discovery is broken, and burying that in a unit file is
/// how it stays broken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub kms: Option<String>,
    pub render: Option<String>,
    pub output: Option<String>,
}

impl Overrides {
    pub fn from_env() -> Self {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            kms: read("MEDIABOX_KMS_NODE"),
            render: read("MEDIABOX_RENDER_NODE"),
            output: read("MEDIABOX_OUTPUT"),
        }
    }
}

/// How firmly a link in the topology is known.
///
/// Ordered from strongest to weakest; a chain is as strong as its weakest
/// link ([`Confidence::and`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// A structural fact the kernel publishes: the CEC adapter is a child
    /// device of the transmitter; the sound card names the transmitter as its
    /// codec in the device tree. Nothing about a connector is ever this: the
    /// kernel publishes no link from a DRM connector to its transmitter.
    Exact,
    /// Measured on this boot, from independent places, and unambiguous: the
    /// sink's EDID physical address is the one the transmitter's CEC adapter
    /// was given, or the only connected connector of its kind meets the only
    /// transmitter with a cable, a running PHY clock, and connector order
    /// agrees.
    Measured,
    /// Worked out without anything to measure: the Nth connector of a kind is
    /// the Nth transmitter of that kind by address. Nothing observed
    /// disagrees; nothing observed confirms.
    Derived,
    /// More than one reading fits, or the readings disagree. Reported rather
    /// than resolved: a wrong answer sends audio or a CEC command to a
    /// different television than the picture.
    Ambiguous,
    /// There is nothing to link to.
    Unavailable,
}

impl Confidence {
    /// The weaker of two links.
    pub fn and(self, other: Confidence) -> Confidence {
        self.max(other)
    }

    /// Firm enough to act on -- to send a CEC command down.
    pub fn actionable(self) -> bool {
        matches!(
            self,
            Confidence::Exact | Confidence::Measured | Confidence::Derived
        )
    }
}

/// The name this used to have. Same type.
pub type Binding = Confidence;

/// A display output, with everything that belongs to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Output {
    pub connector: Connector,
    /// The transmitter driving it, if one could be identified. Under an
    /// ambiguous binding this is the best candidate, kept so that what
    /// follows the transmitter structurally (the sound card) behaves as it
    /// did; anything that must not act on a guess -- CEC -- checks
    /// [`Output::binding`] first.
    pub controller: Option<String>,
    /// Connector to transmitter.
    pub binding: Confidence,
    /// What was measured, in words, for whoever reads the report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// The sound card that carries this output's audio.
    pub audio: Option<AudioEndpoint>,
    /// The CEC adapter on this output. `None` on DisplayPort, which has none.
    pub cec: Option<CecAdapter>,
}

impl Output {
    /// A selector that names this output and survives a reboot.
    pub fn selector(&self) -> String {
        self.connector.name.clone()
    }

    /// Connector to transmitter to CEC adapter. The second link is
    /// structural (the adapter is the transmitter's child device).
    pub fn cec_confidence(&self) -> Confidence {
        match &self.cec {
            Some(_) => self.binding.and(Confidence::Exact),
            None => Confidence::Unavailable,
        }
    }

    /// Connector to transmitter to sound card. The second link is structural
    /// (the card's device-tree codec is the transmitter).
    pub fn audio_confidence(&self) -> Confidence {
        match &self.audio {
            Some(_) => self.binding.and(Confidence::Exact),
            None => Confidence::Unavailable,
        }
    }
}

/// Why the output that was chosen was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionReason {
    /// `MEDIABOX_OUTPUT` named it.
    Override,
    /// A remembered choice named it and it is still there.
    Configured,
    /// It is the only connected output.
    OnlyConnected,
    /// Several were connected and the policy below picked this one.
    Policy,
    /// A remembered or overridden choice named an output that is not connected,
    /// so the policy chose instead. Reported, because it is the case where what
    /// a person asked for is not what they got.
    ConfiguredUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    pub output: Output,
    pub reason: SelectionReason,
}

/// Everything discovery found, and what it decided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Platform {
    /// The DRM device that sets modes.
    pub kms: Option<DrmNode>,
    /// The DRM device that renders, on the same hardware as the one above.
    pub render: Option<DrmNode>,
    pub controllers: Vec<Controller>,
    /// What was measured about each transmitter, to tie connectors to them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measured: Vec<(String, topology::TransmitterEvidence)>,
    /// The display device's debugfs directory, found from the KMS device.
    #[serde(default = "debugfs_unknown")]
    pub debugfs: debugfs::Debugfs,
    /// Every output this board has, connected or not.
    pub outputs: Vec<Output>,
    /// The one the product should use, if there is one.
    pub selected: Option<Selection>,
    /// What could not be decided. Empty on a healthy board.
    pub warnings: Vec<String>,
}

impl Platform {
    /// Inspect the running machine.
    pub fn discover() -> Self {
        Self::inspect(&Roots::from_env(), &Overrides::from_env())
    }

    /// Inspect a machine, real or captured, with the overrides given.
    pub fn inspect(roots: &Roots, overrides: &Overrides) -> Self {
        let mut warnings = Vec::new();
        let dt = dt::DeviceTree::open(roots);
        if !dt.is_present() {
            warnings.push(
                "no device tree under sysfs; audio and CEC cannot be tied to an output".into(),
            );
        }

        let Some((kms, connectors)) = drm::find_kms(roots, overrides.kms.as_deref()) else {
            warnings.push("no DRM device with connectors: this machine cannot set a mode".into());
            return Self {
                kms: None,
                render: None,
                controllers: Vec::new(),
                measured: Vec::new(),
                debugfs: debugfs_unknown(),
                outputs: Vec::new(),
                selected: None,
                warnings,
            };
        };
        if connectors.is_empty() {
            // Only reachable through the override: discovery picks the device
            // that owns connectors, so a device with none was asked for by
            // name. Say which, because the symptom further down is "no
            // connected output" and that would point at the cable.
            warnings.push(format!(
                "MEDIABOX_KMS_NODE named {}, which owns no connectors and cannot set a mode",
                kms.name
            ));
        }
        let render = drm::find_render(roots, &kms, overrides.render.as_deref());
        if render.is_none() {
            warnings.push(format!(
                "no render node shares hardware with {}; rendering has nowhere to go",
                kms.name
            ));
        }

        let controllers = drm::controllers(roots, &dt);
        let audio = audio::endpoints(roots, &dt, &controllers);
        let cec = cec::adapters(roots, &controllers);

        // The debugfs directory of this KMS device, never `dri/0` by
        // assumption, and what can be measured to tie connectors to
        // transmitters. Both are optional: without them the binding is by
        // connector order and says so.
        let debugfs = debugfs::resolve(roots, &kms);
        let summary = debugfs.read("summary").known();
        let measured = topology::measure(roots, &controllers, &cec);
        let bound = topology::bind(
            &connectors,
            &controllers,
            &measured,
            summary.as_deref(),
            &mut warnings,
        );
        let outputs: Vec<Output> = bound
            .into_iter()
            .map(|topology::Bound { connector, controller, confidence: binding, evidence }| Output {
                audio: controller.as_ref().and_then(|device| {
                    audio
                        .iter()
                        .find(|endpoint| &endpoint.controller == device)
                        .cloned()
                }),
                cec: controller.as_ref().and_then(|device| {
                    cec.iter()
                        .find(|adapter| &adapter.controller == device)
                        .cloned()
                }),
                connector,
                controller,
                binding,
                evidence,
            })
            .collect();

        let selected = select(roots, overrides, &outputs, &mut warnings);
        Self {
            kms: Some(kms),
            render,
            controllers,
            measured,
            debugfs,
            outputs,
            selected,
            warnings,
        }
    }

    pub fn selected_output(&self) -> Option<&Output> {
        self.selected.as_ref().map(|selection| &selection.output)
    }
}

/// Which output the product should put itself on.
///
/// `auto` is the product's answer and the only one a person has to have. An
/// explicit choice is honoured while it is plugged in and reported — not
/// silently obeyed — when it is not.
fn select(
    roots: &Roots,
    overrides: &Overrides,
    outputs: &[Output],
    warnings: &mut Vec<String>,
) -> Option<Selection> {
    let usable: Vec<&Output> = outputs
        .iter()
        .filter(|output| output.connector.connected && output.connector.mode_count > 0)
        .collect();
    if usable.is_empty() {
        warnings.push("no connected output with a usable mode".into());
        return None;
    }

    let wanted = overrides
        .output
        .clone()
        .map(|value| (value, SelectionReason::Override))
        .or_else(|| {
            std::fs::read_to_string(roots.state(OUTPUT_SELECTION_FILE))
                .ok()
                .map(|value| (value, SelectionReason::Configured))
        })
        .map(|(value, reason)| (value.trim().to_string(), reason))
        .filter(|(value, _)| !value.is_empty() && !value.eq_ignore_ascii_case("auto"));

    if let Some((selector, reason)) = wanted {
        if let Some(output) = usable.iter().find(|output| matches(output, &selector)) {
            return Some(Selection {
                output: (*output).clone(),
                reason,
            });
        }
        warnings.push(format!(
            "the chosen output '{selector}' is not connected; using the connected one instead"
        ));
        return Some(Selection {
            output: policy(&usable).clone(),
            reason: SelectionReason::ConfiguredUnavailable,
        });
    }

    let reason = if usable.len() == 1 {
        SelectionReason::OnlyConnected
    } else {
        SelectionReason::Policy
    };
    Some(Selection {
        output: policy(&usable).clone(),
        reason,
    })
}

/// Does this selector name this output?
///
/// A connector name, a sink's own name, or a sink's manufacturer-and-product
/// code. The last two are what make "the television" keep meaning the same
/// television after it has been moved to the other socket.
fn matches(output: &Output, selector: &str) -> bool {
    if output.connector.name.eq_ignore_ascii_case(selector) {
        return true;
    }
    let Some(sink) = &output.connector.sink else {
        return false;
    };
    if sink
        .name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(selector))
    {
        return true;
    }
    let code = format!("{}{:04X}", sink.manufacturer, sink.product);
    let serialised = format!("{code}-{:08X}", sink.serial);
    code.eq_ignore_ascii_case(selector) || serialised.eq_ignore_ascii_case(selector)
}

/// The deterministic fallback, when several outputs are connected and nobody
/// has said which.
///
/// A television first: this is an appliance for a television, and HDMI is where
/// one is. Then the lowest-numbered connector of that kind, so two identical
/// boards with the same cabling come up the same way round.
fn policy<'a>(usable: &[&'a Output]) -> &'a Output {
    let rank = |output: &Output| match output.connector.kind {
        ConnectorKind::HdmiA => 0,
        ConnectorKind::DisplayPort => 1,
        ConnectorKind::EmbeddedDisplayPort => 2,
        ConnectorKind::Other => 3,
    };
    usable
        .iter()
        .min_by_key(|output| (rank(output), output.connector.index))
        .copied()
        .expect("caller checked that there is at least one")
}

fn debugfs_unknown() -> debugfs::Debugfs {
    debugfs::Debugfs::Unknown {
        reason: "not resolved".into(),
    }
}

/// Paths a consumer needs, resolved once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Devices {
    pub kms: Option<PathBuf>,
    pub render: Option<PathBuf>,
    pub cec: Option<PathBuf>,
    pub alsa_card_id: Option<String>,
    pub alsa_driver: Option<String>,
    pub connector: Option<String>,
    pub connector_sysfs: Option<PathBuf>,
}

impl From<&Platform> for Devices {
    fn from(platform: &Platform) -> Self {
        let selected = platform.selected_output();
        Self {
            kms: platform.kms.as_ref().map(|node| node.device.clone()),
            render: platform.render.as_ref().map(|node| node.device.clone()),
            // Only an adapter the output is firmly tied to: a CEC command sent
            // down a guessed adapter reaches a different television.
            cec: selected
                .filter(|output| output.cec_confidence().actionable())
                .and_then(|output| output.cec.as_ref())
                .map(|adapter| adapter.device.clone()),
            alsa_card_id: selected
                .and_then(|output| output.audio.as_ref())
                .map(|endpoint| endpoint.card_id.clone()),
            alsa_driver: selected
                .and_then(|output| output.audio.as_ref())
                .and_then(|endpoint| endpoint.driver.clone()),
            connector: selected.map(|output| output.connector.name.clone()),
            connector_sysfs: selected.map(|output| output.connector.sysfs.clone()),
        }
    }
}
