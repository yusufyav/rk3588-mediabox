//! The hardware-animation experiment: Tarayıcı's turning sphere on the
//! display controller's own plane instead of in the interface's frame.
//!
//! Off unless MEDIABOX_TV_GLOBE_OFFLOAD names a method (`Method`) and the
//! frames it needs are there; then nothing of it exists but one read of an
//! environment variable.
//!
//! The sphere was drawn beforehand (src/hit/gl/globe.rs) as the light it adds
//! to the ground, at `FRAMES` places of the camera. Which of them is on the
//! panel, where, how large and how bright follow from the camera's offset
//! `d` alone, at the moment the interface's frame is drawn: there is no
//! playback, so a reversal, a held key and an interrupted move are only
//! other values of `d`. The plane goes up in the same atomic commit as the
//! interface's frame, under it. The controller mixes
//! `interface + (1 - interface's alpha) * plane`, so where the interface's
//! alpha is cleared and its colour kept (`GlContext::punch`) the plane's
//! light is added to the ground Slint drew -- which needs no alpha of the
//! video.

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_int, c_void};
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use drm::buffer::{Buffer, DrmFourcc};
use drm::control;

use crate::platform::SharedKms;
use crate::video::{Colour, Frame, Imported, Rect};

pub const FRAMES: usize = 129;
/// Full range, so nothing of the light is rounded away -- and the controller
/// then converts by BT.601 whatever is asked (measured 2026-11-25: asked for
/// BT.709 full, `y2r[BT.601_F]`), so that is what the frames are.
const FULL: Colour = Colour { encoding: 0, range: 1, eotf: 0 };

type Ask = Box<dyn Fn(bool) -> Option<f32>>;

/// The camera's offset at either end: where the sphere has faded out.
const REACH: f32 = 0.9;
/// The frames stand closer together where the camera is slow: near rest.
const CLOSER: f32 = 1.6;
/// The sphere's box and its stage, in design pixels (quiet/home.slint,
/// `QuietPlaces`; the sphere is 194 units round the middle of 650).
const BOX: f32 = 396.0;
const STAGE: (f32, f32, f32) = (1201.0, 150.0, 650.0);
/// What the frames were drawn at (`Scene.presence` on the home screen).
#[cfg(test)]
pub const PRESENCE: f32 = 0.9;

/// The camera's offset frame `index` was drawn at.
#[cfg(test)]
pub fn offset_of(index: usize) -> f32 {
    let s = index as f32 / (FRAMES - 1) as f32 * 2.0 - 1.0;
    REACH * s.signum() * s.abs().powf(CLOSER)
}

fn index_of(d: f32) -> usize {
    let s = d.signum() * (d.abs() / REACH).min(1.0).powf(1.0 / CLOSER);
    ((s + 1.0) / 2.0 * (FRAMES - 1) as f32).round() as usize
}

fn unit(panel: (u32, u32)) -> f32 {
    (panel.0 as f32 / 1920.0).min(panel.1 as f32 / 1080.0)
}

/// The box's left, top and side at `d`, in the panel's pixels, unrounded.
fn place(d: f32, panel: (u32, u32)) -> (f32, f32, f32) {
    let u = unit(panel);
    let scale = if d >= 0.0 { (1.0 - d * 0.32).max(0.25) } else { 1.0 - d * 0.42 };
    let side = BOX * u * scale;
    let half = STAGE.2 * u / 2.0;
    let x = STAGE.0 * u + half;
    let y = STAGE.1 * u + half + STAGE.2 * u * 0.34 * d;
    (x - side / 2.0, y - side / 2.0, side)
}

/// The side of a frame drawn for `panel`.
pub fn drawn_at(panel: (u32, u32)) -> u32 {
    place(0.0, panel).2.round() as u32
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub index: usize,
    pub dst: Rect,
    /// The plane's `alpha` property.
    pub alpha: u16,
}

