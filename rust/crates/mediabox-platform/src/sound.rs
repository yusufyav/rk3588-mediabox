//! Every real place sound can go, and what the sink at the end of it takes.
//!
//! [`crate::audio`] answers one question -- which card carries the sound of
//! which display transmitter. This answers the one a person asks in the
//! settings: where can sound go on this box right now, and in what form.
//!
//! Each device is named by something that survives a reboot, a replug and a
//! change in probe order, never by an ALSA card number:
//!
//! * a display's own sound is `display:<transmitter>` -- the transmitter's
//!   device-tree address, which is what the card is bound to;
//! * a USB device is `usb:<vendor>:<product>:<card id>`;
//! * anything else is `alsa:<card id>`, the id ALSA derives from the device
//!   tree or the driver.
//!
//! What a display's sink declares is read from the ELD the HDMI driver
//! publishes on the card's `ELD` control. This board's vendor driver leaves
//! that empty while the ELD bypass is on -- which it has to be, or the codec
//! refuses to pack a bitstream at all (`mediabox-hdmi-prepare`) -- and then
//! the same Short Audio Descriptors are read from the EDID of the connector
//! the card is bound to, which is what the kernel builds the ELD from. Which
//! of the two it was is part of the answer.

use std::path::Path;

use mediabox_core::{AudioDevice, AudioKind, CapsSource, SadEntry, SinkAudio};

use crate::drm::ConnectorKind;
use crate::roots::{Roots, file_name, read_trimmed, sorted_children};
use crate::Platform;

/// Where alsa-lib looks for per-driver card definitions. The compressed-audio
/// PCM (`hdmi:CARD=…`) exists for a card only when its driver's file is here.
pub const ALSA_CARDS: &str = "/usr/share/alsa/cards";

/// Every playback device on the board, with what its sink declares.
pub fn devices(platform: &Platform, roots: &Roots, alsa_cards: &Path) -> Vec<AudioDevice> {
    let selected = platform
        .selected_output()
        .and_then(|output| output.audio_route().ok())
        .map(|endpoint| endpoint.card_id.clone());
    let mut found = Vec::new();
    for card in sorted_children(roots.sys("class/sound")) {
        let name = file_name(&card);
        let Some(index) = name.strip_prefix("card").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Some(card_id) = read_trimmed(card.join("id")) else { continue };
        let Some(device_number) = first_playback(&card) else { continue };
        let plug = format!("plughw:CARD={card_id},DEV={device_number}");

        // A display's own sound: only while something is plugged into it.
        let output = platform.outputs.iter().find(|output| {
            output.audio.as_ref().is_some_and(|endpoint| endpoint.card_id == card_id)
        });
        if let Some(output) = output {
            if !output.connector.connected || output.audio_route().is_err() {
                continue;
            }
            let endpoint = output.audio.as_ref().expect("found by it");
            let bitstream = endpoint
                .driver
                .as_ref()
                .is_some_and(|driver| alsa_cards.join(format!("{driver}.conf")).is_file());
            let sink_name = output.connector.sink.as_ref().and_then(|sink| sink.name.clone());
            let eld = read_eld(roots, index).and_then(|eld| sad_from_eld(&eld));
            let sink = eld.or_else(|| {
                std::fs::read(output.connector.sysfs.join("edid"))
                    .ok()
                    .and_then(|edid| sad_from_edid(&edid))
            });
            let kind = match output.connector.kind {
                ConnectorKind::DisplayPort => AudioKind::DisplayPort,
                _ => AudioKind::Hdmi,
            };
            found.push(AudioDevice {
                id: format!("display:{}", endpoint.controller),
                label: match sink_name {
                    Some(sink) => format!("{} · {}", output.connector.name, sink),
                    None => output.connector.name.clone(),
                },
                kind,
                card_id: card_id.clone(),
                card_index: Some(index),
                pcm: if bitstream {
                    format!("hdmi:CARD={card_id},DEV=0")
                } else {
                    plug
                },
                bitstream,
                connector: Some(output.connector.name.clone()),
                display: selected.as_deref() == Some(card_id.as_str()),
                sink,
            });
            continue;
        }

        let usbid = read_trimmed(roots.proc(&format!("asound/card{index}/usbid")));
        let (id, kind, label) = match usbid {
            Some(usbid) => {
                let product = read_trimmed(card.join("device/../product"))
                    .or_else(|| long_name(roots, index))
                    .unwrap_or_else(|| card_id.clone());
                (
                    format!("usb:{usbid}:{card_id}"),
                    AudioKind::Usb,
                    format!("USB · {product}"),
                )
            }
            None => {
                let name = long_name(roots, index).unwrap_or_else(|| card_id.clone());
                (format!("alsa:{card_id}"), AudioKind::Analog, format!("Analog · {name}"))
            }
        };
        found.push(AudioDevice {
            id,
            label,
            kind,
            card_id,
            card_index: Some(index),
            pcm: plug,
            bitstream: false,
            connector: None,
            display: false,
            sink: None,
        });
    }
    found
}

