//! `mediabox-platform` — what this board is, on stdout.
//!
//! One command, so that a shell script, a unit file and a person debugging an
//! appliance over ssh all get the same answer as the television interface does,
//! from the same code. Scripts that grew their own `for c in
//! /sys/class/drm/card*-HDMI-A-*` loops are what this replaces.
//!
//!   mediabox-platform inspect [--json]   everything, and why
//!   mediabox-platform kms-node           the DRM device to set modes on
//!   mediabox-platform render-node        the DRM device to render on
//!   mediabox-platform output             the selected connector's name
//!   mediabox-platform alsa-card          that output's sound card id
//!   mediabox-platform cec-device         that output's CEC adapter
//!
//! The one-word forms print nothing and exit non-zero when there is no answer,
//! so `card="$(mediabox-platform alsa-card)" || exit` is the whole of a caller's
//! error handling.

use mediabox_platform::video::parse_sink_video;
use mediabox_platform::{Binding, Devices, Platform, SelectionReason};

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "inspect".into());
    let args_rest: Vec<String> = args.collect();
    let json = args_rest.iter().any(|arg| arg == "--json");

    if command == "--help" || command == "-h" || command == "help" {
        eprint!("{}", USAGE);
        return std::process::ExitCode::SUCCESS;
    }

    let platform = Platform::discover();
    let devices = Devices::from(&platform);

    let one = |value: Option<String>| -> std::process::ExitCode {
        match value {
            Some(value) => {
                println!("{value}");
                std::process::ExitCode::SUCCESS
            }
            None => std::process::ExitCode::FAILURE,
        }
    };

    match command.as_str() {
        "inspect" => {
            if json {
                match serde_json::to_string_pretty(&platform) {
                    Ok(text) => println!("{text}"),
                    Err(error) => {
                        eprintln!("mediabox-platform: {error}");
                        return std::process::ExitCode::FAILURE;
                    }
                }
            } else {
                report(&platform);
            }
            // A board with no usable output is a real state — nothing is
            // plugged in — and the report is still worth printing. The exit
            // code is what tells a caller whether the product can come up.
            if platform.selected.is_none() {
                return std::process::ExitCode::FAILURE;
            }
            std::process::ExitCode::SUCCESS
        }
        "kms-node" => one(devices.kms.map(|path| path.display().to_string())),
        "render-node" => one(devices.render.map(|path| path.display().to_string())),
        "output" => one(devices.connector),
        // One line per display output, tab separated: name, state, preferred
        // mode. A stable shape for the shell scripts that used to sweep
        // /sys/class/drm themselves, so that they and the interface agree on
        // what this board's outputs are.
        "outputs" => {
            for output in &platform.outputs {
                println!(
                    "{}\t{}\t{}",
                    output.connector.name,
                    if output.connector.connected {
                        "connected"
                    } else {
                        "disconnected"
                    },
                    output.connector.preferred_mode.as_deref().unwrap_or("-")
                );
            }
            if platform.outputs.is_empty() {
                return std::process::ExitCode::FAILURE;
            }
            std::process::ExitCode::SUCCESS
        }
        "mode" => one(platform
            .selected_output()
            .and_then(|output| output.connector.preferred_mode.clone())),
        "alsa-card" => one(devices.alsa_card_id),
        // The number ALSA gave that card *this boot*. Only for the interfaces
        // that are indexed by number and offer nothing else — /proc/asound is
        // the one that matters. Resolving the stable id to this boot's number
        // here is the opposite of assuming the number: the assumption is what
        // moves when a card probes in a different order.
        "alsa-index" => one(platform
            .selected_output()
            .and_then(|output| output.audio.as_ref())
            .and_then(|audio| audio.card_index)
            .map(|index| index.to_string())),
        "alsa-driver" => one(devices.alsa_driver),
        "connector-path" => one(devices
            .connector_sysfs
            .map(|path| path.display().to_string())),
        "cec-device" => one(devices.cec.map(|path| path.display().to_string())),
        // What the selected television can actually be sent at a given mode.
        //
        // Two separate facts live in an EDID: the formats a sink understands,
        // and how fast a signal it will accept. Their intersection is what may
        // be put on the wire, and it is not the same on two ports of one set --
        // this is the command that says so out loud rather than leaving a
        // player to ask for something the link cannot carry and be quietly
        // given something else.
        //
        //   mediabox-platform color-modes [piksel-saati-kHz]
        "color-modes" => {
            let clock: u32 = args_rest
                .iter()
                .find(|arg| !arg.starts_with('-'))
                .and_then(|arg| arg.parse().ok())
                .unwrap_or(594_000);
            let Some(path) = devices.connector_sysfs.clone() else {
                eprintln!("mediabox-platform: seçili bir çıkış yok");
                return std::process::ExitCode::FAILURE;
            };
            let edid = std::fs::read(path.join("edid")).unwrap_or_default();
            let Some(sink) = parse_sink_video(&edid) else {
                eprintln!("mediabox-platform: {} üzerinde okunabilir bir EDID yok", path.display());
                return std::process::ExitCode::FAILURE;
            };
            let modes = sink.modes_for(clock);
            if json {
                let report = serde_json::json!({
                    "connector": devices.connector,
                    "pixel_clock_khz": clock,
                    "max_character_rate_khz": sink.max_character_rate_khz,
                    "rate_is_declared": sink.rate_is_declared,
                    "st2084": sink.st2084,
                    "hlg": sink.hlg,
                    "hdr10_fits": sink.hdr10_fits(clock),
                    "advertised": sink.advertised,
                    "allowed": modes,
                    "auto_sdr": sink.best_for(clock, false),
                    "auto_hdr": sink.best_for(clock, true),
                });
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
            } else {
                println!(
                    "{}  {} kHz piksel saati",
                    devices.connector.as_deref().unwrap_or("-"),
                    clock
                );
                println!(
                    "  bağlantı tavanı  {} kHz ({})",
                    sink.max_character_rate_khz,
                    if sink.rate_is_declared { "sink bildirdi" } else { "bildirilmedi, taban varsayıldı" }
                );
                println!(
                    "  HDR              ST2084={} HLG={} bu modda sığar={}",
                    sink.st2084,
                    sink.hlg,
                    sink.hdr10_fits(clock)
                );
                let auto_sdr = sink.best_for(clock, false);
                let auto_hdr = sink.best_for(clock, true);
                if modes.is_empty() {
                    println!("  izin verilen     yok — bu mod bu bağlantıya sığmıyor");
                }
                for mode in &modes {
                    let mut marks = Vec::new();
                    if Some(*mode) == auto_sdr {
                        marks.push("SDR varsayılanı");
                    }
                    if Some(*mode) == auto_hdr {
                        marks.push("HDR varsayılanı");
                    }
                    println!(
                        "  {:<16} {} kHz{}{}",
                        mode.label(),
                        mode.character_rate_khz(clock),
                        if marks.is_empty() { "" } else { "  <- " },
                        marks.join(", ")
                    );
                }
            }
            if modes.is_empty() {
                return std::process::ExitCode::FAILURE;
            }
            std::process::ExitCode::SUCCESS
        }

        other => {
            eprintln!("mediabox-platform: bilinmeyen komut '{other}'");
            eprint!("{}", USAGE);
            std::process::ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
kullanım: mediabox-platform <komut> [--json]

  inspect        tüm keşif sonucu (varsayılan)
  kms-node       mod kuran DRM aygıtı
  render-node    çizim yapan DRM aygıtı
  output         seçilen konektör
  outputs        her çıkış için bir satır: ad, durum, tercih edilen mod
  mode           seçilen çıkışın tercih ettiği mod
  alsa-card      o çıkışın ALSA kart kimliği
  alsa-index     o kartın bu açılıştaki ALSA numarası (/proc/asound için)
  cec-device     o çıkışın CEC aygıtı
  color-modes    seçili çıkışa gönderilebilecek renk modları [piksel-saati-kHz]
";

fn report(platform: &Platform) {
    let show = |label: &str, value: String| println!("{label:<14}{value}");
    show(
        "KMS",
        platform
            .kms
            .as_ref()
            .map(|node| {
                format!(
                    "{} ({}, driver {})",
                    node.device.display(),
                    node.name,
                    node.driver.as_deref().unwrap_or("?")
                )
            })
            .unwrap_or_else(|| "-".into()),
    );
    show(
        "Render",
        platform
            .render
            .as_ref()
            .map(|node| format!("{} ({})", node.device.display(), node.name))
            .unwrap_or_else(|| "-".into()),
    );

    println!("\nTransmitters");
    for controller in &platform.controllers {
        println!(
            "  {:<18} {:?}{} cable={}",
            controller.device,
            controller.kind,
            controller
                .alias
                .as_deref()
                .map(|alias| format!(" ({alias})"))
                .unwrap_or_default(),
            match controller.cable_present {
                Some(true) => "yes",
                Some(false) => "no",
                None => "?",
            }
        );
    }

    println!("\nOutputs");
    for output in &platform.outputs {
        let chosen = platform
            .selected_output()
            .is_some_and(|selected| selected.connector.name == output.connector.name);
        println!(
            "  {}{:<12} {:<13} {}",
            if chosen { "* " } else { "  " },
            output.connector.name,
            if output.connector.connected {
                "connected"
            } else {
                "disconnected"
            },
            output.connector.preferred_mode.as_deref().unwrap_or("-")
        );
        println!(
            "      transmitter {} ({})",
            output.controller.as_deref().unwrap_or("-"),
            match output.binding {
                Binding::Measured => "measured",
                Binding::Ordered => "by order",
                Binding::Ambiguous => "ambiguous",
            }
        );
        println!(
            "      audio       {}",
            output
                .audio
                .as_ref()
                .map(|audio| format!(
                    "{} ({}, eld {})",
                    audio.card_id,
                    audio.pcm.as_deref().unwrap_or("-"),
                    if audio.eld_present {
                        "present"
                    } else {
                        "absent"
                    }
                ))
                .unwrap_or_else(|| "-".into())
        );
        println!(
            "      cec         {}",
            output
                .cec
                .as_ref()
                .map(|cec| cec.device.display().to_string())
                .unwrap_or_else(|| "unavailable".into())
        );
        if let Some(sink) = &output.connector.sink {
            println!(
                "      sink        {}{:04X} serial {:08X}{}",
                sink.manufacturer,
                sink.product,
                sink.serial,
                sink.name
                    .as_deref()
                    .map(|name| format!(" \"{name}\""))
                    .unwrap_or_default()
            );
        }
    }

    match &platform.selected {
        Some(selection) => println!(
            "\nSelected      {} ({})",
            selection.output.connector.name,
            match selection.reason {
                SelectionReason::Override => "MEDIABOX_OUTPUT",
                SelectionReason::Configured => "remembered",
                SelectionReason::OnlyConnected => "the only one connected",
                SelectionReason::Policy => "policy",
                SelectionReason::ConfiguredUnavailable => "remembered one absent, policy used",
            }
        ),
        None => println!("\nSelected      none"),
    }
    for warning in &platform.warnings {
        println!("warning       {warning}");
    }
}
