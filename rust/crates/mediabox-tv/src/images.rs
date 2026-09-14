//! Artwork, without a browser to do it for us.
//!
//! Posters and backdrops are absolute URLs on third-party hosts — the media
//! core passes through whatever the addon said and there is no image proxy
//! anywhere in the stack. So this process fetches, caches, decodes and sizes
//! them itself, and the one rule it keeps above all others is that none of that
//! happens on the thread that draws.
//!
//! Three budgets, all in bytes rather than in pictures. A poster at 480x720 is
//! 1.4 MB decoded and a backdrop at 1920x1080 is 8.3 MB, so a limit counted in
//! images would mean something different depending on which screen the remote
//! was on:
//!
//!   * on disk, the bytes as downloaded, evicted by age
//!   * in memory, decoded pixels, evicted least-recently-used
//!   * on the GPU, only what a screen is showing or is about to
//!
//! The third is not enforced here. It is enforced by the caller only ever
//! handing a decoded image to the shelf items inside the visible window, which
//! is what keeps the whole catalogue out of texture memory.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use sha2::{Digest, Sha256};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Decoded pixels held in this process. Chosen against the appliance's 8 GB and
/// the interface's own resident size; the metrics line reports what is actually
/// used so it can be moved with evidence rather than by feel.
const CPU_BUDGET_BYTES: usize = 160 * 1024 * 1024;

/// Downloaded bytes kept between runs, so a restart after a Kodi handover is
/// not a fresh download of every shelf.
const DISK_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// How many hosts we are willing to be waiting on at once. Higher does not fill
/// shelves faster — the panel only has so many posters on it — and it does cost
/// sockets on a box that is also streaming.
const IN_FLIGHT: usize = 6;

/// What a picture is wanted for. The width is part of the identity: the same
/// URL fetched for a shelf and for a backdrop is decoded twice, at two sizes,
/// and neither is the full-resolution original.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub url: String,
    pub width: u32,
}

impl Key {
    pub fn new(url: impl Into<String>, width: u32) -> Self {
        Self { url: url.into(), width }
    }
}

enum Done {
    Ready(Key, SharedPixelBuffer<Rgba8Pixel>),
    /// Kept so a poster that 404s is asked for once rather than on every frame
    /// the shelf is redrawn. Addon artwork does 404, regularly.
    Failed(Key),
}

pub struct ImageManager {
    wanted: Sender<Key>,
    done: Receiver<Done>,

    cache: HashMap<Key, Entry>,
    /// Requested and not yet answered, so the same picture is not queued twice.
    pending: HashMap<Key, ()>,
    failed: HashMap<Key, ()>,

    bytes: usize,
    tick: u64,
}

struct Entry {
    buffer: SharedPixelBuffer<Rgba8Pixel>,
    bytes: usize,
    used: u64,
}

impl ImageManager {
    /// `poke` is called once per finished picture, from the fetching thread.
    /// It exists so nothing has to poll for arrivals: an interface that woke up
    /// ten times a second to ask would show up in the idle CPU figure it is
    /// there to keep honest.
    pub fn new(cache_dir: impl AsRef<Path>, poke: impl Fn() + Send + Sync + 'static) -> Self {
        let dir = cache_dir.as_ref().to_path_buf();
        let _ = std::fs::create_dir_all(&dir);

        let (wanted_tx, wanted_rx) = std::sync::mpsc::channel::<Key>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Done>();

        std::thread::Builder::new()
            .name("mediabox-tv-art".into())
            .spawn(move || worker(dir, wanted_rx, done_tx, std::sync::Arc::new(poke)))
            .expect("the artwork thread could not be started");

        Self {
            wanted: wanted_tx,
            done: done_rx,
            cache: HashMap::new(),
            pending: HashMap::new(),
            failed: HashMap::new(),
            bytes: 0,
            tick: 0,
        }
    }

    /// Ask for a picture. Cheap to call for every item in a window on every
    /// move: anything already held, already asked for or already known to be
    /// missing costs a hash lookup.
    pub fn want(&mut self, key: &Key) {
        if key.url.is_empty()
            || self.cache.contains_key(key)
            || self.pending.contains_key(key)
            || self.failed.contains_key(key)
        {
            return;
        }
        self.pending.insert(key.clone(), ());
        let _ = self.wanted.send(key.clone());
    }