/// The number of the card's first playback PCM (`pcmC<card>D<n>p`).
fn first_playback(card: &Path) -> Option<u32> {
    sorted_children(card)
        .iter()
        .filter_map(|entry| {
            let name = file_name(entry);
            let rest = name.strip_prefix("pcmC")?;
            let (_, rest) = rest.split_once('D')?;
            rest.strip_suffix('p')?.parse().ok()
        })
        .min()
}

/// The card's name as `/proc/asound/cards` has it: `rockchip-es8388`.
fn long_name(roots: &Roots, index: u32) -> Option<String> {
    let text = std::fs::read_to_string(roots.proc("asound/cards")).ok()?;
    text.lines().find_map(|line| {
        let (number, rest) = line.trim_start().split_once(' ')?;
        (number.parse::<u32>().ok()? == index).then(|| {
            rest.split_once("]: ")
                .map(|(_, name)| name.split(" - ").next().unwrap_or(name).trim().to_string())
                .unwrap_or_else(|| rest.trim().to_string())
        })
    })
}

/// The card's `ELD` control, read with `SNDRV_CTL_IOCTL_ELEM_READ` on its
/// control device. `None` when there is no such control, or it is empty.
fn read_eld(roots: &Roots, index: u32) -> Option<Vec<u8>> {
    use std::os::fd::AsRawFd;

    // struct snd_ctl_elem_id and struct snd_ctl_elem_value, include/uapi/
    // sound/asound.h: 64 bytes of id, the `indirect` word, the value union
    // (1024 bytes on a 64-bit kernel, eight-aligned) and 128 reserved.
    #[repr(C)]
    struct ElemId {
        numid: u32,
        iface: i32,
        device: u32,
        subdevice: u32,
        name: [u8; 44],
        index: u32,
    }
    #[repr(C, align(8))]
    struct ElemValue {
        id: ElemId,
        indirect: u32,
        _pad: u32,
        value: [u8; 1024],
        reserved: [u8; 128],
    }
    const _: () = assert!(std::mem::size_of::<ElemValue>() == 1224);
    const SNDRV_CTL_ELEM_IFACE_PCM: i32 = 3;
    // _IOWR('U', 0x12, struct snd_ctl_elem_value)
    const ELEM_READ: u64 = (3 << 30) | (1224 << 16) | ((b'U' as u64) << 8) | 0x12;

    let file = std::fs::File::open(roots.dev(&format!("snd/controlC{index}"))).ok()?;
    let mut value = ElemValue {
        id: ElemId {
            numid: 0,
            iface: SNDRV_CTL_ELEM_IFACE_PCM,
            device: 0,
            subdevice: 0,
            name: [0; 44],
            index: 0,
        },
        indirect: 0,
        _pad: 0,
        value: [0; 1024],
        reserved: [0; 128],
    };
    value.id.name[..3].copy_from_slice(b"ELD");
    // SAFETY: a valid control fd and a struct of exactly the size the ioctl
    // number encodes, which the kernel fills in place.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), ELEM_READ as _, &mut value) };
    if result != 0 {
        return None;
    }
    let eld = value.value[..128].to_vec();
    eld.iter().any(|byte| *byte != 0).then_some(eld)
}

/// The Short Audio Descriptors of an ELD (HDA ELD / `drm_eld.h`): the
/// monitor-name length in the low five bits of byte 4, the descriptor count
/// in the top four of byte 5, the speaker allocation in byte 7, and the
/// descriptors from byte 20 plus the name.
pub fn sad_from_eld(eld: &[u8]) -> Option<SinkAudio> {
    if eld.len() < 20 {
        return None;
    }
    let name_length = usize::from(eld[4] & 0x1f);
    let count = usize::from(eld[5] >> 4);
    let start = 20 + name_length;
    if count == 0 || eld.len() < start + count * 3 {
        return None;
    }
    let entries = eld[start..start + count * 3]
        .chunks_exact(3)
        .filter_map(short_audio_descriptor)
        .collect::<Vec<_>>();
    (!entries.is_empty()).then(|| SinkAudio {
        source: CapsSource::Eld,
        entries,
        speakers: speakers(eld[7]),
    })
}