/// `d` as GlobeScene has it; none where it has faded out.
pub fn pose(d: f32, panel: (u32, u32)) -> Option<Pose> {
    let fade = 1.0 - d.abs() / REACH;
    if fade <= 0.0 {
        return None;
    }
    let (x, y, side) = place(d, panel);
    let side = side.round().max(2.0) as u32;
    Some(Pose {
        index: index_of(d),
        dst: Rect { x: x.round() as i32, y: y.round() as i32, width: side, height: side },
        alpha: (fade * 65535.0).round() as u16,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    /// No sphere at all: what the rest of the frame costs.
    Nothing,
    /// B: one buffer, which the controller moves, scales and fades.
    Still,
    /// C: each frame decoded by the VPU when it is wanted.
    Decode,
    /// D: every frame decoded by the VPU at start, and kept.
    Cache,
}

/// A frame the plane can show, and what holds its memory.
pub struct Shown {
    pub framebuffer: control::framebuffer::Handle,
    pub size: u32,
    pub colour: Colour,
    pub index: usize,
    _imported: Option<Arc<Imported<SharedKms>>>,
    _frame: Option<MppFrame>,
}

/// What goes up with the interface's next frame.
#[derive(Clone)]
pub struct Overlay {
    pub shown: Arc<Shown>,
    pub pose: Pose,
}

#[derive(Default)]
struct State {
    want: Option<usize>,
    ready: bool,
    failed: bool,
    /// D's frames; C's last one alone.
    kept: Vec<Arc<Shown>>,
    latest: Option<Arc<Shown>>,
    decode_us: Vec<u32>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One move, counted: from the sphere coming up to its going.
#[derive(Default)]
struct Tally {
    frames: u32,
    distinct: u32,
    late: u32,
    last: Option<usize>,
}

struct Engine {
    method: Method,
    /// The panel the frames were drawn for.
    panel: (u32, u32),
    shared: Arc<Shared>,
    tally: std::cell::RefCell<Tally>,
    still: Option<Arc<Shown>>,
}

thread_local! {
    static ENGINE: std::cell::RefCell<Option<Engine>> = const { std::cell::RefCell::new(None) };
    static NEXT: std::cell::RefCell<Option<Overlay>> = const { std::cell::RefCell::new(None) };
    static ASK: std::cell::RefCell<Option<Ask>> = const { std::cell::RefCell::new(None) };
}

/// How the interface says where the sphere is: given whether the plane can
/// take it, it tells the scene (`QuietOffload.globe`) and answers with `d`
/// while the home screen shows it.
pub fn on_ask(ask: impl Fn(bool) -> Option<f32> + 'static) {
    ASK.with(|slot| *slot.borrow_mut() = Some(Box::new(ask)));
}

/// What the environment asks for. The frames' path and the panel they were
/// drawn for: MEDIABOX_TV_GLOBE_FRAMES, MEDIABOX_TV_GLOBE_PANEL (WxH).
pub fn start(kms: &SharedKms) {
    let method = match std::env::var("MEDIABOX_TV_GLOBE_OFFLOAD").as_deref() {
        Ok("nothing") => Method::Nothing,
        Ok("still") => Method::Still,
        Ok("decode") => Method::Decode,
        Ok("cache") => Method::Cache,
        _ => return,
    };
    let path = std::env::var("MEDIABOX_TV_GLOBE_FRAMES").unwrap_or_default();
    let panel = std::env::var("MEDIABOX_TV_GLOBE_PANEL")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((2560, 1440));
    let shared = Arc::new(Shared::default());
    let mut still = None;
    match method {
        Method::Nothing => shared.lock().ready = true,
        Method::Still => match still_frame(kms, &path, drawn_at(panel)) {
            Ok(frame) => {
                still = Some(frame);
                shared.lock().ready = true;
            }
            Err(error) => {
                eprintln!("mediabox-tv.globe {error}");
                return;
            }
        },
        Method::Decode | Method::Cache => {
            let (worker, kms) = (shared.clone(), kms.clone());
            let cache = method == Method::Cache;
            std::thread::Builder::new()
                .name("globe-vpu".into())
                .spawn(move || {
                    if let Err(error) = decode(&worker, kms, &path, cache) {
                        eprintln!("mediabox-tv.globe {error}; Slint draws the sphere");
                        worker.lock().failed = true;
                    }
                })
                .expect("a thread");
        }
    }
    eprintln!("mediabox-tv.globe experiment {method:?} panel={}x{}", panel.0, panel.1);
    ENGINE.with(|slot| *slot.borrow_mut() = Some(Engine { method, panel, shared, tally: Default::default(), still }));
}

/// The display refused the plane, or is being changed: Slint's from now on.
pub fn give_up(shared_error: &str) {
    ENGINE.with(|slot| {
        if let Some(engine) = slot.borrow().as_ref()
            && !std::mem::replace(&mut engine.shared.lock().failed, true)
        {
            eprintln!("mediabox-tv.globe refused: {shared_error}; Slint draws the sphere");
        }
    });
}

/// Before the interface's frame is drawn: where the sphere is in it, and
/// which frame that wants.
pub fn begin_frame(panel: (u32, u32)) {
    ENGINE.with(|slot| {
        let slot = slot.borrow();
        let Some(engine) = slot.as_ref() else { return };
        let able = {
            let s = engine.shared.lock();
            s.ready && !s.failed && engine.panel == panel
        };
        let d = ASK.with(|ask| ask.borrow().as_ref().and_then(|ask| ask(able)));
        let pose = if able { d.and_then(|d| pose(d, panel)) } else { None };
        let next = pose.and_then(|pose| engine.frame_for(pose));
        engine.count(pose, next.as_ref());
        NEXT.with(|slot| *slot.borrow_mut() = next);
    });
}

/// The rectangle of the panel the sphere's light comes through, this frame.
pub fn hole() -> Option<Rect> {
    NEXT.with(|slot| slot.borrow().as_ref().map(|next| next.pose.dst))
}

/// What goes up with this frame: asked again now that it is drawn, for the
/// frame the VPU finished while it was.
pub fn take() -> Option<Overlay> {
    let next = NEXT.with(|slot| slot.borrow_mut().take())?;
    ENGINE.with(|slot| slot.borrow().as_ref().and_then(|engine| engine.frame_for(next.pose)))
}

impl Engine {
    fn frame_for(&self, pose: Pose) -> Option<Overlay> {
        let shown = match self.method {
            Method::Nothing => None,
            Method::Still => self.still.clone(),
            Method::Cache => self.shared.lock().kept.get(pose.index).cloned(),
            Method::Decode => {
                let mut s = self.shared.lock();
                if s.want.replace(pose.index) != Some(pose.index) {
                    self.shared.changed.notify_all();
                }
                s.latest.clone()
            }
        }?;
        mediabox_core::timeline!("globe", "want={} shown={}", pose.index, shown.index);
        Some(Overlay { shown, pose })
    }

    /// Counts the move, and says it when it is over.
    fn count(&self, pose: Option<Pose>, next: Option<&Overlay>) {
        let mut tally = self.tally.borrow_mut();
        match (pose, next) {
            (Some(pose), next) => {
                let index = next.map(|n| n.shown.index);
                tally.frames += 1;
                tally.distinct += u32::from(index.is_some() && index != tally.last);
                tally.late += u32::from(index != Some(pose.index));
                tally.last = index;
            }
            (None, _) if tally.frames > 0 => {
                let t = std::mem::take(&mut *tally);
                let mut us = std::mem::take(&mut self.shared.lock().decode_us);
                us.sort_unstable();
                let at = |p: usize| us.get((us.len() * p / 100).min(us.len().saturating_sub(1))).copied().unwrap_or(0);
                eprintln!(
                    "mediabox-tv.globe frames={} distinct={} late_at_begin={} decoded={} decode_us p50={} p95={} max={}",
                    t.frames, t.distinct, t.late, us.len(), at(50), at(95), at(100)
                );
            }
            _ => {}
        }
    }
}

/// B's frame: XRGB in a dumb buffer, from the raw frame beside the stream.
fn still_frame(kms: &SharedKms, path: &str, side: u32) -> Result<Arc<Shown>, String> {
    use drm::control::Device;
    let raw = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if raw.len() != (side * side * 4) as usize {
        return Err(format!("{path}: not {side}x{side} XRGB"));
    }
    let mut buffer = kms
        .create_dumb_buffer((side, side), DrmFourcc::Xrgb8888, 32)
        .map_err(|e| format!("dumb buffer: {e}"))?;
    let pitch = buffer.pitch() as usize;
    {
        let mut map = kms.map_dumb_buffer(&mut buffer).map_err(|e| e.to_string())?;
        for (row, from) in raw.chunks_exact(side as usize * 4).enumerate() {
            map[row * pitch..][..from.len()].copy_from_slice(from);
        }
    }
    let framebuffer = kms.add_framebuffer(&buffer, 24, 32).map_err(|e| format!("framebuffer: {e}"))?;
    Ok(Arc::new(Shown {
        framebuffer,
        size: side,
        colour: FULL,
        index: FRAMES / 2,
        _imported: None,
        _frame: None,
    }))
}

// ------------------------------------------------------------- the decoder
//
// MPP by its C interface, opened when asked for: the interface links nothing
// of the media runtime's, and does not start to for an experiment.

type Ptr = *mut c_void;
type Call1 = unsafe extern "C" fn(Ptr) -> c_int;
type Get = unsafe extern "C" fn(Ptr) -> u32;

/// `MppApi`, as far as it is used.
#[repr(C)]
struct Api {
    size: u32,
    version: u32,
    decode: Ptr,
    put_packet: unsafe extern "C" fn(Ptr, Ptr) -> c_int,
    get_frame: unsafe extern "C" fn(Ptr, *mut Ptr) -> c_int,
    _unused: [Ptr; 9],
    reset: Call1,
    control: unsafe extern "C" fn(Ptr, u32, Ptr) -> c_int,
}

const MPP_SET_OUTPUT_TIMEOUT: u32 = 0x0020_0007;
const MPP_DEC_SET_EXT_BUF_GROUP: u32 = 0x0031_0002;
const MPP_DEC_SET_INFO_CHANGE_READY: u32 = 0x0031_0003;
const MPP_DEC_SET_PARSER_SPLIT_MODE: u32 = 0x0031_0005;
const MPP_DEC_SET_IMMEDIATE_OUT: u32 = 0x0031_000c;
const MPP_VIDEO_CODING_HEVC: c_int = 0x0100_0004;
const MPP_BUFFER_TYPE_DMA_HEAP: c_int = 4;

struct Mpp(Ptr);

impl Mpp {
    fn open() -> Result<Self, String> {
        let path = std::env::var("MEDIABOX_TV_MPP")
            .unwrap_or_else(|_| "/opt/rk3588-mediabox/media-runtime/lib/librockchip_mpp.so.1".into());
        let name = CString::new(path.clone()).map_err(|e| e.to_string())?;
        // SAFETY: a path, and the flag for resolving everything now.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() { Err(format!("cannot open {path}")) } else { Ok(Self(handle)) }
    }

    /// SAFETY: `F` must be the type of the function of that name.
    unsafe fn get<F: Copy>(&self, name: &str) -> Result<F, String> {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        let at = unsafe { libc::dlsym(self.0, c.as_ptr()) };
        if at.is_null() {
            return Err(format!("MPP has no {name}"));
        }
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&at) })
    }
}

