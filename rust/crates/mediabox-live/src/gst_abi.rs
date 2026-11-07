//! The audio sink's fields the sound clock's controller reads, through
//! src/gst_abi.c (compiled against the GStreamer headers the player is
//! built with), and the guard that holds those headers to the libraries
//! loaded: the same GStreamer version, the same libgstreamer and
//! libgstaudio (their GNU build-ids, taken from the libraries at build
//! time), GstAudioBaseSink and GstAudioClock of the instance sizes the
//! headers give. Anything else, and no field is read (`guard`).

use std::ffi::c_void;

#[path = "elf_note.rs"]
mod elf_note;

unsafe extern "C" {
    fn mbx_gst_headers_version() -> u32;
    fn mbx_audio_base_sink_type() -> gst::glib::ffi::GType;
    fn mbx_audio_clock_type() -> gst::glib::ffi::GType;
    fn mbx_audio_base_sink_size() -> usize;
    fn mbx_audio_clock_size() -> usize;
    fn mbx_audio_base_sink_next_sample(sink: *mut gst::ffi::GstElement) -> u64;
    fn mbx_audio_base_sink_clock(sink: *mut gst::ffi::GstElement) -> *mut gst::ffi::GstClock;
    fn mbx_audio_clock_user_data(clock: *mut gst::ffi::GstClock) -> *mut c_void;
    fn mbx_audio_clock_time_offset(clock: *mut gst::ffi::GstClock) -> i64;
}

/// The libraries the shim's headers came with (build.rs).
pub const BUILT_AGAINST: [(&str, &str); 2] =
    [("libgstreamer-1.0.so.0", env!("MEDIABOX_GST_CORE_BUILD_ID")), ("libgstaudio-1.0.so.0", env!("MEDIABOX_GST_AUDIO_BUILD_ID"))];

/// What the guard holds against each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// major * 1e6 + minor * 1e3 + micro.
    pub headers_version: u32,
    pub runtime_version: u32,
    /// (library, built against, loaded).
    pub libraries: Vec<(String, String, Option<String>)>,
    /// (type, the headers' sizeof, the type system's instance size).
    pub sizes: Vec<(String, usize, usize)>,
}

/// None when every fact agrees; why not, when one does not.
pub fn check(f: &Facts) -> Result<(), String> {
    if f.headers_version != f.runtime_version {
        return Err(format!("GStreamer {} loaded, headers of {}", f.runtime_version, f.headers_version));
    }
    for (lib, built, loaded) in &f.libraries {
        match loaded {
            None => return Err(format!("{lib} not loaded")),
            Some(id) if id != built => return Err(format!("{lib} build-id {id}, built against {built}")),
            Some(_) => {}
        }
    }
    for (ty, headers, instance) in &f.sizes {
        if headers != instance || *headers == 0 {
            return Err(format!("{ty} is {instance} bytes, the headers' {headers}"));
        }
    }
    Ok(())
}

/// The build-ids of the loaded objects whose names end in one of `libs`.
fn loaded_build_ids(libs: &[&str]) -> Vec<Option<String>> {
    struct Seek<'a> {
        libs: &'a [&'a str],
        found: Vec<Option<String>>,
    }
    unsafe extern "C" fn each(info: *mut libc::dl_phdr_info, _: usize, data: *mut c_void) -> i32 {
        // SAFETY: dl_iterate_phdr gives a valid info for the call and our
        // own `Seek` as data.
        let (info, seek) = unsafe { (&*info, &mut *(data as *mut Seek)) };
        if info.dlpi_name.is_null() {
            return 0;
        }
        // SAFETY: a NUL-terminated name, valid for the call.
        let name = unsafe { std::ffi::CStr::from_ptr(info.dlpi_name) }.to_string_lossy();
        let Some(k) = seek.libs.iter().position(|l| name.rsplit('/').next() == Some(*l)) else { return 0 };
        for i in 0..usize::from(info.dlpi_phnum) {
            // SAFETY: dlpi_phnum headers at dlpi_phdr; a PT_NOTE's bytes
            // are mapped at its address in the object.
            let ph = unsafe { &*info.dlpi_phdr.add(i) };
            if ph.p_type == libc::PT_NOTE {
                let notes = unsafe { std::slice::from_raw_parts((info.dlpi_addr + ph.p_vaddr) as *const u8, ph.p_memsz as usize) };
                if let Some(id) = elf_note::build_id(notes) {
                    seek.found[k] = Some(id);
                }
            }
        }
        0
    }
    let mut seek = Seek { libs, found: vec![None; libs.len()] };
    // SAFETY: the callback only reads what the loader gives it.
    unsafe { libc::dl_iterate_phdr(Some(each), &mut seek as *mut Seek as *mut c_void) };
    seek.found
}

fn instance_size(ty: gst::glib::ffi::GType) -> usize {
    let mut q = std::mem::MaybeUninit::<gst::glib::gobject_ffi::GTypeQuery>::zeroed();
    // SAFETY: a registered type (from its get_type) and a query to fill.
    unsafe {
        gst::glib::gobject_ffi::g_type_query(ty, q.as_mut_ptr());
        q.assume_init().instance_size as usize
    }
}