/// The Short Audio Descriptors of an EDID's CTA extensions: the audio data
/// block (tag 1), and the speaker allocation data block (tag 4).
pub fn sad_from_edid(bytes: &[u8]) -> Option<SinkAudio> {
    let edid = crate::edid::Edid::parse(bytes);
    let mut issues = Vec::new();
    let mut entries = Vec::new();
    let mut allocation = None;
    for (index, block) in edid.cta_extensions() {
        for db in crate::edid::data_blocks(index, block, &mut issues) {
            match db.tag {
                1 => entries.extend(db.payload.chunks_exact(3).filter_map(short_audio_descriptor)),
                4 if !db.payload.is_empty() => allocation = Some(db.payload[0]),
                _ => {}
            }
        }
    }
    (!entries.is_empty()).then(|| SinkAudio {
        source: CapsSource::Edid,
        entries,
        speakers: allocation.map(speakers).unwrap_or_default(),
    })
}

/// One Short Audio Descriptor (CTA-861-G 7.5.2): the format code and the
/// channel count in the first byte, the sample rates in the second, and in
/// the third the sample sizes for LPCM or the maximum bit rate in 8 kbit/s
/// steps for formats 2 to 8.
fn short_audio_descriptor(sad: &[u8]) -> Option<SadEntry> {
    let coding = (sad[0] >> 3) & 0x0f;
    if coding == 0 || coding == 15 {
        return None;
    }
    const RATES: [u32; 7] = [32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
    let rates = RATES
        .iter()
        .enumerate()
        .filter(|(bit, _)| sad[1] & (1 << bit) != 0)
        .map(|(_, rate)| *rate)
        .collect();
    let name = match coding {
        1 => "LPCM",
        2 => "AC-3",
        3 => "MPEG-1",
        4 => "MP3",
        5 => "MPEG-2",
        6 => "AAC LC",
        7 => "DTS",
        8 => "ATRAC",
        9 => "DSD",
        10 => "E-AC-3",
        11 => "DTS-HD",
        12 => "Dolby TrueHD",
        13 => "DST",
        _ => "WMA Pro",
    };
    Some(SadEntry {
        coding,
        name: name.into(),
        channels: (sad[0] & 0x07) + 1,
        rates,
        max_kbps: (2..=8).contains(&coding).then(|| u32::from(sad[2]) * 8),
    })
}

/// The speaker allocation's first byte (CTA-861-G table 65).
fn speakers(byte: u8) -> Vec<String> {
    const NAMES: [&str; 8] = ["FL/FR", "LFE", "FC", "RL/RR", "RC", "FLC/FRC", "RLC/RRC", "FLW/FRW"];
    NAMES
        .iter()
        .enumerate()
        .filter(|(bit, _)| byte & (1 << bit) != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::AudioCodec;

    /// The Sony KD-65XE9005 the Plus is measured on, read off HDMI-A-2 on
    /// 2026-09-27.
    const SONY: &str = include_str!("../tests/sony-kd65xe9005.edid.hex");

    fn bytes(hex: &str) -> Vec<u8> {
        let hex: String = hex.split_whitespace().collect();
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_sony_declares_six_channel_pcm_ac3_dts_and_eac3() {
        let sink = sad_from_edid(&bytes(SONY)).expect("an audio data block");
        assert_eq!(sink.source, CapsSource::Edid);
        // As edid-decode reads the same bytes: LPCM 6ch, AC-3 6ch 640k,
        // DTS 6ch 1504k, E-AC-3 8ch at 48 kHz.
        assert_eq!(sink.codecs(), vec![AudioCodec::Ac3, AudioCodec::Eac3, AudioCodec::Dts]);
        assert_eq!(sink.pcm_channels(), 6);
        assert_eq!(sink.ac3_max_kbps(), Some(640));
        let dts = sink.entries.iter().find(|entry| entry.coding == 7).unwrap();
        assert_eq!(dts.max_kbps, Some(1504));
        assert_eq!(dts.rates, vec![32_000, 44_100, 48_000]);
        assert_eq!(sink.speakers, vec!["FL/FR", "LFE", "FC", "RL/RR"]);
    }

    #[test]
    fn an_eld_is_read_as_the_descriptors_it_carries() {
        // A 6-byte monitor name, two descriptors: LPCM 2ch, AC-3 6ch 640k.
        let mut eld = vec![0u8; 20];
        eld[0] = 0x10;
        eld[4] = 6;
        eld[5] = 2 << 4;
        eld[7] = 0x0f;
        eld.extend_from_slice(b"SONYTV");
        eld.extend_from_slice(&[0x09, 0x07, 0x07]);
        eld.extend_from_slice(&[0x15, 0x07, 0x50]);
        let sink = sad_from_eld(&eld).unwrap();
        assert_eq!(sink.source, CapsSource::Eld);
        assert_eq!(sink.codecs(), vec![AudioCodec::Ac3]);
        assert_eq!(sink.ac3_max_kbps(), Some(640));
        assert_eq!(sink.pcm_channels(), 2);
        // The all-zero ELD this driver publishes under the bypass is none.
        assert_eq!(sad_from_eld(&[0u8; 128]), None);
    }
}