    /// The picture, if it is here. Marks it as used, which is what keeps the
    /// shelf the remote is on out of the eviction queue.
    pub fn get(&mut self, key: &Key) -> Option<slint::Image> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.cache.get_mut(key)?;
        entry.used = tick;
        Some(slint::Image::from_rgba8(entry.buffer.clone()))
    }

    /// Moves whatever the worker finished into the cache. Returns true if
    /// anything arrived, which is the caller's signal to repaint the window.
    pub fn collect(&mut self) -> bool {
        let mut changed = false;

        while let Ok(done) = self.done.try_recv() {
            match done {
                Done::Ready(key, buffer) => {
                    self.pending.remove(&key);
                    let bytes = buffer.width() as usize * buffer.height() as usize * 4;
                    self.tick += 1;
                    self.bytes += bytes;
                    self.cache.insert(key, Entry { buffer, bytes, used: self.tick });
                    changed = true;
                }
                Done::Failed(key) => {
                    self.pending.remove(&key);
                    self.failed.insert(key, ());
                }
            }
        }

        if changed {
            self.evict();
        }
        changed
    }

    /// Least-recently-used, by bytes. Linear in the number of held pictures,
    /// which is a few hundred; a heap here would be more code than it saves.
    fn evict(&mut self) {
        while self.bytes > CPU_BUDGET_BYTES {
            let Some(victim) = self
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.cache.remove(&victim) {
                self.bytes -= entry.bytes;
            }
        }
    }

    /// For the metrics line.
    pub fn held_mb(&self) -> u64 {
        (self.bytes / (1024 * 1024)) as u64
    }
}

type Poke = std::sync::Arc<dyn Fn() + Send + Sync>;

fn worker(dir: PathBuf, wanted: Receiver<Key>, done: Sender<Done>, poke: Poke) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("mediabox-tv.art no runtime: {e}");
            return;
        }
    };

    runtime.block_on(async move {
        prune_disk(&dir).await;

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .user_agent("MediaBox/1.0")
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                eprintln!("mediabox-tv.art no http client: {e}");
                return;
            }
        };

        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(IN_FLIGHT));

        // The channel is synchronous and this is an async context, so the
        // blocking receive is moved off the runtime's own threads.
        let (queue_tx, mut queue_rx) = tokio::sync::mpsc::unbounded_channel::<Key>();
        std::thread::Builder::new()
            .name("mediabox-tv-art-q".into())
            .spawn(move || {
                while let Ok(key) = wanted.recv() {
                    if queue_tx.send(key).is_err() {
                        break;
                    }
                }
            })
            .expect("the artwork queue could not be started");

        while let Some(key) = queue_rx.recv().await {
            let client = client.clone();
            let dir = dir.clone();
            let done = done.clone();
            let gate = gate.clone();
            let poke = poke.clone();

            tokio::spawn(async move {
                let Ok(_permit) = gate.acquire().await else { return };
                let answer = match fetch_and_decode(&client, &dir, &key).await {
                    Some(buffer) => Done::Ready(key, buffer),
                    None => Done::Failed(key),
                };
                if done.send(answer).is_ok() {
                    poke();
                }
            });
        }
    });
}

async fn fetch_and_decode(
    client: &reqwest::Client,
    dir: &Path,
    key: &Key,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let path = dir.join(digest(&key.url));

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) if !bytes.is_empty() => bytes,
        _ => {
            let response = client.get(&key.url).send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let bytes = response.bytes().await.ok()?.to_vec();
            if bytes.is_empty() {
                return None;
            }
            // Written through a temporary and renamed: two shelves can want the
            // same backdrop, and a half-written file read by the other is a
            // picture that decodes to nothing.
            let temporary = path.with_extension("part");
            if let Ok(mut file) = std::fs::File::create(&temporary) {
                if file.write_all(&bytes).is_ok() {
                    let _ = std::fs::rename(&temporary, &path);
                } else {
                    let _ = std::fs::remove_file(&temporary);
                }
            }
            bytes
        }
    };

    let width = key.width;
    tokio::task::spawn_blocking(move || decode(&bytes, width)).await.ok()?
}

fn decode(bytes: &[u8], target_width: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    if w == 0 || h == 0 {
        return None;
    }

    // Never upscale. A 300-wide poster stretched to 480 is the same picture
    // with more pixels to push, and the panel is doing that anyway.
    let width = target_width.min(w).max(1);
    let height = ((h as u64 * width as u64) / w as u64).max(1) as u32;

    let rgba = if width == w {
        decoded.into_rgba8()
    } else {
        image::imageops::resize(
            &decoded.into_rgba8(),
            width,
            height,
            image::imageops::FilterType::Triangle,
        )
    };

    Some(SharedPixelBuffer::clone_from_slice(rgba.as_raw(), width, height))
}

fn digest(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Keeps the download cache under its budget, oldest first. Run once, at
/// startup: the cache grows by a few megabytes a session and a sweep per
/// request would cost more than it reclaims.
async fn prune_disk(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, meta.len(), entry.path()))
        })
        .collect();

    let total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= DISK_BUDGET_BYTES {
        return;
    }

    files.sort_by_key(|(modified, _, _)| *modified);
    let mut over = total - DISK_BUDGET_BYTES;
    for (_, len, path) in files {
        if over == 0 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            over = over.saturating_sub(len);
        }
    }
}
