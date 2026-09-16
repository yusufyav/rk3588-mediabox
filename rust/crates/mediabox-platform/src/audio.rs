//! Which sound card belongs to which display output.
//!
//! This is the one that bit hardest. On the Ultra the only HDMI connector is
//! `HDMI-A-1` and its sound card is `rockchiphdmi1`; on the Plus `HDMI-A-1` is
//! `rockchiphdmi0` and `rockchiphdmi1` is the *second* connector. A product
//! that writes `amixer -c rockchiphdmi1` is therefore correct on one board and
//! silently addressing the wrong television on the other.
//!
//! The link is in the device tree and is exact: an HDMI sound card's node names
//! its codec by phandle, and that phandle is the transmitter. So the question
//! "which card carries the sound for this output" is answered by following one
//! pointer, on any board, with nothing written down.
//!
//! Card *ids* are used everywhere and card *numbers* nowhere: the id comes from
//! the device tree and the number from probe order, and probe order is not a
//! promise.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::drm::Controller;
use crate::dt::DeviceTree;
use crate::roots::{Roots, file_name, read_trimmed, sorted_children};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioEndpoint {
    /// `rockchiphdmi1`. Stable, from the device tree.
    pub card_id: String,
    /// The number ALSA gave it this boot. Reported so a person reading a log
    /// can find it; never used to address anything.
    pub card_index: Option<u32>,
    /// The first playback device on the card, as `hw:CARD=<id>,DEV=<n>`.
    pub pcm: Option<String>,
    /// What ALSA reports as the card's driver — `rockchip-hdmi1`. alsa-lib
    /// keys `/usr/share/alsa/cards/<driver>.conf` off this, which is how the
    /// compressed-audio PCM this product needs comes to exist at all, so the
    /// name has to be the selected output's rather than a fixed one.
    pub driver: Option<String>,
    /// The transmitter this card carries audio for.
    pub controller: String,
    /// Whether the sink published an ELD. Absent ELD is why compressed audio
    /// cannot be packed, which is a state worth reporting rather than a fault.
    pub eld_present: bool,
}

/// Every sound card on the board, paired with the transmitter it belongs to.
///
/// Cards that are not attached to a display transmitter — the board's own
/// codec, the HDMI receiver's capture card — resolve to no controller and are
/// left out.
pub fn endpoints(roots: &Roots, dt: &DeviceTree, controllers: &[Controller]) -> Vec<AudioEndpoint> {
    let mut found = Vec::new();
    for card in sorted_children(roots.sys("class/sound")) {
        let name = file_name(&card);
        if !name.starts_with("card") || !name[4..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Some(card_id) = read_trimmed(card.join("id")) else {
            continue;
        };
        let Some(of_node) = std::fs::canonicalize(card.join("device/of_node")).ok() else {
            continue;
        };

        // The exact link: the sound card's codec phandle is the transmitter.
        let codec = dt.follow(&of_node, "rockchip,codec").map(PathBuf::from);
        // And the one a board without that property leaves: the card's node is
        // named after the alias of the controller it belongs to, so
        // `hdmi1-sound` is the card for whatever `/aliases/hdmi1` points at.
        let by_alias = || -> Option<PathBuf> {
            let node = file_name(&of_node);
            let alias = node.strip_suffix("-sound")?;
            let target = dt.string_property(&dt.base().join("aliases"), alias)?;
            Some(dt.base().join(target.trim_start_matches('/')))
        };
        let Some(controller_node) = codec.or_else(by_alias) else {
            continue;
        };
        let Some(controller) = controllers
            .iter()
            .find(|controller| controller.of_node.as_deref() == Some(controller_node.as_path()))
        else {
            continue;
        };

        found.push(AudioEndpoint {
            card_index: name[4..].parse().ok(),
            driver: dt.string_property(&of_node, "rockchip,card-name"),
            pcm: first_playback_device(&card)
                .map(|device| format!("hw:CARD={card_id},DEV={device}")),
            eld_present: sorted_children(&card)
                .iter()
                .any(|entry| file_name(entry).starts_with("eld")),
            card_id,
            controller: controller.device.clone(),
        });
    }
    found
}

/// The number of the card's first playback PCM, from `pcmC<card>D<device>p`.
fn first_playback_device(card: &std::path::Path) -> Option<u32> {
    let mut devices: Vec<u32> = sorted_children(card)
        .iter()
        .filter_map(|entry| {
            let name = file_name(entry);
            let rest = name.strip_prefix("pcmC")?;
            let (_, rest) = rest.split_once('D')?;
            rest.strip_suffix('p')?.parse().ok()
        })
        .collect();
    devices.sort_unstable();
    devices.into_iter().next()
}
