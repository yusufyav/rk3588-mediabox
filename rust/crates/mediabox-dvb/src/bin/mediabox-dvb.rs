//! mediabox-dvb: the satellite receiver's process.
//!
//! `play` is what the control plane starts (as the transient
//! mediabox-dvb.service): one service of one source into a FIFO the player
//! reads. The receiver is opened only by `play --hardware`; every other mode
//! -- `play --recording`, `selftest`, `select`, `analyse`, `firmware-check`
//! -- has no way to reach it (`usb::HardwareAccess`). Nothing here powers or
//! steers an LNB.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use mediabox_dvb::log::Log;
use mediabox_dvb::service::{self, Options, Output, Source};
use mediabox_dvb::tune::{TuneParams, universal_if_khz};
use mediabox_dvb::usb::HardwareAccess;
use mediabox_dvb::ts::{Analyser, Selector};

#[derive(Parser)]
#[command(name = "mediabox-dvb", about = "Kanky Link-4K DVB-S2 receiver for MediaBox")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One service of a recording or of the receiver, into a FIFO / file / stdout.
    Play(Box<Play>),
    /// Offline: write the single-programme TS of one service of a recording.
    Select {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        service: u16,
        #[arg(long)]
        output: PathBuf,
    },
    /// Offline: integrity and programmes of a recording, as JSON.
    Analyse {
        #[arg(long)]
        input: PathBuf,
    },
    /// Check the firmware files against the verified digests. No USB access.
    FirmwareCheck {
        #[arg(long, default_value = mediabox_dvb::firmware::DEFAULT_DIR)]
        firmware_dir: PathBuf,
    },
    /// Everything that can be checked without the receiver: libusb's symbols
    /// (no context is created, no device listed), the firmware files, the TS
    /// pipeline and the SPS reader, a FIFO, and optionally a recording.
    Selftest {
        #[arg(long, default_value = mediabox_dvb::firmware::DEFAULT_DIR)]
        firmware_dir: PathBuf,
        #[arg(long)]
        recording: Option<PathBuf>,
        #[arg(long, default_value_t = 10801)]
        service: u16,
    },
}

#[derive(Args)]
struct Play {
    #[arg(long)]
    service: u16,
    /// Read this recording to its end instead of the receiver. Never opens USB.
    #[arg(long, conflicts_with_all = ["transponder", "rf_khz", "hardware"])]
    recording: Option<PathBuf>,
    /// STATE-CHANGING: open the real receiver (reset, firmware, tune). Without
    /// it nothing touches USB. The LNB is never powered: the RF source,
    /// supply and tone included, is prepared outside MediaBox.
    #[arg(long)]
    hardware: bool,
    /// Write the whole multiplex unchanged (diagnostics: NIT, SDT other).
    #[arg(long)]
    raw: bool,
    /// Read the recording at the pace of its programme clock, as a broadcast.
    #[arg(long, requires = "recording")]
    pace: bool,
    /// Ku-band frequency in kHz; the tuner is set to its IF behind a
    /// universal LNB (9.75 / 10.6 GHz oscillator by band).
    #[arg(long, requires = "hardware")]
    transponder: Option<u32>,
    /// L-band frequency at the tuner input.
    #[arg(long, conflicts_with = "transponder", requires = "hardware")]
    rf_khz: Option<u32>,
    /// Symbol rate in kS/s.
    #[arg(long)]
    sr: Option<u32>,
    /// When the carrier does not lock at the nominal IF, retune by +step and
    /// then -step (kHz); 0 = nominal only.
    #[arg(long, default_value_t = 10_000)]
    carrier_search_khz: i32,
    #[arg(long, value_parser = clap::value_parser!(u32).range(10..=5000))]
    lock_timeout_ms: Option<u32>,
    #[arg(long, default_value = mediabox_dvb::firmware::DEFAULT_DIR)]
    firmware_dir: PathBuf,
    /// Create this FIFO and write into it once a reader has opened it.
    #[arg(long, conflicts_with_all = ["output", "stdout"])]
    fifo: Option<PathBuf>,
    /// Write into this new file.
    #[arg(long, conflicts_with = "stdout")]
    output: Option<PathBuf>,
    #[arg(long)]
    stdout: bool,
    #[arg(long)]
    status: Option<PathBuf>,
    /// Echoed in the status file (the control plane's start).
    #[arg(long)]
    token: Option<String>,
    /// Log file; it keeps every USB frame.
    #[arg(long)]
    log: Option<PathBuf>,
    #[arg(long, default_value_t = 30)]
    reader_timeout_s: u64,
    #[arg(long, default_value_t = 10)]
    service_timeout_s: u64,
    #[arg(long, default_value_t = mediabox_dvb::usb::DEFAULT_TRANSFERS)]
    usb_transfers: usize,
    #[arg(long, default_value_t = mediabox_dvb::usb::DEFAULT_BUFFER)]
    usb_buffer_bytes: usize,
    /// Demodulator status while streaming every N s; 0 = never.
    #[arg(long, default_value_t = 5)]
    signal_every_s: u64,
    /// After a USB disconnect, wait this long for the receiver; 0 = end.
    #[arg(long, default_value_t = 0)]
    reconnect_s: u64,
    #[arg(long, default_value_t = 32 << 20)]
    output_queue_bytes: usize,
    /// Stay up between players this many seconds (0 = end with the first
    /// player), changing service on `select.json` beside --status.
    #[arg(long, default_value_t = 0)]
    hold_reader_s: u64,
}

