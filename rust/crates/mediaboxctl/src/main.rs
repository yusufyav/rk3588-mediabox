use clap::{Parser, Subcommand};
use mediabox_core::{Request, Response, Surface};
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
    let request = to_request(&args.command);
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
                if args.json {
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
