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
//!   mediabox-platform plan [--wait <s>]  the display plan, if it is for what
//!                                        is plugged in now
//!   mediabox-platform sway-output        the browser compositor's output line
//!   mediabox-platform edid [--json]      that output's EDID, checked
//!   mediabox-platform source [--json]    the source profile this system matches
//!
//! The one-word forms print nothing and exit non-zero when there is no answer,
//! so `card="$(mediabox-platform alsa-card)" || exit` is the whole of a caller's
//! error handling.

use mediabox_platform::{Confidence, Devices, Platform, SelectionReason};

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
        //
        // The fourth column is the SHA-256 of the EDID the connector
        // publishes: a different sink on a socket that never read
        // `disconnected` in between is still a different line.
        "outputs" => {
            for output in &platform.outputs {
                println!(
                    "{}\t{}\t{}\t{}",
                    output.connector.name,
                    if output.connector.connected {
                        "connected"
                    } else {
                        "disconnected"
                    },
                    output.connector.preferred_mode.as_deref().unwrap_or("-"),
                    output
                        .connector
                        .sink
                        .as_ref()
                        .and_then(|sink| sink.sha256.as_deref())
                        .unwrap_or("-")
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
        // Nothing, and the reason on stderr, when the output's transmitter is
        // not firmly known: a caller then sends sound nowhere rather than to
        // the likely card.
        "alsa-card" => match platform.selected_output().map(|output| output.audio_route()) {
            Some(Ok(audio)) => one(Some(audio.card_id.clone())),
            Some(Err(why)) => {
                eprintln!("mediabox-platform: {why}");
                std::process::ExitCode::FAILURE
            }
            None => std::process::ExitCode::FAILURE,
        },
        // The number ALSA gave that card *this boot*. Only for the interfaces
        // that are indexed by number and offer nothing else — /proc/asound is
        // the one that matters. Resolving the stable id to this boot's number
        // here is the opposite of assuming the number: the assumption is what
        // moves when a card probes in a different order.
        "alsa-index" => one(platform
            .selected_output()
            .and_then(|output| output.audio_route().ok())
            .and_then(|audio| audio.card_index)
            .map(|index| index.to_string())),
        "alsa-driver" => one(devices.alsa_driver),
        "connector-path" => one(devices
            .connector_sysfs
            .map(|path| path.display().to_string())),
        "cec-device" => match platform.selected_output().map(|output| output.cec_route()) {
            Some(Ok(adapter)) => one(Some(adapter.device.display().to_string())),
            Some(Err(why)) => {
                eprintln!("mediabox-platform: {why}");
                std::process::ExitCode::FAILURE
            }
            None => std::process::ExitCode::FAILURE,
        },
        // The plan the control plane wrote for Kodi and the browser, printed
        // only if it was made for this boot, the observer's current
        // generation, and the connector and EDID read now. Otherwise nothing
        // on stdout, why on stderr, and a failing exit: an owner then starts
        // on its own defaults for this sink rather than on another sink's
        // mode. `--wait <s>` looks again until one is current.
        "plan" => {
            let wait = flag_value(&args_rest, "--wait")
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(0.0);
            match current_plan(&platform, wait) {
                Ok(plan) => {
                    print!("{}", plan.render());
                    std::process::ExitCode::SUCCESS
                }
                Err(why) => {
                    eprintln!("mediabox-platform: plan: {why}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        // What sway includes: the plan's mode on the plan's connector, or a
        // comment saying why there is none -- never a mode for all outputs,
        // and never one from a plan for another sink.
        "sway-output" => {
            match current_plan(&platform, 0.0) {
                Ok(plan) => print!("{}", plan.sway_output()),
                Err(why) => println!("# no current display plan: {why}"),
            }
            std::process::ExitCode::SUCCESS
        }
        // The KMS device's debugfs directory, found from the device rather
        // than assumed to be `dri/0`. Nothing when it cannot be resolved and
        // checked -- a script then says "unknown", it does not guess.
        "dri-debugfs" => one(platform
            .debugfs
            .dir()
            .map(|dir| dir.display().to_string())),
        // The selected output's EDID as the checked parser reads it: its
        // status, its identity, what the CTA blocks declare and what was set
        // aside. Diagnostics; nothing reads this to decide anything.
        // The source profile, from what can be read without opening the
        // display device. The connector's properties are the interface's to
        // read (it holds the device); here they are reported as unverified.
        "source" => {
            let signature = mediabox_platform::source::SourceSignature {
                static_part: mediabox_platform::source::static_signature(
                    &mediabox_platform::Roots::from_env(),
                    &platform,
                ),
                properties: None,
            };
            let resolved = mediabox_platform::source::resolve(&signature);
            if json {
                let value = serde_json::json!({
                    "profile": resolved.profile.name(),
                    "match": resolved.matched,
                    "signature": signature,
                });
                println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            } else {
                println!("profile       {}", resolved.describe());
            }
            std::process::ExitCode::SUCCESS
        }
        "edid" => {
            let Some(output) = platform.selected_output() else {
                return std::process::ExitCode::FAILURE;
            };
            let bytes = std::fs::read(output.connector.sysfs.join("edid")).unwrap_or_default();
            let report = mediabox_platform::edid::EdidReport::of(&bytes);
            if json {
                match serde_json::to_string_pretty(&report) {
                    Ok(text) => println!("{text}"),
                    Err(error) => {
                        eprintln!("mediabox-platform: {error}");
                        return std::process::ExitCode::FAILURE;
                    }
                }
            } else {
                println!("connector     {}", output.connector.name);
                println!("status        {:?}", report.status);
                println!("bytes         {}", report.bytes);
                println!(
                    "sha256        {}",
                    report.sha256.as_ref().map(|id| id.0.as_str()).unwrap_or("-")
                );
                println!("checkvalue    {} (legacy)", report.legacy_checkvalue);
                for issue in &report.issues {
                    println!("issue         {issue:?}");
                }
            }
            if report.status.usable() {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        other => {
            eprintln!("mediabox-platform: bilinmeyen komut '{other}'");
            eprint!("{}", USAGE);
            std::process::ExitCode::FAILURE
        }
    }
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

/// The plan file, if it is for what is plugged in now; looked for again for
/// up to `wait` seconds.
fn current_plan(platform: &Platform, wait: f64) -> Result<mediabox_platform::output::Plan, String> {
    let roots = mediabox_platform::Roots::from_env();
    let path = roots.run(mediabox_platform::output::PLAN_FILE);
    let observer = roots.run(mediabox_platform::observer::RUNTIME);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(wait.max(0.0));
    let mut platform = std::borrow::Cow::Borrowed(platform);
    loop {
        let answer = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))
            .and_then(|text| mediabox_platform::output::Plan::parse(&text))
            .and_then(|plan| {
                let generation = mediabox_platform::observer::published_generation(&observer);
                match plan.stale(generation.as_ref(), platform.selected_identity().as_ref()) {
                    Some(why) => Err(why),
                    None => Ok(plan),
                }
            });
        if answer.is_ok() || std::time::Instant::now() >= deadline {
            return answer;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        platform = std::borrow::Cow::Owned(Platform::discover());
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
  plan           şu anki ekran için yazılmış plan (değilse hata) [--wait <sn>]
  sway-output    tarayıcı kompozitörünün çıkış satırı
  dri-debugfs    mod kuran DRM aygıtının debugfs dizini (doğrulanmış)
  edid           o çıkışın EDID'i: durum, SHA-256 kimlik, sorunlar
  source         bu sistemin eşleştiği kaynak profili
";

fn confidence(value: Confidence) -> &'static str {
    match value {
        Confidence::Exact => "exact",
        Confidence::Measured => "measured",
        Confidence::Derived => "derived: connector order",
        Confidence::Ambiguous => "ambiguous",
        Confidence::Unavailable => "unavailable",
    }
}

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
        "debugfs",
        match &platform.debugfs {
            mediabox_platform::debugfs::Debugfs::Resolved { dir, minor } => {
                format!("{} (minor {minor})", dir.display())
            }
            mediabox_platform::debugfs::Debugfs::Unknown { reason } => format!("unknown: {reason}"),
        },
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
            confidence(output.binding)
        );
        for line in &output.evidence {
            println!("                  {line}");
        }
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
                .map(|cec| format!(
                    "{} ({})",
                    cec.device.display(),
                    confidence(output.cec_confidence())
                ))
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
            println!(
                "      edid        {:?} sha256 {}",
                sink.edid_status,
                sink.sha256.as_deref().unwrap_or("-")
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
