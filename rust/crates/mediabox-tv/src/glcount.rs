//! What a frame asks of GL, counted at the functions themselves: FemtoVG, Skia
//! and the spatial scenes all load theirs through one door
//! (`GlContext::get_proc_address`). Measurement only (`gl-count`); without
//! MEDIABOX_TV_GLCOUNT the functions are handed over as they are.

use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

static DRAWS: AtomicU64 = AtomicU64::new(0);
static VERTS: AtomicU64 = AtomicU64::new(0);
static PASSES: AtomicU64 = AtomicU64::new(0);
static TEX_BYTES: AtomicU64 = AtomicU64::new(0);
static TEX_UPLOADS: AtomicU64 = AtomicU64::new(0);
static BUF_BYTES: AtomicU64 = AtomicU64::new(0);

fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("MEDIABOX_TV_GLCOUNT").is_some())
}

macro_rules! counted {
    ($($real:ident $name:ident ($($arg:ident: $ty:ty),*) $count:block)*) => {
        $(
            static $real: AtomicUsize = AtomicUsize::new(0);
            #[allow(clippy::too_many_arguments)]
            unsafe extern "C" fn $name($($arg: $ty),*) {
                $count
                // SAFETY: the address GL gave for this very name.
                let real: unsafe extern "C" fn($($ty),*) = unsafe { std::mem::transmute($real.load(Relaxed)) };
                unsafe { real($($arg),*) }
            }
        )*
    };
}

fn upload(w: i32, h: i32, data: *const c_void) {
    if !data.is_null() {
        TEX_UPLOADS.fetch_add(1, Relaxed);
        TEX_BYTES.fetch_add(w as u64 * h as u64 * 4, Relaxed);
    }
}

fn drew(vertices: i32, instances: i32) {
    DRAWS.fetch_add(1, Relaxed);
    VERTS.fetch_add(vertices as u64 * instances as u64, Relaxed);
}

counted! {
    ARRAYS draw_arrays(mode: u32, first: i32, count: i32) { drew(count, 1); }
    ELEMENTS draw_elements(mode: u32, count: i32, kind: u32, at: *const c_void) { drew(count, 1); }
    ARRAYS_I draw_arrays_i(mode: u32, first: i32, count: i32, n: i32) { drew(count, n); }
    ELEMENTS_I draw_elements_i(mode: u32, count: i32, kind: u32, at: *const c_void, n: i32) { drew(count, n); }
    BIND bind(target: u32, which: u32) { if target != 0x8CA8 { PASSES.fetch_add(1, Relaxed); } }
    IMAGE image(t: u32, l: i32, i: i32, w: i32, h: i32, b: i32, f: u32, k: u32, p: *const c_void) { upload(w, h, p); }
    SUB sub(t: u32, l: i32, x: i32, y: i32, w: i32, h: i32, f: u32, k: u32, p: *const c_void) { upload(w, h, p); }
    DATA data(t: u32, size: isize, p: *const c_void, usage: u32) { if !p.is_null() { BUF_BYTES.fetch_add(size as u64, Relaxed); } }
    SUBDATA subdata(t: u32, at: isize, size: isize, p: *const c_void) { BUF_BYTES.fetch_add(size as u64, Relaxed); }
}

pub fn wrap(name: &CStr, real: *const c_void) -> *const c_void {
    if !on() || real.is_null() {
        return real;
    }
    let put = |slot: &AtomicUsize, with: *const c_void| {
        slot.store(real as usize, Relaxed);
        with
    };
    match name.to_bytes() {
        b"glDrawArrays" => put(&ARRAYS, draw_arrays as *const c_void),
        b"glDrawElements" => put(&ELEMENTS, draw_elements as *const c_void),
        b"glDrawArraysInstanced" => put(&ARRAYS_I, draw_arrays_i as *const c_void),
        b"glDrawElementsInstanced" => put(&ELEMENTS_I, draw_elements_i as *const c_void),
        b"glBindFramebuffer" => put(&BIND, bind as *const c_void),
        b"glTexImage2D" => put(&IMAGE, image as *const c_void),
        b"glTexSubImage2D" => put(&SUB, sub as *const c_void),
        b"glBufferData" => put(&DATA, data as *const c_void),
        b"glBufferSubData" => put(&SUBDATA, subdata as *const c_void),
        _ => real,
    }
}

/// The second under way: what each of its frames took, and since when.
struct Window {
    took: Vec<Duration>,
    since: Option<Instant>,
}

thread_local! {
    static WINDOW: RefCell<Window> = const { RefCell::new(Window { took: Vec::new(), since: None }) };
}

/// A frame was drawn, `took` of the processor's time: the drawing itself,
/// without the waits for the panel and for EGL. Said once a second of drawing.
pub fn frame(took: Duration) {
    if !on() {
        return;
    }
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        window.took.push(took);
        let now = Instant::now();
        let since = *window.since.get_or_insert(now);
        if now.duration_since(since) < Duration::from_secs(1) {
            return;
        }
        let mut took = std::mem::take(&mut window.took);
        window.since = Some(now);
        took.sort();
        let at = |p: usize| took[(took.len() * p / 100).min(took.len() - 1)].as_secs_f64() * 1e3;
        let per = |n: &AtomicU64| n.swap(0, Relaxed) as f64 / took.len() as f64;
        eprintln!(
            "mediabox-tv.glcount frames={} draws={:.1} vertices={:.0} binds={:.1} tex_uploads={:.2} tex_kb={:.1} buf_kb={:.1} draw_p50={:.2} draw_p95={:.2} draw_p99={:.2}",
            took.len(),
            per(&DRAWS),
            per(&VERTS),
            per(&PASSES),
            per(&TEX_UPLOADS),
            per(&TEX_BYTES) / 1024.0,
            per(&BUF_BYTES) / 1024.0,
            at(50),
            at(95),
            at(99)
        );
    });
}
