//! Which transmitter drives which connector, and how sure that is.
//!
//! The kernel publishes no link from a DRM connector to the transmitter behind
//! it. What it does publish, independently, is enough to measure one:
//!
//! * the sink's EDID carries the physical address it gave this input, and the
//!   transmitter's CEC adapter is given that same address when the sink is on
//!   its socket -- so an address match ties a connector (by its EDID) to an
//!   adapter, and the adapter is structurally the transmitter's;
//! * the transmitter's own hotplug state (extcon) says whether a cable is in
//!   it;
//! * the transmitter's PHY feeds a pixel clock that runs, at the rate the
//!   display controller reports for the connector, only while that connector
//!   is lit.
//!
//! Connector order -- the Nth `HDMI-A` is the Nth HDMI transmitter by address
//! -- is a derivation, not a measurement, and never decides on its own when
//! anything measured disagrees. Two candidates that fit equally well are
//! [`Confidence::Ambiguous`]; nothing here picks the first one.

use serde::{Deserialize, Serialize};

use crate::debugfs::{self, ConnectorActivity};
use crate::drm::{Connector, Controller};
use crate::roots::{Roots, file_name, sorted_children};
use crate::{CecAdapter, Confidence};

/// What was measured about one transmitter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransmitterEvidence {
    /// The transmitter's hotplug state.
    pub cable: Option<bool>,
    /// The physical address its CEC adapter holds: `Some(None)` is
    /// `f.f.f.f`, no sink; `None` is not known.
    pub cec_physical_address: Option<Option<u16>>,
    /// The pixel clock its PHY provides, by name, with its rate and whether it
    /// is enabled.
    pub phy_clock: Option<PhyClock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhyClock {
    pub name: String,
    pub rate_hz: u64,
    pub enabled: bool,
}

/// Gather what can be measured about each transmitter.
pub fn measure(
    roots: &Roots,
    controllers: &[Controller],
    adapters: &[CecAdapter],
) -> Vec<(String, TransmitterEvidence)> {
    controllers
        .iter()
        .map(|controller| {
            let cec = adapters
                .iter()
                .find(|adapter| adapter.controller == controller.device)
                .and_then(|adapter| debugfs::cec_physical_address(roots, &adapter.name).known());
            (
                controller.device.clone(),
                TransmitterEvidence {
                    cable: controller.cable_present,
                    cec_physical_address: cec,
                    phy_clock: phy_clock(roots, controller),
                },
            )
        })
        .collect()
}

/// The pixel clock of the PHY a transmitter consumes: the device link
/// `supplier:phy:*` names the PHY, the PHY's device-tree node names its output
/// clock (`clock-output-names`), and the clock framework reports that clock.
fn phy_clock(roots: &Roots, controller: &Controller) -> Option<PhyClock> {
    for entry in sorted_children(&controller.sysfs) {
        if !file_name(&entry).starts_with("supplier:phy:") {
            continue;
        }
        let Ok(link) = std::fs::canonicalize(&entry) else { continue };
        let Ok(phy) = std::fs::canonicalize(link.join("supplier")) else { continue };
        // .../<phy platform device>/phy/<phy name>
        let Some(device) = phy.parent().and_then(|dir| dir.parent()) else { continue };
        let Ok(names) = std::fs::read(device.join("of_node/clock-output-names")) else {
            continue;
        };
        let Some(name) = names
            .split(|byte| *byte == 0)
            .find(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
        else {
            continue;
        };
        if let Some((rate_hz, enabled)) = debugfs::clock(roots, &name).known() {
            return Some(PhyClock {
                name,
                rate_hz,
                enabled: enabled > 0,
            });
        }
    }
    None
}

/// How one transmitter fits one connected connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    /// Something measured says it is not this one.
    Contradicted,
    /// Nothing measured either way.
    Silent,
    /// Measured to fit, but not by anything unique to this sink.
    Weak,
    /// The sink's own physical address is on this transmitter's adapter.
    Strong,
}