/// A decoded frame: MPP's buffer is out of its pool until this goes.
struct MppFrame(Ptr, unsafe extern "C" fn(*mut Ptr) -> c_int);
// SAFETY: MPP's frames are reference counted and may be let go anywhere.
unsafe impl Send for MppFrame {}
unsafe impl Sync for MppFrame {}

impl Drop for MppFrame {
    fn drop(&mut self) {
        // SAFETY: the frame MPP gave, let go once.
        unsafe { (self.1)(&mut self.0) };
    }
}

/// The stream's access units, by their delimiters (NAL type 35).
fn access_units(stream: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut starts: Vec<usize> = stream
        .windows(6)
        .enumerate()
        .filter(|(_, w)| w[..4] == [0, 0, 0, 1] && (w[4] >> 1) & 0x3f == 35)
        .map(|(at, _)| at)
        .collect();
    starts.push(stream.len());
    starts.windows(2).map(|w| w[0]..w[1]).collect()
}

fn decode(shared: &Shared, kms: SharedKms, path: &str, cache: bool) -> Result<(), String> {
    let began = Instant::now();
    let stream = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let units = access_units(&stream);
    if units.len() != FRAMES {
        return Err(format!("{path}: {} frames, not {FRAMES}", units.len()));
    }
    let mpp = Mpp::open()?;
    // SAFETY (all of it): MPP's C interface, as its headers declare it
    // (media-runtime/include/rockchip); every pointer is a live local, or one
    // MPP gave.
    unsafe {
        let create: unsafe extern "C" fn(*mut Ptr, *mut *mut Api) -> c_int = mpp.get("mpp_create")?;
        let init: unsafe extern "C" fn(Ptr, c_int, c_int) -> c_int = mpp.get("mpp_init")?;
        let destroy: Call1 = mpp.get("mpp_destroy")?;
        let packet_init: unsafe extern "C" fn(*mut Ptr, *mut c_void, usize) -> c_int = mpp.get("mpp_packet_init")?;
        let packet_deinit: unsafe extern "C" fn(*mut Ptr) -> c_int = mpp.get("mpp_packet_deinit")?;
        let frame_deinit: unsafe extern "C" fn(*mut Ptr) -> c_int = mpp.get("mpp_frame_deinit")?;
        let info_change: Get = mpp.get("mpp_frame_get_info_change")?;
        let errinfo: Get = mpp.get("mpp_frame_get_errinfo")?;
        let discard: Get = mpp.get("mpp_frame_get_discard")?;
        let width: Get = mpp.get("mpp_frame_get_width")?;
        let height: Get = mpp.get("mpp_frame_get_height")?;
        let hor: Get = mpp.get("mpp_frame_get_hor_stride")?;
        let ver: Get = mpp.get("mpp_frame_get_ver_stride")?;
        let fmt: Get = mpp.get("mpp_frame_get_fmt")?;
        let buffer: unsafe extern "C" fn(Ptr) -> Ptr = mpp.get("mpp_frame_get_buffer")?;
        let fd_of: unsafe extern "C" fn(Ptr, *const c_char) -> c_int = mpp.get("mpp_buffer_get_fd_with_caller")?;
        let size_of: unsafe extern "C" fn(Ptr, *const c_char) -> usize = mpp.get("mpp_buffer_get_size_with_caller")?;
        let group_get: unsafe extern "C" fn(*mut Ptr, c_int, c_int, *const c_char, *const c_char) -> c_int =
            mpp.get("mpp_buffer_group_get")?;
        let tag = c"globe".as_ptr();

        let (mut ctx, mut api): (Ptr, *mut Api) = (std::ptr::null_mut(), std::ptr::null_mut());
        if create(&mut ctx, &mut api) != 0 {
            return Err("mpp_create".into());
        }
        let api = &*api;
        let mut off = 0u32;
        (api.control)(ctx, MPP_DEC_SET_PARSER_SPLIT_MODE, (&raw mut off).cast());
        if init(ctx, 0, MPP_VIDEO_CODING_HEVC) != 0 {
            return Err("mpp_init".into());
        }
        let mut on = 1u32;
        (api.control)(ctx, MPP_DEC_SET_IMMEDIATE_OUT, (&raw mut on).cast());
        let mut wait = 100i64;
        (api.control)(ctx, MPP_SET_OUTPUT_TIMEOUT, (&raw mut wait).cast());

        let mut group: Ptr = std::ptr::null_mut();
        // The framebuffers, by the buffer they are of: its descriptor is the
        // buffer's for as long as its group lives.
        let mut imported: HashMap<c_int, Arc<Imported<SharedKms>>> = HashMap::new();
        let mut said = false;
        let mut one = |index: usize| -> Result<Arc<Shown>, String> {
            let unit = &stream[units[index].clone()];
            let mut packet: Ptr = std::ptr::null_mut();
            if packet_init(&mut packet, unit.as_ptr().cast_mut().cast(), unit.len()) != 0 {
                return Err("mpp_packet_init".into());
            }
            let asked = Instant::now();
            let mut sent = (api.put_packet)(ctx, packet);
            while sent != 0 && asked.elapsed() < Duration::from_millis(200) {
                std::thread::sleep(Duration::from_micros(200));
                sent = (api.put_packet)(ctx, packet);
            }
            packet_deinit(&mut packet);
            if sent != 0 {
                return Err(format!("decode_put_packet {sent}"));
            }
            loop {
                let mut raw: Ptr = std::ptr::null_mut();
                let got = (api.get_frame)(ctx, &mut raw);
                if raw.is_null() {
                    if asked.elapsed() > Duration::from_secs(1) {
                        return Err(format!("no frame {index} ({got})"));
                    }
                    continue;
                }
                let frame = MppFrame(raw, frame_deinit);
                if info_change(raw) != 0 {
                    if group.is_null() && group_get(&mut group, MPP_BUFFER_TYPE_DMA_HEAP, 0, tag, tag) != 0 {
                        return Err("mpp_buffer_group_get".into());
                    }
                    (api.control)(ctx, MPP_DEC_SET_EXT_BUF_GROUP, group);
                    (api.control)(ctx, MPP_DEC_SET_INFO_CHANGE_READY, std::ptr::null_mut());
                    continue;
                }
                if errinfo(raw) != 0 || discard(raw) != 0 {
                    return Err(format!("frame {index} came back broken"));
                }
                if fmt(raw) != 0 {
                    return Err(format!("frame format {:#x}, not linear NV12", fmt(raw)));
                }
                let mem = buffer(raw);
                let fd = fd_of(mem, tag);
                let (w, h, pitch, rows) = (width(raw), height(raw), hor(raw), ver(raw));
                let import = match imported.get(&fd) {
                    Some(done) => done.clone(),
                    None => {
                        let made = Arc::new(crate::video::import_frame(
                            &kms,
                            &Frame {
                                fourcc: DrmFourcc::Nv12,
                                width: w,
                                height: h,
                                pitches: [pitch, pitch, 0, 0],
                                offsets: [0, pitch * rows, 0, 0],
                                planes: 2,
                                modifier: 0,
                                colour: Colour::default(),
                            },
                            &[BorrowedFd::borrow_raw(fd)],
                        )?);
                        imported.insert(fd, made.clone());
                        made
                    }
                };
                if !std::mem::replace(&mut said, true) {
                    eprintln!(
                        "mediabox-tv.globe first frame {:.1} ms after the thread began: {w}x{h} NV12 pitch={pitch} rows={rows} buffer={} bytes",
                        began.elapsed().as_secs_f64() * 1e3,
                        size_of(mem, tag)
                    );
                }
                let us = asked.elapsed().as_micros() as u32;
                shared.lock().decode_us.push(us);
                mediabox_core::timeline!("globe-decoded", "index={index} us={us}");
                return Ok(Arc::new(Shown {
                    framebuffer: import.framebuffer(),
                    size: w,
                    colour: FULL,
                    index,
                    _imported: Some(import),
                    _frame: Some(frame),
                }));
            }
        };

        if cache {
            let mut kept = Vec::with_capacity(FRAMES);
            for index in 0..FRAMES {
                kept.push(one(index)?);
            }
            // The frames keep their buffers; the decoder is done with.
            destroy(ctx);
            let mut s = shared.lock();
            s.decode_us.clear();
            s.kept = kept;
            s.ready = true;
            drop(s);
            eprintln!(
                "mediabox-tv.globe {FRAMES} frames decoded in {:.0} ms, {} framebuffers",
                began.elapsed().as_secs_f64() * 1e3,
                imported.len()
            );
            // Drawn again, with the plane.
            let _ = slint::invoke_from_event_loop(redraw);
            return Ok(());
        }
        let mut done = FRAMES / 2;
        let first = one(done)?;
        {
            let mut s = shared.lock();
            s.latest = Some(first);
            s.ready = true;
        }
        let _ = slint::invoke_from_event_loop(redraw);
        loop {
            let want = {
                let mut s = shared.lock();
                loop {
                    match s.want {
                        Some(want) if want != done => break want,
                        _ => s = shared.changed.wait(s).unwrap_or_else(|e| e.into_inner()),
                    }
                }
            };
            let shown = one(want)?;
            done = want;
            shared.lock().latest = Some(shown);
        }
    }
}

