//! One log for a receiver session: the journal (stderr) and, optionally, a
//! file that also keeps every USB frame -- the record `tests/replay.rs` reads.

use std::io::Write;
use std::sync::Mutex;

pub struct Log {
    file: Mutex<Option<std::fs::File>>,
    start: std::time::Instant,
    /// USB frames go to the file only: on the console they drown the rest.
    pub quiet_usb: bool,
    /// Nothing on stderr (tests).
    pub silent: bool,
}

impl Log {
    pub fn new(path: Option<&std::path::Path>) -> std::io::Result<Self> {
        let file = match path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Some(std::fs::File::create(path)?)
            }
            None => None,
        };
        Ok(Self { file: Mutex::new(file), start: std::time::Instant::now(), quiet_usb: true, silent: false })
    }

    pub fn silent() -> Self {
        Self { file: Mutex::new(None), start: std::time::Instant::now(), quiet_usb: true, silent: true }
    }

    pub fn line(&self, message: impl AsRef<str>) {
        let message = message.as_ref();
        if !self.silent && !(self.quiet_usb && message.starts_with("  USB")) {
            eprintln!("mediabox-dvb: {message}");
        }
        if let Some(file) = self.file.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            let at = self.start.elapsed();
            let _ = writeln!(file, "{:>8.3} {message}", at.as_secs_f64());
        }
    }
}
