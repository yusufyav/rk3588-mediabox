use clap::{Parser, Subcommand};
use mediabox_core::{Request, Response};
use serde_json::Value;
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
}

#[derive(Debug, Subcommand)]
enum KodiCommand {
    Status,
    PlayPause,
    Stop,
    Seek {
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
    Search { query: String },
    Inspect { url: String },
    Streams { media_type: String, id: String },
    Policy { url: String },
    Sessions,
    SessionStart { url: String },
    SessionStop { id: String },
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
        Command::Media { command } => match command {
            MediaCommand::Status => Request::MediaStatus,
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
    if value.get("ok").is_some() {
        let response: Response =
            serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if !response.ok {
            let error = response
                .error
                .map(|e| format!("{}: {}", e.code, e.message))
                .unwrap_or_else(|| "bilinmeyen daemon hatası".into());
            return Err(error);
        }
        let result = response.result.unwrap_or(Value::Null);
        if let Some(hostname) = result.get("hostname").and_then(Value::as_str) {
            println!(
                "Sistem: {hostname} ({})",
                result
                    .get("architecture")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
            );
            if let Some(uptime) = result.get("uptime_seconds").and_then(Value::as_u64) {
                println!("Çalışma süresi: {uptime} saniye");
            }
            if let Some(kodi) = result.get("kodi") {
                println!(
                    "Kodi: {}",
                    kodi.get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("bilinmiyor")
                );
            }
            if let Some(cec) = result.get("cec") {
                println!(
                    "CEC: {}",
                    if cec.get("available").and_then(Value::as_bool) == Some(true) {
                        "hazır"
                    } else {
                        "kullanılamıyor"
                    }
                );
            }
            if let Some(media) = result.get("media") {
                println!(
                    "Medya: {}",
                    if media.get("available").and_then(Value::as_bool) == Some(false) {
                        "kullanılamıyor"
                    } else {
                        "hazır"
                    }
                );
                if let Some(torrent) = media
                    .pointer("/torrentNetwork/status")
                    .and_then(Value::as_str)
                {
                    println!("Torrent ağı: {torrent}");
                }
            }
        } else if result.get("available").is_some() {
            println!(
                "CEC adaptörü: {}",
                result
                    .get("adapter")
                    .and_then(Value::as_str)
                    .unwrap_or("yok")
            );
            println!(
                "Fiziksel adres: {}",
                result
                    .get("physical_address")
                    .and_then(Value::as_str)
                    .unwrap_or("yok")
            );
            println!(
                "Mantıksal adresler: {}",
                result.get("logical_addresses").unwrap_or(&Value::Null)
            );
            if let Some(error) = result.get("error").and_then(Value::as_str) {
                println!("CEC hatası: {error}");
            }
        } else if result.get("state").is_some() && result.get("jsonrpc_reachable").is_some() {
            println!(
                "Kodi: {} (JSON-RPC: {})",
                result.get("state").and_then(Value::as_str).unwrap_or("?"),
                if result.get("jsonrpc_reachable").and_then(Value::as_bool) == Some(true) {
                    "erişilebilir"
                } else {
                    "erişilemiyor"
                }
            );
        } else if result.is_array() {
            let entries = result.as_array().expect("array");
            if entries.is_empty() {
                println!("Cihaz bulunamadı.");
            }
            for item in entries {
                println!(
                    "CEC {}: fiziksel={} tür={}",
                    item.get("logical_address").unwrap_or(&Value::Null),
                    item.get("physical_address")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    item.get("device_type")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                );
            }
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
        }
    } else {
        println!(
            "Input: {} {} ({})",
            value.get("action").and_then(Value::as_str).unwrap_or("?"),
            if value.get("pressed").and_then(Value::as_bool) == Some(true) {
                "basıldı"
            } else {
                "bırakıldı"
            },
            value.get("source").and_then(Value::as_str).unwrap_or("?")
        );
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