fn fit(
    connector: &Connector,
    evidence: &TransmitterEvidence,
    activity: Option<&ConnectorActivity>,
    notes: &mut Vec<String>,
    device: &str,
) -> Fit {
    let mut support = Fit::Silent;
    if evidence.cable == Some(false) {
        notes.push(format!("{device}: no cable"));
        return Fit::Contradicted;
    }
    if evidence.cable == Some(true) {
        support = Fit::Weak;
    }
    match (evidence.cec_physical_address, connector.physical_address) {
        (Some(None), _) => {
            notes.push(format!("{device}: CEC adapter has no physical address (f.f.f.f)"));
            return Fit::Contradicted;
        }
        (Some(Some(held)), Some(declared)) if held != declared => {
            notes.push(format!(
                "{device}: CEC holds {} but {}'s EDID declares {}",
                pa(held),
                connector.name,
                pa(declared)
            ));
            return Fit::Contradicted;
        }
        (Some(Some(held)), Some(_)) => {
            notes.push(format!(
                "{device}: CEC physical address {} is {}'s EDID address",
                pa(held),
                connector.name
            ));
            support = Fit::Strong;
        }
        _ => {}
    }
    if let (Some(clock), Some(activity)) = (&evidence.phy_clock, activity)
        && activity.active
    {
        if !clock.enabled {
            notes.push(format!(
                "{device}: {} is lit on video port {} but PHY clock {} is off",
                connector.name, activity.video_port, clock.name
            ));
            return Fit::Contradicted;
        }
        let matches_dclk = activity
            .dclk_khz
            .is_some_and(|dclk| u64::from(dclk) * 1000 == clock.rate_hz);
        notes.push(format!(
            "{device}: PHY clock {} running at {} Hz{}",
            clock.name,
            clock.rate_hz,
            if matches_dclk { ", the connector's dclk" } else { "" }
        ));
        if support == Fit::Silent {
            support = Fit::Weak;
        }
    }
    support
}

fn pa(address: u16) -> String {
    format!(
        "{:x}.{:x}.{:x}.{:x}",
        address >> 12,
        (address >> 8) & 0xF,
        (address >> 4) & 0xF,
        address & 0xF
    )
}

/// One connector's binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub connector: Connector,
    pub controller: Option<String>,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
}