fn play(args: Play) -> ExitCode {
    let source = if let Some(path) = args.recording {
        Source::Recording(path)
    } else if args.hardware {
        let Some(sr) = args.sr else {
            eprintln!("mediabox-dvb: --sr is required with the receiver");
            return ExitCode::from(2);
        };
        let rf_khz = match (args.transponder, args.rf_khz) {
            (Some(frequency), None) => universal_if_khz(frequency),
            (None, Some(rf)) => rf,
            _ => {
                eprintln!("mediabox-dvb: --hardware needs --transponder or --rf-khz");
                return ExitCode::from(2);
            }
        };
        Source::Satellite {
            params: TuneParams {
                rf_khz,
                symbol_rate_ksps: sr,
                lock_timeout_ms: args.lock_timeout_ms,
                search_offsets_khz: match args.carrier_search_khz {
                    0 => Vec::new(),
                    step => vec![step.abs(), -step.abs()],
                },
            },
            firmware_dir: args.firmware_dir,
            access: HardwareAccess::chosen_with_hardware_flag(),
        }
    } else {
        eprintln!("mediabox-dvb: --recording FILE, or --hardware for the receiver itself");
        return ExitCode::from(2);
    };
    let output = match (args.fifo, args.output, args.stdout) {
        (Some(path), None, false) => Output::Fifo(path),
        (None, Some(path), false) => Output::File(path),
        (None, None, true) => {
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                eprintln!("mediabox-dvb: stdout is a terminal; pipe it into a player");
                return ExitCode::from(2);
            }
            Output::Stdout
        }
        _ => {
            eprintln!("mediabox-dvb: one of --fifo, --output, --stdout");
            return ExitCode::from(2);
        }
    };
    let log = match Log::new(args.log.as_deref()) {
        Ok(log) => log,
        Err(error) => {
            eprintln!("mediabox-dvb: log: {error}");
            return ExitCode::from(2);
        }
    };
    service::install_signal_handlers();
    let options = Options {
        service_id: args.service,
        token: args.token,
        source,
        output,
        status: args.status,
        reader_timeout: Duration::from_secs(args.reader_timeout_s),
        service_timeout: Duration::from_secs(args.service_timeout_s),
        usb_transfers: args.usb_transfers,
        usb_buffer_bytes: args.usb_buffer_bytes,
        usb_queue_bytes: mediabox_dvb::usb::DEFAULT_QUEUE_BYTES,
        signal_every: (args.signal_every_s > 0).then(|| Duration::from_secs(args.signal_every_s)),
        reconnect: Duration::from_secs(args.reconnect_s),
        pace: args.pace,
        raw: args.raw,
        output_queue_bytes: args.output_queue_bytes,
        hold_reader: (args.hold_reader_s > 0).then(|| Duration::from_secs(args.hold_reader_s)),
    };
    let status = service::run(options, &log);
    match status.end {
        Some(end) if !end.is_failure() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

fn select(input: PathBuf, service: u16, output: PathBuf) -> ExitCode {
    let data = match std::fs::read(&input) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("mediabox-dvb: {}: {error}", input.display());
            return ExitCode::FAILURE;
        }
    };
    let mut analyser = Analyser::new();
    let mut selector = Selector::new(service);
    let mut out = Vec::new();
    for chunk in data.chunks(153_408) {
        analyser.feed(chunk, |packet| selector.packet(packet, &mut out));
    }
    if let Err(error) = std::fs::OpenOptions::new().write(true).create_new(true).open(&output).and_then(|mut file| {
        std::io::Write::write_all(&mut file, &out)
    }) {
        eprintln!("mediabox-dvb: {}: {error}", output.display());
        return ExitCode::FAILURE;
    }
    println!(
        "{}",
        serde_json::json!({"selection": selector.selection(), "input": analyser.integrity(), "output_bytes": out.len()})
    );
    if selector.ready() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn analyse(input: PathBuf) -> ExitCode {
    let data = match std::fs::read(&input) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("mediabox-dvb: {}: {error}", input.display());
            return ExitCode::FAILURE;
        }
    };
    let mut analyser = Analyser::new();
    for chunk in data.chunks(153_408) {
        analyser.feed(chunk, |_| {});
    }
    println!(
        "{}",
        serde_json::json!({
            "integrity": analyser.integrity(),
            "cc_error_pids": analyser.cc_error_pids(),
            "programmes": analyser.programmes(),
            // All events, ended ones too: a recording's clock is not today's.
            "guide": mediabox_dvb::service::guide_of(&analyser, None, i64::MIN / 2),
        })
    );
    ExitCode::SUCCESS
}

fn firmware_check(dir: PathBuf) -> ExitCode {
    match mediabox_dvb::firmware::Firmware::load(&dir) {
        Ok(firmware) => {
            println!(
                "{}",
                serde_json::json!({
                    "avl62x1_patch_bytes": firmware.patch.len(),
                    "avl62x1_patch_sha256": mediabox_dvb::sha256::hex(&firmware.patch),
                    "rda5815m_tables": firmware.tables.release,
                    "bridge_firmware_blocks": firmware.bridge_blocks.as_ref().map(Vec::len),
                })
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("mediabox-dvb: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Play(args) => play(*args),
        Command::Select { input, service, output } => select(input, service, output),
        Command::Analyse { input } => analyse(input),
        Command::FirmwareCheck { firmware_dir } => firmware_check(firmware_dir),
        Command::Selftest { firmware_dir, recording, service } => {
            let report = mediabox_dvb::selftest::run(&firmware_dir, recording.as_deref(), service);
            println!("{}", serde_json::to_string(&report).unwrap_or_default());
            if report.ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
    }
}
