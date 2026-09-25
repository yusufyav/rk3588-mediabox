use clap::{Parser, Subcommand};
use mediabox_core::{
    ColorFormat, ColorMode, FanPoint, FanProfile, OutputStatus, Request, ResolutionChoice, Response,
    Surface,
};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Parser)]
#[command(version, about = "MediaBox yerel yönetim aracı")]
struct Args {
    #[arg(long, default_value = "/run/mediabox/mediaboxd.sock")]
    socket: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Status,
    System,
    /// CPU, memory, storage, temperature, DRM, network and encoder state.
    Diagnostics,
    Kodi {
        #[command(subcommand)]
        command: KodiCommand,
    },
    Cec {
        #[command(subcommand)]
        command: CecCommand,
    },
    Media {
        #[command(subcommand)]
        command: MediaCommand,
    },
    Input {
        #[command(subcommand)]
        command: InputCommand,
    },
    /// Which process owns the television.
    Surface {
        #[command(subcommand)]
        command: SurfaceCommand,
    },
    /// Which product owns the television, including after a reboot
    DisplayOwner {
        #[command(subcommand)]
        command: DisplayOwnerCommand,
    },
    /// The display's modes and colour modes, and the one it is driven at
    Display {
        #[command(subcommand)]
        command: DisplayCommand,
    },
    /// The fan as the kernel runs it, and its curve for the next boot
    Fan {
        #[command(subcommand)]
        command: FanCommand,
    },
    /// The wired ports, and an address written by hand on trial
    Ethernet {
        #[command(subcommand)]
        command: EthernetCommand,
    },
}

#[derive(Debug, Subcommand)]
enum EthernetCommand {
    /// Every wired port, what it has, what it is set to, and a change waiting
    Status,
    /// Put an address on a port on trial; it is taken back unless kept
    Set {
        /// The port, as `status` names it
        interface: String,
        /// `dhcp`, or an address with its prefix: `192.168.2.80/24`
        #[arg(value_parser = parse_ethernet)]
        address: EthernetAddress,
        /// The default route's gateway, with an address
        #[arg(long)]
        gateway: Option<std::net::Ipv4Addr>,
        /// A name server, with an address; may be given twice
        #[arg(long)]
        dns: Vec<std::net::Ipv4Addr>,
    },
    /// Keep the address on trial
    Keep,
    /// Take the address on trial back now
    Revert,
}

#[derive(Debug, Clone)]
enum EthernetAddress {
    Dhcp,
    Fixed(std::net::Ipv4Addr, u8),
}

fn parse_ethernet(text: &str) -> Result<EthernetAddress, String> {
    if text.eq_ignore_ascii_case("dhcp") {
        return Ok(EthernetAddress::Dhcp);
    }
    let (address, prefix) = text
        .split_once('/')
        .ok_or("`dhcp` ya da `192.168.2.80/24` biçiminde olmalı")?;
    Ok(EthernetAddress::Fixed(
        address.parse().map_err(|_| format!("{address} bir IPv4 adresi değil"))?,
        prefix.parse().map_err(|_| format!("{prefix} bir ağ öneki değil"))?,
    ))
}

#[derive(Debug, Subcommand)]
enum FanCommand {
    /// SoC temperature, PWM duty and carrier, and the curve
    Status,
    /// Save a curve for the next boot. `custom` takes 2-10 --point T:PWM
    Set {
        #[arg(value_enum)]
        profile: FanProfileArg,
        /// One point of a custom curve, e.g. --point 50:50
        #[arg(long = "point", value_parser = parse_fan_point)]
        points: Vec<FanPoint>,
    },
    /// Go back to the product's default curve from the next boot on
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum FanProfileArg {
    Balanced,
    Cool,
    Custom,
}

impl From<FanProfileArg> for FanProfile {
    fn from(value: FanProfileArg) -> Self {
        match value {
            FanProfileArg::Balanced => FanProfile::Balanced,
            FanProfileArg::Cool => FanProfile::Cool,
            FanProfileArg::Custom => FanProfile::Custom,
        }
    }
}

/// `T:PWM`, both whole numbers. Whether the point is safe is the daemon's
/// question, not this parser's.
fn parse_fan_point(text: &str) -> Result<FanPoint, String> {
    let (temperature, pwm) = text
        .split_once(':')
        .ok_or_else(|| format!("'{text}': SICAKLIK:PWM bekleniyor"))?;
    Ok(FanPoint::new(
        temperature
            .trim()
            .parse()
            .map_err(|_| format!("'{temperature}' sayı değil"))?,
        pwm.trim()
            .parse()
            .map_err(|_| format!("'{pwm}' sayı değil"))?,
    ))
}

#[derive(Debug, Subcommand)]
enum DisplayCommand {
    /// What is on the wire, what was chosen, and a change waiting to be kept
    Status,
    /// Every mode the display lists and what can be sent at each; with a mode,
    /// every colour cell there and why a refused one is refused
    Modes {
        /// `3840x2160p30`, as the list prints it
        mode: Option<String>,
    },
    /// Put a mode on the wire on trial; it is taken back unless kept
    Set {
        /// `auto`, `3840x2160@30`, or a label from the list (`3840x2160p29.97`)
        #[arg(value_parser = parse_mode_name)]
        mode: ModeName,
        /// A colour mode at that mode; left out, `Auto`
        #[arg(long, value_enum)]
        color: Option<ColorFormatArg>,
        /// Bits per component, with --color
        #[arg(long, default_value_t = 8)]
        bits: u8,
    },
    /// Keep the mode on trial
    Keep,
    /// Take the mode on trial back now
    Revert,
}

/// The formats a person may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ColorFormatArg {
    Rgb,
    Ycbcr444,
    Ycbcr422,
    Ycbcr420,
}