/// Tie each display connector to the transmitter behind it.
pub fn bind(
    connectors: &[Connector],
    controllers: &[Controller],
    measured: &[(String, TransmitterEvidence)],
    summary: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<Bound> {
    let evidence_of = |device: &str| {
        measured
            .iter()
            .find(|(name, _)| name == device)
            .map(|(_, evidence)| evidence.clone())
            .unwrap_or_default()
    };
    let mut out: Vec<Bound> = Vec::new();
    for connector in connectors {
        if !connector.kind.is_display_output() {
            continue;
        }
        let kind = connector.kind;
        let same_kind: Vec<&Controller> = controllers
            .iter()
            .filter(|controller| controller.kind == kind)
            .collect();
        let mut notes = Vec::new();

        // In order: the Nth connector of a type is the Nth transmitter of that
        // type. Connector numbering starts at 1.
        let ordered = connector
            .index
            .checked_sub(1)
            .and_then(|slot| same_kind.get(slot as usize))
            .map(|controller| controller.device.clone());

        if same_kind.is_empty() {
            if !controllers.is_empty() {
                warnings.push(format!("{}: no transmitter of its kind was found", connector.name));
            }
            out.push(Bound {
                connector: connector.clone(),
                controller: None,
                confidence: Confidence::Unavailable,
                evidence: notes,
            });
            continue;
        }

        if !connector.connected {
            // Nothing plugged in, nothing to measure against.
            let confidence = if ordered.is_some() {
                notes.push("connector order (nothing plugged in to measure)".into());
                Confidence::Derived
            } else {
                Confidence::Ambiguous
            };
            out.push(Bound {
                connector: connector.clone(),
                controller: ordered,
                confidence,
                evidence: notes,
            });
            continue;
        }

        let activity = summary.and_then(|text| debugfs::connector_activity(text, &connector.name));
        let fits: Vec<(&Controller, Fit)> = same_kind
            .iter()
            .map(|controller| {
                let evidence = evidence_of(&controller.device);
                (
                    *controller,
                    fit(connector, &evidence, activity.as_ref(), &mut notes, &controller.device),
                )
            })
            .collect();
        let strong: Vec<&Controller> = fits
            .iter()
            .filter(|(_, fit)| *fit == Fit::Strong)
            .map(|(controller, _)| *controller)
            .collect();
        let fitting: Vec<&Controller> = fits
            .iter()
            .filter(|(_, fit)| matches!(fit, Fit::Strong | Fit::Weak))
            .map(|(controller, _)| *controller)
            .collect();
        let anything_measured = fits.iter().any(|(_, fit)| *fit != Fit::Silent);
        let connected_of_kind = connectors
            .iter()
            .filter(|other| other.kind == kind && other.connected)
            .count();

        let (controller, confidence) = if strong.len() == 1 {
            // The sink's own address, on one adapter only.
            let chosen = strong[0].device.clone();
            if ordered.as_deref().is_some_and(|ordered| ordered != chosen) {
                notes.push(format!(
                    "connector order says {}, the physical address says {chosen}",
                    ordered.as_deref().unwrap_or("-")
                ));
            }
            (Some(chosen), Confidence::Measured)
        } else if strong.len() > 1 {
            warnings.push(format!(
                "{}: the physical address {} is held by more than one CEC adapter; \
                 audio and CEC are not being tied to an output",
                connector.name,
                connector.physical_address.map(pa).unwrap_or_default()
            ));
            (ordered.clone(), Confidence::Ambiguous)
        } else if fitting.len() == 1 && connected_of_kind == 1 {
            let chosen = fitting[0].device.clone();
            match &ordered {
                Some(ordered) if *ordered == chosen => (Some(chosen), Confidence::Measured),
                Some(ordered) => {
                    warnings.push(format!(
                        "{}: the cable is in {chosen} but connector order says {ordered}; \
                         audio and CEC are not being tied to an output",
                        connector.name
                    ));
                    (Some(chosen), Confidence::Ambiguous)
                }
                None => (Some(chosen), Confidence::Measured),
            }
        } else if fitting.len() > 1 {
            // Two transmitters fit equally well: two sinks connected and
            // nothing that tells them apart. Not resolved by picking one.
            notes.push(format!(
                "{} transmitters fit equally: {}",
                fitting.len(),
                fitting
                    .iter()
                    .map(|controller| controller.device.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            (ordered.clone(), Confidence::Ambiguous)
        } else if !anything_measured {
            // A board that publishes none of the evidence: order is all there
            // is, and nothing disagrees with it.
            notes.push("connector order (nothing on this board to measure)".into());
            (ordered.clone(), if ordered.is_some() { Confidence::Derived } else { Confidence::Ambiguous })
        } else {
            warnings.push(format!(
                "{}: connected, but no transmitter of its kind measures as its own",
                connector.name
            ));
            (ordered.clone(), Confidence::Ambiguous)
        };
        out.push(Bound {
            connector: connector.clone(),
            controller,
            confidence,
            evidence: notes,
        });
    }

    // One transmitter, one connector. A transmitter claimed twice is kept by
    // the firmer claim; equal claims are both ambiguous.
    // Decided on the claims as they stood, not as this pass rewrites them.
    let claims: Vec<(Option<String>, Confidence)> = out
        .iter()
        .map(|bound| (bound.controller.clone(), bound.confidence))
        .collect();
    for (index, bound) in out.iter_mut().enumerate() {
        let Some(device) = bound.controller.clone() else { continue };
        let firmest_rival = claims
            .iter()
            .enumerate()
            .filter(|(other, (claimed, _))| *other != index && claimed.as_deref() == Some(&device))
            .map(|(_, (_, confidence))| *confidence)
            .min();
        if let Some(rival) = firmest_rival
            && bound.confidence >= rival
            && bound.confidence.actionable()
        {
            bound
                .evidence
                .push(format!("{device} is claimed by another connector as firmly or more"));
            bound.confidence = Confidence::Ambiguous;
        }
    }
    out
}