/// The facts of this process.
pub fn facts() -> Facts {
    let (major, minor, micro, _) = gst::version();
    let libs: Vec<&str> = BUILT_AGAINST.iter().map(|(l, _)| *l).collect();
    let loaded = loaded_build_ids(&libs);
    // SAFETY: plain functions of the shim, no arguments.
    let (headers_version, sink_type, clock_type, sink_size, clock_size) = unsafe {
        (mbx_gst_headers_version(), mbx_audio_base_sink_type(), mbx_audio_clock_type(), mbx_audio_base_sink_size(), mbx_audio_clock_size())
    };
    Facts {
        headers_version,
        runtime_version: major * 1_000_000 + minor * 1_000 + micro,
        libraries: BUILT_AGAINST.iter().zip(loaded).map(|((l, b), found)| ((*l).to_string(), (*b).to_string(), found)).collect(),
        sizes: vec![
            ("GstAudioBaseSink".into(), sink_size, instance_size(sink_type)),
            ("GstAudioClock".into(), clock_size, instance_size(clock_type)),
        ],
    }
}

/// The guard, once a process.
pub fn guard() -> Result<(), String> {
    static GUARD: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    GUARD.get_or_init(|| check(&facts())).clone()
}

/// An audiobasesink whose fields may be read: the guard passed and the
/// element is one.
#[derive(Debug, Clone, Copy)]
pub struct AudioSink(*mut gst::ffi::GstElement);

// SAFETY: only a pointer to an element whose owner holds it while used.
unsafe impl Send for AudioSink {}
unsafe impl Sync for AudioSink {}

impl AudioSink {
    pub fn of(sink: &gst::Element) -> Result<Self, String> {
        use gst::glib::translate::ToGlibPtr;
        use gst::prelude::*;
        guard()?;
        // SAFETY: the shim's type function.
        let ty: gst::glib::Type = unsafe { gst::glib::translate::from_glib(mbx_audio_base_sink_type()) };
        if !sink.type_().is_a(ty) {
            return Err(format!("{} is not a GstAudioBaseSink", sink.type_().name()));
        }
        Ok(Self(sink.to_glib_none().0))
    }

    /// The sample the sink writes the next buffer at; none when it will
    /// place it at its time.
    pub fn next_sample(self) -> Option<u64> {
        // SAFETY: an audiobasesink (`of`), alive while its probe runs.
        let v = unsafe { mbx_audio_base_sink_next_sample(self.0) };
        (v != u64::MAX).then_some(v)
    }

    /// The clock the sink made, as its field holds it.
    pub fn clock_field(self) -> usize {
        // SAFETY: as `next_sample`.
        unsafe { mbx_audio_base_sink_clock(self.0) as usize }
    }

    pub fn ptr(self) -> usize {
        self.0 as usize
    }
}

/// Whether `clock` -- what the sink's `provide_clock` gave -- is the
/// sink's own GstAudioClock: its field's, made by it (user data), of the
/// type.
pub fn the_sinks_clock(field: usize, provided: usize, user_data: usize, sink: usize, is_audio_clock: bool) -> Result<(), String> {
    if provided == 0 || field != provided {
        return Err(format!("the sink provides clock {provided:#x}, its field holds {field:#x}"));
    }
    if !is_audio_clock {
        return Err("the sink's clock is not a GstAudioClock".into());
    }
    if user_data != sink {
        return Err(format!("the clock was made by {user_data:#x}, not the sink {sink:#x}"));
    }
    Ok(())
}

/// The sink's clock, held to `the_sinks_clock`.
pub fn audio_clock(sink: AudioSink, clock: &gst::Clock) -> Result<AudioClock, String> {
    use gst::glib::translate::ToGlibPtr;
    use gst::prelude::*;
    let ptr: *mut gst::ffi::GstClock = clock.to_glib_none().0;
    // SAFETY: the shim's type function.
    let ty: gst::glib::Type = unsafe { gst::glib::translate::from_glib(mbx_audio_clock_type()) };
    let is_audio_clock = clock.type_().is_a(ty);
    // SAFETY: read only once it is a GstAudioClock.
    let user_data = if is_audio_clock { unsafe { mbx_audio_clock_user_data(ptr) as usize } } else { 0 };
    the_sinks_clock(sink.clock_field(), ptr as usize, user_data, sink.ptr(), is_audio_clock)?;
    Ok(AudioClock(ptr))
}

/// The sink's GstAudioClock, checked.
#[derive(Debug, Clone, Copy)]
pub struct AudioClock(*mut gst::ffi::GstClock);

impl AudioClock {
    /// The offset the clock adds (gst_audio_clock_adjust).
    pub fn time_offset(self) -> i64 {
        // SAFETY: the sink's audio clock (`audio_clock`), held by the
        // caller's reference.
        unsafe { mbx_audio_clock_time_offset(self.0) }
    }
}

#[cfg(test)]
#[path = "gst_abi_tests.rs"]
mod tests;