impl ColorFormatArg {
    fn mode(self, bits: u8) -> ColorMode {
        let format = match self {
            ColorFormatArg::Rgb => ColorFormat::Rgb,
            ColorFormatArg::Ycbcr444 => ColorFormat::Ycbcr444,
            ColorFormatArg::Ycbcr422 => ColorFormat::Ycbcr422,
            ColorFormatArg::Ycbcr420 => ColorFormat::Ycbcr420,
        };
        ColorMode::new(format, bits)
    }
}

/// A mode as a person types it. It names a mode of the display plugged in
/// only once it is looked up in that display's offer ([`ModeName::resolve`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeName {
    Auto,
    Rate {
        width: u16,
        height: u16,
        refresh_mhz: u32,
        interlaced: bool,
    },
}

/// `auto`, `WxH@Hz`, or a mode label `WxHp59.94` / `WxHi60`.
fn parse_mode_name(text: &str) -> Result<ModeName, String> {
    if text.eq_ignore_ascii_case("auto") {
        return Ok(ModeName::Auto);
    }
    let bad = || format!("'{text}' bir mod değil: auto, 3840x2160@30 ya da 3840x2160p29.97");
    let (width, rest) = text.split_once('x').ok_or_else(bad)?;
    let at = rest.find(['@', 'p', 'i']).ok_or_else(bad)?;
    let (height, rate) = rest.split_at(at);
    let interlaced = rate.starts_with('i');
    let hz: f64 = rate[1..].trim_end_matches("Hz").parse().map_err(|_| bad())?;
    Ok(ModeName::Rate {
        width: width.parse().map_err(|_| bad())?,
        height: height.parse().map_err(|_| bad())?,
        refresh_mhz: (hz * 1000.0).round() as u32,
        interlaced,
    })
}

