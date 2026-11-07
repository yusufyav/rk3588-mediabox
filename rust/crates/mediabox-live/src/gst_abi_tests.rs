use super::*;
use gst::prelude::*;

fn agreeing() -> Facts {
    gst::init().unwrap();
    let f = facts();
    assert_eq!(check(&f), Ok(()), "this build's own libraries: {f:?}");
    f
}

#[test]
fn the_libraries_loaded_are_the_ones_built_against() {
    let f = agreeing();
    assert_eq!(f.libraries.len(), 2);
    for (lib, built, loaded) in &f.libraries {
        assert_eq!(built.len(), 40, "{lib}: a SHA-1 build-id");
        assert_eq!(loaded.as_deref(), Some(built.as_str()), "{lib}");
    }
    assert_eq!(guard(), Ok(()));
}

#[test]
fn another_gstreamer_version_build_or_layout_is_refused() {
    let good = agreeing();
    let mut f = good.clone();
    f.runtime_version += 1;
    assert!(check(&f).unwrap_err().contains("headers of"), "another micro version");
    let mut f = good.clone();
    f.libraries[1].2 = Some("00".repeat(20));
    assert!(check(&f).unwrap_err().contains("libgstaudio-1.0.so.0 build-id"), "another libgstaudio");
    let mut f = good.clone();
    f.libraries[0].2 = None;
    assert!(check(&f).unwrap_err().contains("not loaded"));
    let mut f = good.clone();
    f.sizes[0].2 += 8;
    assert!(check(&f).unwrap_err().contains("GstAudioBaseSink is"), "a larger instance than the headers'");
}

#[test]
fn only_the_sinks_own_audio_clock_is_taken() {
    let (sink, clock) = (0x1000, 0x2000);
    assert_eq!(the_sinks_clock(clock, clock, sink, sink, true), Ok(()));
    assert!(the_sinks_clock(clock, 0x3000, sink, sink, true).is_err(), "another clock than the field's");
    assert!(the_sinks_clock(clock, 0, sink, sink, true).is_err(), "no clock");
    assert!(the_sinks_clock(clock, clock, 0x4000, sink, true).is_err(), "a clock another sink made");
    assert!(the_sinks_clock(clock, clock, sink, sink, false).is_err(), "not a GstAudioClock");
}

#[test]
fn the_fields_are_read_from_alsasink_and_refused_elsewhere() {
    gst::init().unwrap();
    let sink = gst::ElementFactory::make("alsasink").build().unwrap();
    let fields = AudioSink::of(&sink).expect("an audiobasesink");
    // Zero as GObject made it; -1 from its first state change on.
    assert_eq!(fields.next_sample(), Some(0));
    assert_ne!(fields.clock_field(), 0, "made at its init");
    let fake = gst::ElementFactory::make("fakesink").build().unwrap();
    assert!(AudioSink::of(&fake).is_err(), "not an audiobasesink: not read");
    // The sink's clock, through the shim, is the one it provides once its
    // ring is acquired; the system clock is not taken for it.
    let system = gst::SystemClock::obtain();
    assert!(audio_clock(fields, &system).is_err());
    sink.set_property("device", "null");
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("audiotestsrc").property("num-buffers", 3i32).build().unwrap();
    pipeline.add_many([&src, &sink]).unwrap();
    src.link(&sink).unwrap();
    pipeline.set_state(gst::State::Paused).unwrap();
    let _ = pipeline.state(gst::ClockTime::from_seconds(10));
    let provided = sink.provide_clock().expect("acquired: a clock");
    let clock = audio_clock(fields, &provided).expect("the sink's own");
    assert_eq!(clock.time_offset(), 0);
    pipeline.set_state(gst::State::Null).unwrap();
}
