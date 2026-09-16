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
pub mod cec;
pub mod drm;
pub mod dt;
pub mod roots;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use audio::AudioEndpoint;
pub use cec::CecAdapter;
pub use drm::{Connector, ConnectorKind, Controller, DrmNode, SinkIdentity};
pub use roots::Roots;

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

/// How firmly a connector was tied to the transmitter behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    /// The transmitter reports a cable and exactly one connector of its kind is
    /// connected. Measured, on this boot, from two independent places.
    Measured,
    /// Nothing was plugged in to compare, so connectors and transmitters of the
    /// same kind were paired in order. The kernel numbers connectors in
    /// registration order and this orders transmitters by address; on every
    /// board measured so far the two agree, and where anything *was* plugged in
    /// the measurement above confirms it.
    Ordered,
    /// Order and measurement disagree, or there is more than one way to read
    /// the topology. Reported rather than resolved: a wrong answer here sends
    /// audio to a different television than the picture.
    Ambiguous,
}

/// A display output, with everything that belongs to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Output {
    pub connector: Connector,
    /// The transmitter driving it, if it could be identified.
    pub controller: Option<String>,
    pub binding: Binding,
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

        let bound = bind(&connectors, &controllers, &mut warnings);
        let outputs: Vec<Output> = bound
            .into_iter()
            .map(|(connector, controller, binding)| Output {
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
            })
            .collect();

        let selected = select(roots, overrides, &outputs, &mut warnings);
        Self {
            kms: Some(kms),
            render,
            controllers,
            outputs,
            selected,
            warnings,
        }
    }

    pub fn selected_output(&self) -> Option<&Output> {
        self.selected.as_ref().map(|selection| &selection.output)
    }
}

/// Tie each display connector to the transmitter behind it.
///
/// Two independent readings, and they are combined rather than ranked: the
/// measured one is used when it is unambiguous, the ordered one otherwise, and
/// a disagreement between them is reported instead of being resolved.
fn bind(
    connectors: &[Connector],
    controllers: &[Controller],
    warnings: &mut Vec<String>,
) -> Vec<(Connector, Option<String>, Binding)> {
    let mut out = Vec::new();
    for connector in connectors {
        if !connector.kind.is_display_output() {
            continue;
        }
        let kind = connector.kind;
        let same_kind: Vec<&Controller> = controllers
            .iter()
            .filter(|controller| controller.kind == kind)
            .collect();

        // In order: the Nth connector of a type is the Nth transmitter of that
        // type. Connector numbering starts at 1.
        let ordered = connector
            .index
            .checked_sub(1)
            .and_then(|slot| same_kind.get(slot as usize))
            .map(|controller| controller.device.clone());

        // Measured: one connector of this kind is plugged in, and one
        // transmitter of this kind says so.
        let connected_of_kind = connectors
            .iter()
            .filter(|other| other.kind == kind && other.connected)
            .count();
        let live: Vec<&&Controller> = same_kind
            .iter()
            .filter(|controller| controller.cable_present == Some(true))
            .collect();
        let measured = (connector.connected && connected_of_kind == 1 && live.len() == 1)
            .then(|| live[0].device.clone());

        let (controller, binding) = match (measured, ordered) {
            (Some(measured), Some(ordered)) if measured == ordered => {
                (Some(measured), Binding::Measured)
            }
            (Some(measured), Some(ordered)) => {
                warnings.push(format!(
                    "{}: the cable is in {measured} but connector order says {ordered}; \
                     audio and CEC are not being tied to an output",
                    connector.name
                ));
                let _ = ordered;
                (Some(measured), Binding::Ambiguous)
            }
            (Some(measured), None) => (Some(measured), Binding::Measured),
            (None, Some(ordered)) => (Some(ordered), Binding::Ordered),
            (None, None) => {
                if !controllers.is_empty() {
                    warnings.push(format!(
                        "{}: no transmitter of its kind was found",
                        connector.name
                    ));
                }
                (None, Binding::Ambiguous)
            }
        };
        out.push((connector.clone(), controller, binding));
    }
    out
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
            cec: selected
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