/// A frame, though nothing in the interface has changed.
fn redraw() {
    crate::with_app(|app| {
        if let Some(window) = app.window.upgrade() {
            slint::ComponentHandle::window(&window).request_redraw();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: (u32, u32) = (2560, 1440);

    #[test]
    fn a_frame_is_the_offset_it_was_drawn_at() {
        for index in 0..FRAMES {
            assert_eq!(index_of(offset_of(index)), index);
        }
    }

    #[test]
    fn at_rest_it_stands_where_slint_draws_it() {
        let rest = pose(0.0, PANEL).unwrap();
        assert_eq!(rest, Pose { index: FRAMES / 2, dst: Rect { x: 1771, y: 369, width: 528, height: 528 }, alpha: 65535 });
        assert_eq!(drawn_at(PANEL), 528);
    }

    #[test]
    fn it_is_gone_a_place_away_and_there_on_the_way() {
        assert_eq!(pose(0.9, PANEL), None);
        assert_eq!(pose(-1.0, PANEL), None);
        let near = pose(-0.45, PANEL).unwrap();
        let far = pose(0.45, PANEL).unwrap();
        assert!(near.dst.width > 528 && far.dst.width < 528);
        assert!(near.index < FRAMES / 2 && far.index > FRAMES / 2);
        assert!((i32::from(near.alpha) - 32768).abs() < 2);
    }

    #[test]
    fn the_stream_is_cut_at_its_delimiters() {
        let mut stream = Vec::new();
        for n in 1..=3u8 {
            stream.extend([0, 0, 0, 1, 70, 1, 16]);
            stream.extend([0, 0, 0, 1, 38, 1, n]);
        }
        let units = access_units(&stream);
        assert_eq!(units.len(), 3);
        assert!(units.iter().all(|u| u.len() == 14));
    }
}