impl ModeName {
    /// The one mode of `offer` this names: a rate typed to three decimals
    /// matches the mode's own to within five millihertz. None, or more than
    /// one, is an error that lists what there is.
    fn resolve(self, offer: &mediabox_core::OutputOffer) -> Result<ResolutionChoice, String> {
        let ModeName::Rate {
            width,
            height,
            refresh_mhz,
            interlaced,
        } = self
        else {
            return Ok(ResolutionChoice::Auto);
        };
        let matching: Vec<&mediabox_core::OutputModeOffer> = offer
            .modes()
            .filter(|mode| {
                mode.width == width
                    && mode.height == height
                    && mode.interlaced == interlaced
                    && mode.refresh_mhz.abs_diff(refresh_mhz) <= 5
            })
            .collect();
        match matching.as_slice() {
            [mode] => Ok(mode.choice()),
            [] => Err("bu ekran böyle bir mod listelemiyor (mediaboxctl display modes)".into()),
            several => Err(format!(
                "birden fazla mod uyuyor: {}",
                several.iter().map(|mode| mode.label.as_str()).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

#[derive(Debug, Subcommand)]
enum DisplayOwnerCommand {
    Status,
    /// Hand the television over and remember the choice
    Set {
        #[arg(value_enum)]
        owner: DisplayOwnerArg,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DisplayOwnerArg {
    Mediabox,
    Screenbridge,
}

impl DisplayOwnerArg {
    fn as_str(self) -> &'static str {
        match self {
            DisplayOwnerArg::Mediabox => "mediabox",
            DisplayOwnerArg::Screenbridge => "screenbridge",
        }
    }
}

#[derive(Debug, Subcommand)]
enum SurfaceCommand {
    Status,
    /// Hand the display to Kodi, to the product UI, or to nobody.
    Switch {
        #[arg(value_enum)]
        target: SurfaceArg,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SurfaceArg {
    Kodi,
    Ui,
    Idle,
}

impl From<SurfaceArg> for Surface {
    fn from(value: SurfaceArg) -> Self {
        match value {
            SurfaceArg::Kodi => Surface::Kodi,
            SurfaceArg::Ui => Surface::Ui,
            SurfaceArg::Idle => Surface::Idle,
        }
    }
}

#[derive(Debug, Subcommand)]
enum KodiCommand {
    Status,
    PlayPause,
    Stop,
    Seek {
        // Seeking backwards is `seek -30`, and without this clap reads the
        // leading minus as the start of an option.
        #[arg(allow_hyphen_values = true)]
        seconds: i64,
    },
    Open {
        url: String,
        #[arg(long, default_value_t = 0)]
        resume: u64,
    },
    Restart,
}

#[derive(Debug, Subcommand)]
enum CecCommand {
    Status,
    Devices,
    ActiveSource,
    WakeTv,
    StandbyTv,
}

#[derive(Debug, Subcommand)]
enum InputCommand {
    Monitor,
}

#[derive(Debug, Subcommand)]
enum MediaCommand {
    Status,
    Capabilities,
    Home,
    Catalog {
        media_type: String,
        id: String,
        #[arg(long)]
        addon: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
    },
    Meta {
        media_type: String,
        id: String,
    },
    Subtitles {
        media_type: String,
        id: String,
        #[arg(long)]
        video_id: Option<String>,
    },
    Library,
    LibraryItem {
        id: String,
    },
    /// Create a media session and put it on the television.
    Play {
        url: String,
        #[arg(long, default_value_t = 0)]
        start: u64,
    },
    Search {
        query: String,
    },
    Inspect {
        url: String,
    },
    Streams {
        media_type: String,
        id: String,
    },
    Policy {
        url: String,
    },
    Sessions,
    SessionStart {
        url: String,
    },
    SessionStop {
        id: String,
    },
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let mut request = to_request(&args.command);
    if let Command::Display { command } = &args.command {
        match display_request(&args.socket, command).await {
            Ok(Some(resolved)) => request = resolved,
            Ok(None) => {}
            Err(message) => {
                eprintln!("Hata: {message}");
                std::process::exit(1);
            }
        }
    }
    let monitor = matches!(request, Request::InputMonitor);
    if monitor {
        if let Err(error) = execute_monitor(&args.socket, args.json).await {
            eprintln!(
                "mediaboxd kullanılamıyor ({}): {error}",
                args.socket.display()
            );
            std::process::exit(2);
        }
        return;
    }
    match execute(&args.socket, request).await {
        Ok(lines) => {
            for response in lines {
                let shown = match &args.command {
                    Command::Display { command } if !args.json => {
                        let detail = match command {
                            DisplayCommand::Modes { mode } => Some(mode.as_deref().unwrap_or("")),
                            _ => None,
                        };
                        Some(print_output(&response, detail))
                    }
                    _ => None,
                };
                if let Some(result) = shown {
                    if let Err(message) = result {
                        eprintln!("Hata: {message}");
                        std::process::exit(1);
                    }
                } else if args.json {
                    println!("{}", serde_json::to_string(&response).expect("JSON"));
                } else if let Err(message) = print_human(&response) {
                    eprintln!("Hata: {message}");
                    std::process::exit(1);
                }
            }
        }
        Err(error) => {
            eprintln!(
                "mediaboxd kullanılamıyor ({}): {error}",
                args.socket.display()
            );
            std::process::exit(2);
        }
    }
}

fn to_request(command: &Command) -> Request {
    match command {
        Command::Status => Request::Status,
        Command::System => Request::System,
        Command::Diagnostics => Request::Diagnostics,
        Command::Kodi { command } => match command {
            KodiCommand::Status => Request::KodiStatus,
            KodiCommand::PlayPause => Request::KodiPlayPause,
            KodiCommand::Stop => Request::KodiStop,
            KodiCommand::Seek { seconds } => Request::KodiSeek { seconds: *seconds },
            KodiCommand::Open { url, resume } => Request::KodiOpen {
                url: url.clone(),
                resume_seconds: *resume,
            },
            KodiCommand::Restart => Request::KodiRestart,
        },
        Command::Cec { command } => match command {
            CecCommand::Status => Request::CecStatus,
            CecCommand::Devices => Request::CecDevices,
            CecCommand::ActiveSource => Request::CecActiveSource,
            CecCommand::WakeTv => Request::CecWakeTv,
            CecCommand::StandbyTv => Request::CecStandbyTv,
        },
        Command::Surface { command } => match command {
            SurfaceCommand::Status => Request::SurfaceStatus,
            SurfaceCommand::Switch { target } => Request::SurfaceSwitch {
                target: (*target).into(),
            },
        },
        Command::Display { command } => match command {
            // Named against the display's own offer, and a trial by its id:
            // both are read from the daemon first (`display_request`).
            DisplayCommand::Status
            | DisplayCommand::Modes { .. }
            | DisplayCommand::Set { .. }
            | DisplayCommand::Keep
            | DisplayCommand::Revert => Request::OutputStatus,
        },
        Command::Ethernet { command } => match command {
            EthernetCommand::Status => Request::EthernetStatus,
            EthernetCommand::Set { interface, address, gateway, dns } => Request::EthernetTry {
                interface: interface.clone(),
                config: match address {
                    EthernetAddress::Dhcp => mediabox_core::EthernetConfig::Dhcp,
                    EthernetAddress::Fixed(address, prefix) => mediabox_core::EthernetConfig::Static {
                        address: *address,
                        prefix: *prefix,
                        gateway: *gateway,
                        dns: dns.clone(),
                    },
                },
            },
            EthernetCommand::Keep => Request::EthernetKeep,
            EthernetCommand::Revert => Request::EthernetRevert,
        },
        Command::Fan { command } => match command {
            FanCommand::Status => Request::FanStatus,
            FanCommand::Set { profile, points } => Request::FanCurveSet {
                profile: (*profile).into(),
                points: (!points.is_empty()).then(|| points.clone()),
            },
            FanCommand::Reset => Request::FanCurveReset,
        },
        Command::DisplayOwner { command } => match command {
            DisplayOwnerCommand::Status => Request::DisplayOwner,
            DisplayOwnerCommand::Set { owner } => Request::DisplayOwnerSet {
                owner: owner.as_str().to_string(),
            },
        },
        Command::Media { command } => match command {
            MediaCommand::Status => Request::MediaStatus,
            MediaCommand::Capabilities => Request::MediaCapabilities,
            MediaCommand::Home => Request::MediaHome,
            MediaCommand::Catalog {
                media_type,
                id,
                addon,
                limit,
            } => Request::MediaCatalog {
                media_type: media_type.clone(),
                id: id.clone(),
                addon_id: addon.clone(),
                limit: *limit,
            },
            MediaCommand::Meta { media_type, id } => Request::MediaMeta {
                media_type: media_type.clone(),
                id: id.clone(),
            },
            MediaCommand::Subtitles {
                media_type,
                id,
                video_id,
            } => Request::MediaSubtitles {
                media_type: media_type.clone(),
                id: id.clone(),
                video_id: video_id.clone(),
            },
            MediaCommand::Library => Request::MediaLibrary,
            MediaCommand::LibraryItem { id } => Request::MediaLibraryItem { id: id.clone() },
            MediaCommand::Play { url, start } => Request::MediaPlayOnKodi {
                url: Some(url.clone()),
                stream: None,
                start_seconds: *start,
            },
            MediaCommand::Search { query } => Request::MediaSearch {
                query: query.clone(),
            },
            MediaCommand::Inspect { url } => Request::MediaInspect { url: url.clone() },
            MediaCommand::Streams { media_type, id } => Request::MediaStreams {
                media_type: media_type.clone(),
                id: id.clone(),
            },
            MediaCommand::Policy { url } => Request::MediaPolicy { url: url.clone() },
            MediaCommand::Sessions => Request::MediaSessions,
            MediaCommand::SessionStart { url } => Request::MediaSessionStart { url: url.clone() },
            MediaCommand::SessionStop { id } => Request::MediaSessionStop { id: id.clone() },
        },
        Command::Input {
            command: InputCommand::Monitor,
        } => Request::InputMonitor,
    }
}

/// A display command that names something of the daemon's -- a mode of the
/// display plugged in, the trial running now -- as the request that names it.
async fn display_request(path: &PathBuf, command: &DisplayCommand) -> Result<Option<Request>, String> {
    if matches!(command, DisplayCommand::Status | DisplayCommand::Modes { .. }) {
        return Ok(None);
    }
    let answer = execute(path, Request::OutputStatus)
        .await
        .map_err(|error| format!("mediaboxd kullanılamıyor: {error}"))?;
    let status: OutputStatus = answer
        .into_iter()
        .next()
        .and_then(|value| value.get("result").cloned())
        .and_then(|result| serde_json::from_value(result).ok())
        .ok_or("daemon ekran durumunu vermedi")?;
    let trial = || {
        status
            .trial
            .as_ref()
            .map(|trial| trial.id)
            .ok_or("onay bekleyen bir ayar yok")
    };
    Ok(Some(match command {
        DisplayCommand::Set { mode, color, bits } => {
            let offer = status
                .offer
                .as_ref()
                .ok_or_else(|| status.error.clone().unwrap_or("ekran bilinmiyor".into()))?;
            Request::OutputTry {
                resolution: mode.resolve(offer)?,
                colour: color.map(|format| format.mode(*bits)),
            }
        }
        DisplayCommand::Keep => Request::OutputKeep { trial: trial()? },
        DisplayCommand::Revert => Request::OutputRevert { trial: trial()? },
        DisplayCommand::Status | DisplayCommand::Modes { .. } => return Ok(None),
    }))
}

async fn execute(
    path: &PathBuf,
    request: Request,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let stream = UnixStream::connect(path).await?;
    let (read, mut write) = stream.into_split();
    write
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .await?;
    write.shutdown().await?;
    let mut reader = BufReader::new(read).lines();
    let Some(line) = reader.next_line().await? else {
        return Err("daemon yanıt vermeden bağlantıyı kapattı".into());
    };
    Ok(vec![serde_json::from_str(&line)?])
}

async fn execute_monitor(path: &PathBuf, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let stream = UnixStream::connect(path).await?;
    let (read, mut write) = stream.into_split();
    write
        .write_all(format!("{}\n", serde_json::to_string(&Request::InputMonitor)?).as_bytes())
        .await?;
    write.shutdown().await?;
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let value: Value = serde_json::from_str(&line)?;
        if json {
            println!("{}", serde_json::to_string(&value)?);
        } else {
            print_human(&value).map_err(std::io::Error::other)?;
        }
    }
    Ok(())
}

/// The display, as a person reads it. `detail` is `None` for the summary,
/// `Some("")` for every mode, `Some(label)` for one mode's every cell.
fn print_output(value: &Value, detail: Option<&str>) -> Result<(), String> {
    let response: Response = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    if !response.ok {
        return Err(response
            .error
            .map(|e| e.message)
            .unwrap_or_else(|| "bilinmeyen daemon hatası".into()));
    }
    let status: OutputStatus =
        serde_json::from_value(response.result.unwrap_or(Value::Null)).map_err(|e| e.to_string())?;
    let Some(offer) = &status.offer else {
        println!("Ekran: {}", status.error.as_deref().unwrap_or("bilinmiyor"));
        return Ok(());
    };
    let link = &offer.link;
    println!(
        "Ekran: {} · {} · {} MHz{}",
        offer.sink_name.as_deref().unwrap_or("?"),
        offer.connector,
        link.max_character_rate_khz / 1000,
        if link.declared_by.is_empty() {
            " (bildirilmedi)".to_string()
        } else {
            format!(" ({})", link.declared_by)
        }
    );
    // What the driver reports, and only that, as "now": a value somebody
    // asked for is not the wire.
    if let Some(observed) = &status.observed {
        let known = |value: &mediabox_core::Observed<String>| match value {
            mediabox_core::Observed::Known(value) => value.clone(),
            mediabox_core::Observed::Unknown(_) => "bilinmiyor".into(),
        };
        println!(
            "Sürücü: {} · {}{}",
            known(&observed.mode),
            observed.colour.map(|mode| mode.label()).unwrap_or_else(|| known(&observed.bus_format)),
            match &observed.phy_clock_khz {
                mediabox_core::Observed::Known(khz) => format!(" · PHY {} kHz", khz),
                mediabox_core::Observed::Unknown(_) => String::new(),
            }
        );
    }
    if let Some(applied) = &status.applied {
        println!(
            "Uygulanan (DRM): {} · {}{}",
            applied.label,
            applied.colour.map(|mode| mode.label()).unwrap_or_else(|| "sürücünün seçimi".into()),
            if applied.hdr { " · HDR10" } else { "" }
        );
    }
    if let Some(display) = &status.display {
        println!(
            "Ses: {} · CEC: {}",
            display.audio.device.as_deref().or(display.audio.refused.as_deref()).unwrap_or("-"),
            display.cec.device.as_deref().or(display.cec.refused.as_deref()).unwrap_or("-")
        );
    }
    for note in &status.notes {
        println!("Not: {note}");
    }
    let resolved = offer.resolve(status.setting.resolution);
    println!(
        "Seçim: {}{}",
        match status.setting.resolution {
            ResolutionChoice::Auto => "Otomatik".to_string(),
            _ => resolved.map(|mode| mode.label.clone()).unwrap_or_default(),
        },
        match resolved.and_then(|mode| status.setting.colours.get(&mode.timing_key)) {
            Some(colour) => format!(" · {}", colour.label()),
            None => " · renk otomatik".into(),
        }
    );
    if let Some(trial) = &status.trial {
        println!(
            "Deneme {}: {} sn içinde onaylanmazsa geri alınır{} (mediaboxctl display keep | revert)",
            trial.id,
            trial.seconds_left,
            if trial.applied { "" } else { "; ekran henüz uygulamadı" }
        );
    }
    let Some(detail) = detail else { return Ok(()) };
    if !detail.is_empty() {
        let mode = offer
            .mode(detail)
            .ok_or_else(|| format!("{detail} bu ekranın listesinde yok"))?;
        for cell in &mode.cells {
            match cell.refused {
                None => println!(
                    "  {:<16} {:>4} MHz  gönderilebilir{}",
                    cell.mode.label(),
                    cell.rate_khz / 1000,
                    if cell.hdr10 { " · HDR10" } else { "" }
                ),
                Some(refusal) => println!(
                    "  {:<16} {:>4} MHz  olmaz: {}",
                    cell.mode.label(),
                    cell.rate_khz / 1000,
                    refusal.text(cell.mode.format)
                ),
            }
        }
        return Ok(());
    }
    for group in &offer.groups {
        println!(
            "{}x{}{}{}",
            group.width,
            group.height,
            if group.name.is_empty() { String::new() } else { format!(" · {}", group.name) },
            if group.computer { " · bilgisayar modu" } else { "" }
        );
        for mode in &group.modes {
            let allowed: Vec<String> = mode.allowed().map(|mode| mode.label()).collect();
            println!(
                "  {:<18} {}{}{}",
                mode.label,
                if allowed.is_empty() { "sığmaz".to_string() } else { allowed.join(", ") },
                if mode.hdr10() { " · HDR10" } else { "" },
                if mode.label == offer.auto { " · otomatik" } else { "" }
            );
        }
    }
    Ok(())
}

fn print_human(value: &Value) -> Result<(), String> {
    let mut out = Vec::new();
    render(value, &mut out)?;
    std::io::stdout()
        .write_all(&out)
        .map_err(|error| error.to_string())
}

/// Render one daemon response as text.
///
/// Dispatch is on fields that identify a payload uniquely. `available` does not:
/// the CEC adapter and the media worker both report it, which is how
/// `media status` used to come out as an empty CEC adapter listing. Every arm
/// below keys on something only its own payload has.
fn render(value: &Value, out: &mut impl Write) -> Result<(), String> {
    let write = |out: &mut dyn Write, line: String| -> Result<(), String> {
        writeln!(out, "{line}").map_err(|error| error.to_string())
    };

    if value.get("ok").is_none() {
        return write(
            out,
            format!(
                "Input: {} {} ({})",
                value.get("action").and_then(Value::as_str).unwrap_or("?"),
                if value.get("pressed").and_then(Value::as_bool) == Some(true) {
                    "basıldı"
                } else {
                    "bırakıldı"
                },
                value.get("source").and_then(Value::as_str).unwrap_or("?")
            ),
        );
    }

    let response: Response = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    if !response.ok {
        return Err(response
            .error
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| "bilinmeyen daemon hatası".into()));
    }
    let result = response.result.unwrap_or(Value::Null);

    if let Some(hostname) = result.get("hostname").and_then(Value::as_str) {
        write(
            out,
            format!(
                "Sistem: {hostname} ({})",
                result
                    .get("architecture")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
            ),
        )?;
        if let Some(uptime) = result.get("uptime_seconds").and_then(Value::as_u64) {
            write(out, format!("Çalışma süresi: {uptime} saniye"))?;
        }
        if let Some(kodi) = result.get("kodi") {
            write(
                out,
                format!(
                    "Kodi: {}",
                    kodi.get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("bilinmiyor")
                ),
            )?;
        }
        if let Some(cec) = result.get("cec") {
            write(
                out,
                format!(
                    "CEC: {}",
                    if cec.get("available").and_then(Value::as_bool) == Some(true) {
                        "hazır"
                    } else {
                        "kullanılamıyor"
                    }
                ),
            )?;
        }
        if let Some(media) = result.get("media") {
            render_media_status(media, out, &write)?;
        }
        if let Some(surface) = result.get("surface") {
            render_surface(surface, out, &write)?;
        }
        return Ok(());
    }

    // The media worker's own status document. `CecStatus` always serializes
    // `logical_addresses`, so its absence next to `available` is what tells the
    // two apart — including the bare {available:false, error} the daemon
    // synthesizes when the worker is down.
    if result.get("provider").is_some()
        || result.get("capabilityProfile").is_some()
        || (result.get("available").is_some() && result.get("logical_addresses").is_none())
    {
        return render_media_status(&result, out, &write);
    }

    if result.get("active").is_some() && result.get("kodi_active").is_some() {
        return render_surface(&result, out, &write);
    }

    // Which product the board comes up as. `owner` beside the file it was read
    // from is the shape nothing else has.
    if let (Some(owner), Some(file)) = (
        result.get("owner").and_then(Value::as_str),
        result.get("file").and_then(Value::as_str),
    ) {
        write(
            out,
            format!(
                "Ekran sahibi: {}",
                match owner {
                    "mediabox" => "MediaBox",
                    "screenbridge" => "ScreenBridge",
                    other => other,
                }
            ),
        )?;
        write(out, format!("  tercih: {file}"))?;
        write(
            out,
            format!(
                "  şu an: MediaBox {}, ScreenBridge {}",
                if result.get("mediabox_on_display").and_then(Value::as_bool) == Some(true) {
                    "ekranda"
                } else {
                    "ekranda değil"
                },
                if result.get("screenbridge_active").and_then(Value::as_bool) == Some(true) {
                    "çalışıyor"
                } else {
                    "durmuş"
                }
            ),
        )?;
        return Ok(());
    }

    // The CEC adapter: `logical_addresses` is the field nothing else carries.
    if result.get("logical_addresses").is_some() {
        write(
            out,
            format!(
                "CEC adaptörü: {}",
                result
                    .get("adapter")
                    .and_then(Value::as_str)
                    .unwrap_or("yok")
            ),
        )?;
        write(
            out,
            format!(
                "Fiziksel adres: {}",
                result
                    .get("physical_address")
                    .and_then(Value::as_str)
                    .unwrap_or("yok")
            ),
        )?;
        write(
            out,
            format!(
                "Mantıksal adresler: {}",
                result.get("logical_addresses").unwrap_or(&Value::Null)
            ),
        )?;
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            write(out, format!("CEC hatası: {error}"))?;
        }
        return Ok(());
    }

    if result.get("state").is_some() && result.get("jsonrpc_reachable").is_some() {
        return write(
            out,
            format!(
                "Kodi: {} (JSON-RPC: {})",
                result.get("state").and_then(Value::as_str).unwrap_or("?"),
                if result.get("jsonrpc_reachable").and_then(Value::as_bool) == Some(true) {
                    "erişilebilir"
                } else {
                    "erişilemiyor"
                }
            ),
        );
    }

    if let Some(entries) = result.as_array() {
        if entries.is_empty() {
            return write(out, "Cihaz bulunamadı.".into());
        }
        for item in entries {
            write(
                out,
                format!(
                    "CEC {}: fiziksel={} tür={}",
                    item.get("logical_address").unwrap_or(&Value::Null),
                    item.get("physical_address")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    item.get("device_type")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                ),
            )?;
        }
        return Ok(());
    }

    write(
        out,
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?,
    )
}

fn render_media_status(
    media: &Value,
    out: &mut impl Write,
    write: &impl Fn(&mut dyn Write, String) -> Result<(), String>,
) -> Result<(), String> {
    write(
        out,
        format!(
            "Medya: {}",
            if media.get("available").and_then(Value::as_bool) == Some(false) {
                "kullanılamıyor"
            } else {
                "hazır"
            }
        ),
    )?;
    if let Some(profile) = media.get("capabilityProfile").and_then(Value::as_str) {
        write(out, format!("Yetenek profili: {profile}"))?;
    }
    if let Some(provider) = media.get("provider") {
        write(
            out,
            format!(
                "Sağlayıcı: {} eklenti, oturum {}, akış sunucusu {}",
                provider
                    .get("addonCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                if provider.get("authenticated").and_then(Value::as_bool) == Some(true) {
                    "açık"
                } else {
                    "yok"
                },
                if provider
                    .pointer("/streamingServer/reachable")
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    "erişilebilir"
                } else {
                    "erişilemiyor"
                }
            ),
        )?;
    }
    if let Some(sessions) = media.get("sessions").and_then(Value::as_u64) {
        write(out, format!("Etkin oturum: {sessions}"))?;
    }
    if let Some(torrent) = media
        .pointer("/torrentNetwork/status")
        .and_then(Value::as_str)
    {
        write(out, format!("Torrent ağı: {torrent}"))?;
    }
    if let Some(error) = media.pointer("/error/message").and_then(Value::as_str) {
        write(out, format!("Medya hatası: {error}"))?;
    }
    Ok(())
}

fn render_surface(
    surface: &Value,
    out: &mut impl Write,
    write: &impl Fn(&mut dyn Write, String) -> Result<(), String>,
) -> Result<(), String> {
    write(
        out,
        format!(
            "Ekran: {}",
            match surface.get("active").and_then(Value::as_str) {
                Some("kodi") => "Kodi",
                Some("ui") => "MediaBox arayüzü",
                Some("idle") => "boşta",
                _ => "bilinmiyor",
            }
        ),
    )?;
    if surface.get("ui_installed").and_then(Value::as_bool) == Some(false) {
        write(out, "TV-local arayüz bu cihazda kurulu değil.".into())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ModeName, parse_mode_name};

    #[test]
    fn a_mode_is_named_the_way_the_list_prints_it_or_as_a_rate() {
        let uhd = |refresh_mhz, interlaced| ModeName::Rate {
            width: 3840,
            height: 2160,
            refresh_mhz,
            interlaced,
        };
        assert_eq!(parse_mode_name("auto"), Ok(ModeName::Auto));
        assert_eq!(parse_mode_name("3840x2160@30"), Ok(uhd(30_000, false)));
        assert_eq!(parse_mode_name("3840x2160p29.97"), Ok(uhd(29_970, false)));
        assert_eq!(parse_mode_name("3840x2160p23.976"), Ok(uhd(23_976, false)));
        assert_eq!(parse_mode_name("3840x2160i60"), Ok(uhd(60_000, true)));
        assert!(parse_mode_name("4k").is_err());
    }

    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn json_mode_is_machine_readable() {
        let response = Response::success(serde_json::json!({"value":1}));
        let text = serde_json::to_string(&response).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[tokio::test]
    async fn unavailable_daemon_is_explicit_error() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(format!("/tmp/mediaboxctl-missing-{nonce}.sock"));
        let error = execute(&path, Request::Status)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.is_empty());
    }

    fn rendered(value: &Value) -> String {
        let mut out = Vec::new();
        render(value, &mut out).expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn media_status_is_not_rendered_as_a_cec_adapter() {
        // The regression: this payload carries `available`, which used to be
        // enough to send it down the CEC branch and print an empty adapter.
        let response = Response::success(serde_json::json!({
            "available": true,
            "provider": {
                "authenticated": false,
                "addonCount": 7,
                "streamingServer": {"reachable": true, "version": "4.21.0"}
            },
            "capabilityProfile": "rk3588_orangepi5_production",
            "sessions": 0,
            "torrentNetwork": {"status": "TORRENT_NETWORK_BLOCKED"}
        }));
        let text = rendered(&serde_json::to_value(&response).unwrap());
        assert!(text.contains("Medya: hazır"), "{text}");
        assert!(text.contains("rk3588_orangepi5_production"), "{text}");
        assert!(text.contains("7 eklenti"), "{text}");
        assert!(text.contains("TORRENT_NETWORK_BLOCKED"), "{text}");
        assert!(!text.contains("CEC adaptörü"), "{text}");
        assert!(!text.contains("Fiziksel adres"), "{text}");
    }

    #[test]
    fn cec_status_still_renders_as_a_cec_adapter() {
        let response = Response::success(serde_json::json!({
            "available": true,
            "adapter": "/dev/cec0",
            "physical_address": "2.0.0.0",
            "logical_addresses": [4],
            "known_devices": [],
        }));
        let text = rendered(&serde_json::to_value(&response).unwrap());
        assert!(text.contains("CEC adaptörü: /dev/cec0"), "{text}");
        assert!(text.contains("Fiziksel adres: 2.0.0.0"), "{text}");
        assert!(!text.contains("Medya:"), "{text}");
    }

    #[test]
    fn an_unavailable_media_worker_says_so() {
        let response = Response::success(serde_json::json!({
            "available": false,
            "error": {"code": "MEDIA_WORKER_UNAVAILABLE", "message": "bağlantı yok"}
        }));
        let text = rendered(&serde_json::to_value(&response).unwrap());
        assert!(text.contains("Medya: kullanılamıyor"), "{text}");
        assert!(text.contains("bağlantı yok"), "{text}");
    }

    #[test]
    fn surface_status_names_the_owner_of_the_display() {
        let response = Response::success(serde_json::json!({
            "active": "ui",
            "kodi_active": false,
            "ui_active": true,
            "ui_installed": true,
        }));
        let text = rendered(&serde_json::to_value(&response).unwrap());
        assert!(text.contains("Ekran: MediaBox arayüzü"), "{text}");
    }

    #[test]
    fn json_output_is_unchanged_by_the_formatter_fix() {
        // `--json` prints the response verbatim; the human renderer never runs.
        let response = Response::success(serde_json::json!({"available": true, "provider": {}}));
        let text = serde_json::to_string(&response).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["result"]["available"], true);
        assert!(parsed["result"].get("provider").is_some());
    }

    #[test]
    fn cli_maps_only_typed_commands() {
        assert_eq!(to_request(&Command::Status), Request::Status);
        assert_eq!(
            to_request(&Command::Kodi {
                command: KodiCommand::Seek { seconds: 30 }
            }),
            Request::KodiSeek { seconds: 30 }
        );
        assert_eq!(
            to_request(&Command::Media {
                command: MediaCommand::Search {
                    query: "Dune".into()
                }
            }),
            Request::MediaSearch {
                query: "Dune".into()
            }
        );
    }
}
