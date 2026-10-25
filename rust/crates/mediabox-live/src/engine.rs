//! The pipeline: one clock, one timeline, two outputs that stay open.
//!
//! ```text
//!  FIFO ─read─> appsrc (live, stamped on arrival) ─> tsdemux ─┬─ parse ─> mppvideodec ─> selector ─> appsink ─MBV1─> interface ─> VOP2
//!       \___________ one branch per channel _________________/└─ parse ─> decode/IEC ──> selector ─> alsasink ─> HDMI
//! ```
//!
//! The clock is the system's CLOCK_MONOTONIC (GstSystemClock), the clock the
//! interface's page-flip timestamps are in. The transport stream is a live
//! source: every buffer is stamped with the running time it arrived at, and
//! tsdemux, given a live TIME stream, estimates the broadcaster's clock from
//! the PCRs against those arrival times (mpegtspacketizer's skew estimate) and
//! puts every PTS on this clock. So the broadcast's own timeline is what both
//! sinks wait for -- no sink is the master, and no queue fills with a drift
//! between the broadcaster's 27 MHz and the board's crystal.
//!
//! The two sinks are made once and outlive every channel: the HDMI sound
//! device is not closed between channels (the ring buffer plays silence while
//! the next one locks), and the interface connection with its held frames is
//! the same one. A channel is a branch -- source, demux, parsers, decoders --
//! added in front of two input-selectors and switched to, while the branch
//! before it is taken down on a thread of its own.

use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gst::prelude::*;
use mediabox_core::AudioCodec;

use crate::plane::{Picture, Plane, Sent};

pub struct Options {
    /// The element that decodes video: `mppvideodec` on the appliance.
    pub video_decoder: String,
    pub audio: Audio,
    /// tsdemux's `latency`: how far a PTS may run ahead of its PCR.
    pub demux_latency_ms: u32,
    /// From the sink's wake-up to the light on the panel: the interface
    /// commits at its next vertical blank.
    pub render_delay_ms: u32,
    /// A frame later than this is not sent.
    pub max_lateness_ms: u32,
}

#[derive(Clone)]
pub struct Audio {
    /// The ALSA PCM; `None`: sound goes nowhere.
    pub device: Option<String>,
    pub passthrough: Vec<AudioCodec>,
    /// Linear gain: mpv's volume is cubic (`(v/100)^3`), and this keeps that.
    pub gain: f64,
    pub muted: bool,
    /// The sound setting's Dolby Digital encoding: (kbit/s, fewest channels
    /// encoded). Multichannel PCM is sent as AC-3; fewer channels stay PCM.
    pub ac3_encode: Option<(u32, u32)>,
}

/// What the control socket reads.
#[derive(Default)]
pub struct Shared {
    pub generation: AtomicU64,
    /// A frame (or, with no video, sound) has come out since the last open.
    pub playing: AtomicBool,
    pub eof: AtomicBool,
    pub paused: AtomicBool,
    pub frames: AtomicU64,
    pub late: AtomicU64,
    pub audio_buffers: AtomicU64,
    /// Running time minus the PTS of the frame just sent, ns: how far behind
    /// the clock the picture leaves for the panel.
    pub video_lag_ns: std::sync::atomic::AtomicI64,
    /// PTS minus running time as a sound buffer reaches the sound sink, ns:
    /// how early it is; the sink needs it ahead by the pipeline's latency.
    pub audio_lead_ns: std::sync::atomic::AtomicI64,
    pub info: Mutex<Info>,
}

#[derive(Default, Clone)]
pub struct Info {
    pub path: String,
    pub opened_at: Option<Instant>,
    pub first_frame_ms: Option<u64>,
    pub video_codec: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    pub interlaced: Option<String>,
    pub audio_codec: Option<String>,
    /// The sound as decoded, (rate, channels); unknown when it is passed
    /// through.
    pub audio_decoded: Option<(i32, i32)>,
    pub passthrough: bool,
    /// Every audio stream the programme carries, in PMT order.
    pub audio_tracks: Vec<(String, Option<String>)>,
    pub aid: Option<usize>,
}

/// One channel's elements.
struct Branch {
    generation: u64,
    elements: Arc<Mutex<Vec<gst::Element>>>,
    pads: Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>,
}

/// The FIFO being read, and the branch its stream goes into.
///
/// One reader per FIFO, not per branch: a channel opened again on the same
/// FIFO (an audio track, a resume, a branch that failed) is fed by the same
/// reader, which waits for the new branch with what it has read. With a
/// reader per branch, the old one, blocked in read(2) on the same
/// descriptor, took the next chunk and threw it away. The reader end stays
/// open throughout: the receiver ends when the last reader closes it (EPIPE).
struct Feed {
    path: String,
    into: Arc<Mutex<Option<gst_app::AppSrc>>>,
    /// Another FIFO is read now, or this one has ended: the next channel on
    /// this path needs a reader of its own.
    stop: Arc<AtomicBool>,
}

pub struct Engine {
    pipeline: gst::Pipeline,
    video_selector: gst::Element,
    audio_selector: gst::Element,
    audio_sink: Mutex<Option<gst::Element>>,
    /// The sound device is open for IEC 61937 (non-audio) rather than PCM.
    bitstream: AtomicBool,
    options: Options,
    audio: Mutex<Audio>,
    shared: Arc<Shared>,
    current: Mutex<Option<Branch>>,
    feed: Mutex<Option<Feed>>,
    /// The audio stream to play, by its order in the programme; `None`:
    /// no sound (mpv's `aid=no`, which the control plane sets around a
    /// display mode change).
    aid: Mutex<Option<usize>>,
    /// mpv's `audio-delay`, seconds: the sound sink's `ts-offset`.
    audio_delay: Mutex<f64>,
    trace: crate::trace::Trace,
    plane: Arc<Plane>,
    video_sink: gst_app::AppSink,
    /// The player cannot go on: the interface is gone, or the channel
    /// cannot be played at all. With the reason, for the control plane.
    fail: Box<dyn Fn(Option<String>) + Send + Sync>,
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| format!("GStreamer öğesi yok: {factory}"))
}

fn first_of(factories: &[&str]) -> Option<gst::Element> {
    factories.iter().find_map(|factory| gst::ElementFactory::make(factory).build().ok())
}

impl Engine {
    pub fn new(
        options: Options,
        plane: Arc<Plane>,
        shared: Arc<Shared>,
        trace: crate::trace::Trace,
        fail: impl Fn(Option<String>) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        let pipeline = gst::Pipeline::with_name("mediabox-live");
        // CLOCK_MONOTONIC: the interface's vblank timestamps are in it too.
        let clock = gst::SystemClock::obtain();
        clock.set_property("clock-type", gst::ClockType::Monotonic);
        pipeline.use_clock(Some(&clock));

        let video_selector = make("input-selector")?;
        video_selector.set_property("sync-streams", false);
        video_selector.set_property("cache-buffers", false);
        let audio_selector = make("input-selector")?;
        audio_selector.set_property("sync-streams", false);
        audio_selector.set_property("cache-buffers", false);

        let video_sink = gst_app::AppSink::builder()
            .name("video")
            .sync(true)
            .max_buffers(1)
            .drop(false)
            .enable_last_sample(false)
            .wait_on_eos(false)
            .qos(true)
            .build();
        video_sink.set_async(false);
        video_sink.set_max_lateness(i64::from(options.max_lateness_ms) * 1_000_000);
        video_sink.set_render_delay(gst::ClockTime::from_mseconds(u64::from(options.render_delay_ms)));

        pipeline
            .add_many([&video_selector, video_sink.upcast_ref(), &audio_selector])
            .map_err(|e| e.to_string())?;
        video_selector.link(&video_sink).map_err(|e| e.to_string())?;
        // A radio service has no picture: its sound is what says it plays.
        {
            let shared = Arc::clone(&shared);
            let pipeline_clock = clock.clone();
            let weak_pipeline = pipeline.downgrade();
            audio_selector.static_pad("src").expect("input-selector has a src pad").add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let (Some(buffer), Some(pipeline)) = (info.buffer(), weak_pipeline.upgrade())
                    && let (Some(pts), Some(base)) = (buffer.pts(), pipeline.base_time())
                {
                    let now = pipeline_clock.time().saturating_sub(base).nseconds() as i64;
                    shared.audio_lead_ns.store(pts.nseconds() as i64 - now, Ordering::Relaxed);
                }
                if shared.audio_buffers.fetch_add(1, Ordering::Relaxed) > 0
                    && shared.info.lock().unwrap_or_else(|p| p.into_inner()).video_codec.is_none()
                {
                    shared.playing.store(true, Ordering::SeqCst);
                }
                gst::PadProbeReturn::Ok
            });
        }

        let engine = Arc::new(Self {
            pipeline,
            video_selector,
            audio_selector,
            audio_sink: Mutex::new(None),
            bitstream: AtomicBool::new(false),
            audio: Mutex::new(options.audio.clone()),
            options,
            shared,
            current: Mutex::new(None),
            feed: Mutex::new(None),
            aid: Mutex::new(Some(0)),
            audio_delay: Mutex::new(0.0),
            trace: trace.clone(),
            plane: Arc::clone(&plane),
            video_sink: video_sink.clone(),
            fail: Box::new(fail),
        });
        engine.attach_audio_sink()?;
        engine.attach_video(&video_sink, plane, trace);
        engine
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("pipeline başlatılamadı: {e}"))?;
        Ok(engine)
    }

    pub fn bus(&self) -> gst::Bus {
        self.pipeline.bus().expect("a pipeline has a bus")
    }

    /// The sound device, opened once and kept: alsasink on the plan's PCM,
    /// slaved to the pipeline's clock.
    fn attach_audio_sink(&self) -> Result<(), String> {
        let audio = self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let sink = match &audio.device {
            Some(device) => {
                let sink = make("alsasink")?;
                // A bitstream goes out with the channel status saying so
                // (non-audio, no copyright, original, 48 kHz), in the form ALSA
                // parses -- mpv's. GStreamer's own IEC958 open appends
                // ":{AES0 ...}" to the name, which on "hdmi:CARD=x,DEV=0"
                // ALSA refuses ("Parameter DEV must be an integer", measured
                // 2026-10-24), so alsasink never offers AC-3 on this board.
                let device = if self.bitstream.load(Ordering::SeqCst) {
                    format!("{device},AES0=0x06,AES1=0x82,AES2=0x00,AES3=0x02")
                } else {
                    device.clone()
                };
                sink.set_property("device", &device);
                // Lower than the default 200 ms: the latency of the whole
                // pipeline is the largest of its sinks'.
                sink.set_property("buffer-time", 100_000i64);
                sink.set_property("latency-time", 10_000i64);
                sink
            }
            None => make("fakesink")?,
        };
        sink.set_property("sync", true);
        sink.set_property("async", false);
        sink.set_property("enable-last-sample", false);
        sink.set_property("ts-offset", (*self.audio_delay.lock().unwrap_or_else(|p| p.into_inner()) * 1e9) as i64);
        self.pipeline.add(&sink).map_err(|e| e.to_string())?;
        self.audio_selector.link(&sink).map_err(|e| e.to_string())?;
        sink.sync_state_with_parent().map_err(|e| e.to_string())?;
        *self.audio_sink.lock().unwrap_or_else(|p| p.into_inner()) = Some(sink);
        Ok(())
    }

    fn detach_audio_sink(&self) {
        let Some(sink) = self.audio_sink.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return;
        };
        self.audio_selector.unlink(&sink);
        let _ = sink.set_state(gst::State::Null);
        let _ = self.pipeline.remove(&sink);
    }

    /// Open the sound device for a bitstream or for PCM, if it is not already.
    fn sound_mode(&self, bitstream: bool) {
        if self.bitstream.swap(bitstream, Ordering::SeqCst) == bitstream {
            return;
        }
        self.detach_audio_sink();
        if let Err(error) = self.attach_audio_sink() {
            eprintln!("mediabox-live: ses aygıtı açılamadı: {error}");
        }
    }

    /// The sound device failed under us -- a display mode change takes the
    /// HDMI sound device away ("Device lost") -- or was asked off.
    pub fn reopen_audio_sink(&self) {
        self.detach_audio_sink();
        if self.aid.lock().unwrap_or_else(|p| p.into_inner()).is_none() {
            return;
        }
        if let Err(error) = self.attach_audio_sink() {
            eprintln!("mediabox-live: ses aygıtı yeniden açılamadı: {error}");
        }
    }

    pub fn is_audio_sink(&self, object: &gst::Object) -> bool {
        self.audio_sink
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|sink| object == sink.upcast_ref::<gst::Object>() || object.has_as_ancestor(sink))
    }

    fn attach_video(self: &Arc<Self>, sink: &gst_app::AppSink, plane: Arc<Plane>, trace: crate::trace::Trace) {
        let shared = Arc::clone(&self.shared);
        let mut last_caps: Option<gst::Caps> = None;
        let mut described: Option<Described> = None;
        let mut described_for = 0u64;
        // The last frame shown, by channel: a picture is never put up before
        // one already shown. While tsdemux's estimate of the broadcaster's
        // clock settles -- the first second or two, and the receiver starts
        // each channel with a burst from its last access point -- a PTS can
        // map a few milliseconds earlier than the one before it.
        let mut last: Option<(u64, u64)> = None;
        let engine = Arc::downgrade(self);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let Some(buffer) = sample.buffer() else { return Ok(gst::FlowSuccess::Ok) };
                    let pts = buffer.pts().map(|pts| pts.nseconds()).unwrap_or(0);
                    // Described again for every channel, not only when the caps
                    // change: opening the same kind of channel again reset
                    // what the control socket reports (size 0x0, no rate)
                    // and the plane's HDR curve.
                    let channel = shared.generation.load(Ordering::SeqCst);
                    if let Some(caps) = sample.caps()
                        && (last_caps.as_ref() != Some(&caps.to_owned()) || described_for != channel)
                    {
                        described_for = channel;
                        last_caps = Some(caps.to_owned());
                        described = describe(caps);
                        if let Some(found) = &described {
                            plane.send_hdr(found.eotf);
                            let mut info = shared.info.lock().unwrap_or_else(|p| p.into_inner());
                            info.width = found.width;
                            info.height = found.height;
                            info.fps = found.fps;
                            info.interlaced = Some(found.interlace.clone());
                            eprintln!(
                                "mediabox-live: görüntü {}x{} {} {:?} fps, {}",
                                found.width, found.height, found.format, found.fps, found.interlace
                            );
                        }
                    }
                    let generation = shared.generation.load(Ordering::SeqCst);
                    if last.is_none_or(|(shown_in, _)| shown_in != generation) {
                        trace.event('P', generation, pts / 1000);
                    }
                    if let Some((shown_in, shown_pts)) = last
                        && shown_in == generation
                        && pts <= shown_pts
                    {
                        trace.event('M', 0, pts / 1000);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    last = Some((generation, pts));
                    let Some(picture) = described.as_ref().and_then(|found| picture(found, buffer)) else {
                        // Not a dma-buf the plane takes: a software decoder.
                        // Traced, never copied -- the whole point is that
                        // nothing is.
                        trace.event('X', 0, pts / 1000);
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    if let (Some(clock), Some(base)) = (sink.clock(), sink.base_time()) {
                        let now = clock.time().saturating_sub(base).nseconds() as i64;
                        shared.video_lag_ns.store(now - pts as i64, Ordering::Relaxed);
                    }
                    match plane.send(&picture, sample.clone(), pts) {
                        Sent::Shown => {
                            shared.frames.fetch_add(1, Ordering::Relaxed);
                            if !shared.playing.swap(true, Ordering::SeqCst) {
                                let mut info = shared.info.lock().unwrap_or_else(|p| p.into_inner());
                                info.first_frame_ms = info.opened_at.map(|at| at.elapsed().as_millis() as u64);
                                eprintln!("mediabox-live: ilk kare {:?} ms", info.first_frame_ms);
                            }
                        }
                        Sent::Dropped => {}
                        Sent::Gone => {
                            if let Some(engine) = engine.upgrade() {
                                (engine.fail)(None);
                            }
                            return Err(gst::FlowError::Flushing);
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    /// Open the channel on `path`: a new branch, switched to, and the one
    /// before it taken down.
    pub fn open(self: &Arc<Self>, path: &str) -> Result<(), String> {
        let generation = self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.trace.event('G', generation, 0);
        self.shared.playing.store(false, Ordering::SeqCst);
        self.shared.eof.store(false, Ordering::SeqCst);
        *self.shared.info.lock().unwrap_or_else(|p| p.into_inner()) =
            Info { path: path.to_string(), opened_at: Some(Instant::now()), ..Info::default() };
        let mut feed = self.feed.lock().unwrap_or_else(|p| p.into_inner());
        if feed.as_ref().is_some_and(|feed| feed.path != path || feed.stop.load(Ordering::SeqCst))
            && let Some(old) = feed.take()
        {
            old.stop.store(true, Ordering::SeqCst);
            *old.into.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        // The reader of this FIFO, if it goes on, waits here with what it
        // has read until the new branch takes it.
        let into = feed.as_ref().map(|feed| Arc::clone(&feed.into)).unwrap_or_default();
        let mut target = into.lock().unwrap_or_else(|p| p.into_inner());
        *target = None;
        if let Some(old) = self.current.lock().unwrap_or_else(|p| p.into_inner()).take() {
            self.take_down(old);
            self.blank();
        }
        let (branch, source) = self.build(generation)?;
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = Some(branch);
        *target = Some(source);
        drop(target);
        if feed.is_none() {
            let stop = Arc::new(AtomicBool::new(false));
            let (reader_into, reader_stop, shared, reader_path) =
                (Arc::clone(&into), Arc::clone(&stop), Arc::clone(&self.shared), path.to_string());
            std::thread::Builder::new()
                .name("ts-reader".into())
                .spawn(move || {
                    read_stream(&reader_path, &reader_into, &reader_stop, &shared);
                    reader_stop.store(true, Ordering::SeqCst);
                })
                .map_err(|e| e.to_string())?;
            *feed = Some(Feed { path: path.to_string(), into, stop });
        }
        Ok(())
    }

    /// Between channels the panel is dark, as a receiver's is while it tunes,
    /// and nothing of the channel before reaches it: the frame the sink was
    /// holding for its time is flushed, and the picture is taken off.
    ///
    /// The interface depends on this. It counts a channel's first frame only
    /// after the plane has gone dark (`previous_on_plane` in mediabox-tv);
    /// with the last frame left up, measured 2026-10-24, it took the old
    /// frame for the new channel's ("first frame 56 ms after the key"), then
    /// waited 30 s for a dark plane that never came and stopped the player.
    fn blank(&self) {
        if let Some(pad) = self.video_sink.static_pad("sink") {
            pad.send_event(gst::event::FlushStart::new());
            pad.send_event(gst::event::FlushStop::new(false));
        }
        self.plane.stop();
    }

    fn build(self: &Arc<Self>, generation: u64) -> Result<(Branch, gst_app::AppSrc), String> {
        let source = gst_app::AppSrc::builder()
            .name(format!("ts{generation}"))
            .is_live(true)
            .do_timestamp(true)
            .format(gst::Format::Time)
            .stream_type(gst_app::AppStreamType::Stream)
            .caps(&gst::Caps::builder("video/mpegts").field("systemstream", true).field("packetsize", 188i32).build())
            .max_bytes(4 * 1024 * 1024)
            .block(true)
            .build();
        source.set_property("min-latency", 0i64);
        let demux = make("tsdemux")?;
        demux.set_property("latency", self.options.demux_latency_ms as i32);
        demux.set_property("emit-stats", false);
        // The receiver's FIFO carries one service; a whole multiplex (a
        // recording) is narrowed to one here.
        if let Some(program) = std::env::var("MEDIABOX_LIVE_PROGRAM").ok().and_then(|value| value.parse::<i32>().ok()) {
            demux.set_property("program-number", program);
        }

        let elements = Arc::new(Mutex::new(vec![source.clone().upcast::<gst::Element>(), demux.clone()]));
        let pads = Arc::new(Mutex::new(Vec::new()));
        self.adopt(&[source.upcast_ref(), &demux])?;
        source.link(&demux).map_err(|e| e.to_string())?;

        let weak = Arc::downgrade(self);
        let audio_order = Arc::new(Mutex::new(0usize));
        let video_taken = Arc::new(AtomicBool::new(false));
        {
            let elements = Arc::clone(&elements);
            let pads = Arc::clone(&pads);
            demux.connect_pad_added(move |_, pad| {
                let Some(engine) = weak.upgrade() else { return };
                if engine.shared.generation.load(Ordering::SeqCst) != generation {
                    engine.discard(pad, &elements);
                    return;
                }
                if let Err(error) = engine.stream(generation, pad, &elements, &pads, &audio_order, &video_taken) {
                    eprintln!("mediabox-live: {}: {error}", pad.name());
                    engine.discard(pad, &elements);
                    // Without its picture the channel is not played: said,
                    // not taken for a radio service by its sound.
                    if pad.name().starts_with("video") {
                        (engine.fail)(Some(format!("Kanalın görüntüsü çözülemiyor: {error}")));
                    }
                }
            });
        }
        for element in [source.upcast_ref::<gst::Element>(), &demux] {
            element.sync_state_with_parent().map_err(|e| e.to_string())?;
        }
        Ok((Branch { generation, elements, pads }, source))
    }

    /// Put elements into the running pipeline on its clock and timeline.
    fn adopt(&self, elements: &[&gst::Element]) -> Result<(), String> {
        self.pipeline.add_many(elements.iter().copied()).map_err(|e| e.to_string())?;
        let clock = self.pipeline.clock();
        let base = self.pipeline.base_time();
        for element in elements {
            if let Some(clock) = &clock {
                let _ = element.set_clock(Some(clock));
            }
            if let Some(base) = base {
                element.set_base_time(base);
            }
        }
        Ok(())
    }

    /// A stream the programme carries that is not played: drained.
    fn discard(&self, pad: &gst::Pad, elements: &Arc<Mutex<Vec<gst::Element>>>) {
        let Ok(sink) = make("fakesink") else { return };
        sink.set_property("sync", false);
        sink.set_property("async", false);
        if self.adopt(&[&sink]).is_err() {
            return;
        }
        let _ = pad.link(&sink.static_pad("sink").expect("fakesink has a sink pad"));
        let _ = sink.sync_state_with_parent();
        elements.lock().unwrap_or_else(|p| p.into_inner()).push(sink);
    }

    fn stream(
        self: &Arc<Self>,
        generation: u64,
        pad: &gst::Pad,
        elements: &Arc<Mutex<Vec<gst::Element>>>,
        pads: &Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>,
        audio_order: &Mutex<usize>,
        video_taken: &AtomicBool,
    ) -> Result<(), String> {
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let Some(structure) = caps.structure(0) else {
            return Err("biçimi yok".into());
        };
        let name = structure.name().as_str();
        let version = structure.get::<i32>("mpegversion").ok();
        if name.starts_with("video/") {
            if video_taken.swap(true, Ordering::SeqCst) {
                self.discard(pad, elements);
                return Ok(());
            }
            let parser = match name {
                "video/x-h264" => "h264parse",
                "video/x-h265" => "h265parse",
                "video/mpeg" => "mpegvideoparse",
                _ => {
                    self.discard(pad, elements);
                    return Ok(());
                }
            };
            let parser = make(parser)?;
            let decoder = make(&self.options.video_decoder)?;
            // MPP marks a frame interlaced, and starts its deinterlacer, only
            // when the parser is not in fast mode -- ffmpeg-rockchip turns it
            // off for interlaced video (rkmppdec.c), gstreamer-rockchip leaves
            // it on. A broadcast is interlaced more often than not.
            if decoder.find_property("fast-mode").is_some() {
                decoder.set_property("fast-mode", false);
            }
            // A frame the decoder hands over after a newer channel was asked
            // for is the old channel's, whatever it says: never shown, never
            // counted as the new one's first.
            if let Some(out) = decoder.static_pad("src") {
                let shared = Arc::clone(&self.shared);
                out.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    if shared.generation.load(Ordering::SeqCst) == generation {
                        gst::PadProbeReturn::Ok
                    } else {
                        gst::PadProbeReturn::Drop
                    }
                });
            }
            self.chain(pad, &[parser, decoder], &self.video_selector, elements, pads)?;
            self.shared.info.lock().unwrap_or_else(|p| p.into_inner()).video_codec = Some(codec_name(name, version));
            return Ok(());
        }
        if !name.starts_with("audio/") {
            self.discard(pad, elements);
            return Ok(());
        }
        let order = {
            let mut order = audio_order.lock().unwrap_or_else(|p| p.into_inner());
            *order += 1;
            *order - 1
        };
        let language = pad.sticky_event::<gst::event::Tag>(0).and_then(|tag| {
            tag.tag().get::<gst::tags::LanguageCode>().map(|value| value.get().to_string())
        });
        let codec = codec_name(name, version);
        {
            let mut info = self.shared.info.lock().unwrap_or_else(|p| p.into_inner());
            info.audio_tracks.push((codec.clone(), language));
        }
        let wanted = *self.aid.lock().unwrap_or_else(|p| p.into_inner());
        if wanted != Some(order) {
            self.discard(pad, elements);
            return Ok(());
        }
        let audio = self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let passthrough_codec = match name {
            "audio/x-ac3" => Some(AudioCodec::Ac3),
            "audio/x-eac3" => Some(AudioCodec::Eac3),
            _ => None,
        };
        let parser = match (name, version) {
            ("audio/mpeg", Some(1)) => "mpegaudioparse",
            ("audio/mpeg", _) => "aacparse",
            ("audio/x-ac3" | "audio/x-eac3", _) => "ac3parse",
            _ => {
                self.discard(pad, elements);
                return Ok(());
            }
        };
        // AC-3 goes out as it came when the setting passes it through; E-AC-3
        // needs another burst type and is made AC-3 below.
        let passthrough = passthrough_codec == Some(AudioCodec::Ac3)
            && audio.passthrough.contains(&AudioCodec::Ac3)
            && audio.device.is_some();
        let mut chain = vec![make(parser)?];
        if passthrough {
            let head = self.head(pad, &chain, elements, false)?;
            let framed = head.last().expect("a chain has elements").static_pad("src").ok_or("ayrıştırıcının çıkışı yok")?;
            self.sound_mode(true);
            self.bitstream_tail(&framed, None, elements, pads)?;
            for element in head.iter().rev() {
                element.sync_state_with_parent().map_err(|e| e.to_string())?;
            }
            pad.link(&head[0].static_pad("sink").ok_or("kuyruğun girişi yok")?).map_err(|e| format!("{e:?}"))?;
            eprintln!("mediabox-live: ses AC-3 olduğu gibi (IEC 61937)");
            let mut info = self.shared.info.lock().unwrap_or_else(|p| p.into_inner());
            info.audio_codec = Some(codec);
            info.passthrough = true;
            info.aid = Some(order);
            return Ok(());
        }
        let decoder = match (name, version) {
            ("audio/mpeg", Some(1)) => first_of(&["mpg123audiodec", "avdec_mp2float", "avdec_mp3"]),
            ("audio/mpeg", _) => first_of(&["avdec_aac", "fdkaacdec", "faad"]),
            ("audio/x-ac3", _) => first_of(&["a52dec", "avdec_ac3"]),
            ("audio/x-eac3", _) => first_of(&["avdec_eac3"]),
            _ => None,
        }
        .ok_or_else(|| format!("{codec} için ses çözücüsü yok"))?;
        chain.push(decoder);
        let head = self.head(pad, &chain, elements, false)?;
        let decoded = head.last().expect("a chain has elements").static_pad("src").ok_or("çözücünün çıkışı yok")?;
        // How many channels come out is known on the decoder's first caps,
        // and that decides the rest: the sound setting's Dolby Digital
        // encoding for three channels or more -- mpv's
        // `lavcac3enc=tospdif=yes:minch=N`, which this replaces -- and
        // PCM below that.
        let weak = Arc::downgrade(self);
        let (elements_tail, pads_tail) = (Arc::clone(elements), Arc::clone(pads));
        decoded.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |pad, info| {
            let Some(gst::PadProbeData::Event(event)) = &info.data else { return gst::PadProbeReturn::Ok };
            let gst::EventView::Caps(caps) = event.view() else { return gst::PadProbeReturn::Ok };
            let Some(engine) = weak.upgrade() else { return gst::PadProbeReturn::Remove };
            // A channel left before its sound began: the sound device is
            // the new channel's to open, not this one's.
            if engine.shared.generation.load(Ordering::SeqCst) != generation {
                return gst::PadProbeReturn::Remove;
            }
            let structure = caps.caps().structure(0);
            let channels = structure.and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(2);
            let rate = structure.and_then(|s| s.get::<i32>("rate").ok()).unwrap_or(0);
            engine.shared.info.lock().unwrap_or_else(|p| p.into_inner()).audio_decoded = Some((rate, channels));
            let audio = engine.audio.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let encode = audio.ac3_encode.filter(|(_, min)| channels >= *min as i32);
            engine.sound_mode(encode.is_some());
            if let Some((kbps, _)) = encode {
                eprintln!("mediabox-live: ses {channels} kanal -> Dolby Digital {kbps} kb/s (IEC 61937)");
                if let Err(error) = engine.bitstream_tail(pad, Some(kbps), &elements_tail, &pads_tail) {
                    eprintln!("mediabox-live: ses zinciri kurulamadı: {error}");
                }
                engine.shared.info.lock().unwrap_or_else(|p| p.into_inner()).passthrough = true;
                return gst::PadProbeReturn::Remove;
            }
            let tail = match engine.sound_tail(&audio) {
                Ok(tail) => tail,
                Err(error) => {
                    eprintln!("mediabox-live: ses zinciri kurulamadı: {error}");
                    return gst::PadProbeReturn::Remove;
                }
            };
            eprintln!("mediabox-live: ses {channels} kanal -> PCM");
            if let Err(error) = engine.tail(pad, &tail, &engine.audio_selector, &elements_tail, &pads_tail) {
                eprintln!("mediabox-live: ses zinciri bağlanamadı: {error}");
            }
            gst::PadProbeReturn::Remove
        });
        for element in head.iter().rev() {
            element.sync_state_with_parent().map_err(|e| e.to_string())?;
        }
        pad.link(&head[0].static_pad("sink").ok_or("kuyruğun girişi yok")?)
            .map_err(|e| format!("{e:?}"))?;
        let mut info = self.shared.info.lock().unwrap_or_else(|p| p.into_inner());
        info.audio_codec = Some(codec);
        info.aid = Some(order);
        Ok(())
    }

    /// After the decoder: AC-3 at `kbps`, made into IEC 61937 bursts here and
    /// played as the 2-channel 16-bit 48 kHz PCM they look like, on a device
    /// opened non-audio (`sound_mode`). The bursts keep the frames' times, so
    /// the sound sink waits for them as for any other sound.
    fn bitstream_tail(
        self: &Arc<Self>,
        from: &gst::Pad,
        kbps: Option<u32>,
        elements: &Arc<Mutex<Vec<gst::Element>>>,
        pads: &Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>,
    ) -> Result<(), String> {
        let frames = gst_app::AppSink::builder().sync(false).enable_last_sample(false).max_buffers(0).build();
        frames.set_async(false);
        // `None`: the broadcast's own AC-3, framed by its parser, as it is.
        let tail = match kbps {
            Some(kbps) => {
                let rate = make("capsfilter")?;
                rate.set_property("caps", gst::Caps::builder("audio/x-raw").field("rate", 48000i32).build());
                let encoder = make("avenc_ac3")?;
                encoder.set_property("bitrate", (kbps * 1000) as i32);
                vec![make("audioconvert")?, make("audioresample")?, rate, encoder, make("ac3parse")?, frames.clone().upcast()]
            }
            None => vec![frames.clone().upcast()],
        };
        let refs: Vec<&gst::Element> = tail.iter().collect();
        self.adopt(&refs)?;
        elements.lock().unwrap_or_else(|p| p.into_inner()).extend(tail.iter().cloned());
        gst::Element::link_many(tail.iter()).map_err(|e| e.to_string())?;
        log_caps(tail.iter());

        let bursts = gst_app::AppSrc::builder()
            .format(gst::Format::Time)
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "S16LE")
                    .field("layout", "interleaved")
                    .field("rate", 48000i32)
                    .field("channels", 2i32)
                    .build(),
            )
            .block(true)
            .build();
        self.adopt(&[bursts.upcast_ref()])?;
        elements.lock().unwrap_or_else(|p| p.into_inner()).push(bursts.clone().upcast());
        let input = self.audio_selector.request_pad_simple("sink_%u").ok_or("seçicide giriş yok")?;
        pads.lock().unwrap_or_else(|p| p.into_inner()).push((self.audio_selector.clone(), input.clone()));
        bursts.static_pad("src").ok_or("kaynağın çıkışı yok")?.link(&input).map_err(|e| format!("{e:?}"))?;

        let feed = bursts.clone();
        frames.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let (Some(buffer), Some(segment)) = (sample.buffer(), sample.segment()) else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let Ok(map) = buffer.map_readable() else { return Ok(gst::FlowSuccess::Ok) };
                    let Some(burst) = iec61937_ac3(&map) else { return Ok(gst::FlowSuccess::Ok) };
                    // In running time: the bursts' own segment starts at 0.
                    let running = segment
                        .downcast_ref::<gst::ClockTime>()
                        .and_then(|segment| buffer.pts().and_then(|pts| segment.to_running_time(pts)));
                    let mut out = gst::Buffer::from_mut_slice(burst);
                    {
                        let out = out.get_mut().expect("a new buffer is writable");
                        out.set_pts(running);
                        out.set_duration(gst::ClockTime::from_mseconds(32));
                    }
                    feed.push_buffer(out).map(|_| gst::FlowSuccess::Ok)
                })
                .build(),
        );
        for element in tail.iter().rev() {
            element.sync_state_with_parent().map_err(|e| e.to_string())?;
        }
        bursts.sync_state_with_parent().map_err(|e| e.to_string())?;
        from.link(&tail[0].static_pad("sink").ok_or("zincirin girişi yok")?).map_err(|e| format!("{e:?}"))?;
        self.audio_selector.set_property("active-pad", &input);
        Ok(())
    }

    /// After the decoder, for PCM: converted, resampled, at the volume.
    fn sound_tail(&self, audio: &Audio) -> Result<Vec<gst::Element>, String> {
        let volume = make("volume")?;
        volume.set_property("volume", audio.gain);
        volume.set_property("mute", audio.muted);
        Ok(vec![make("audioconvert")?, make("audioresample")?, volume])
    }

    /// `pad` -> `chain` -> a new input of `selector`, which is switched to.
    /// `pad` -> `chain` -> a new input of `selector`, which is switched to.
    fn chain(
        &self,
        pad: &gst::Pad,
        chain: &[gst::Element],
        selector: &gst::Element,
        elements: &Arc<Mutex<Vec<gst::Element>>>,
        pads: &Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>,
    ) -> Result<(), String> {
        let head = self.head(pad, chain, elements, false)?;
        let last = head.last().expect("a chain has elements").static_pad("src").ok_or("zincirin çıkışı yok")?;
        self.tail(&last, &[], selector, elements, pads)?;
        for element in head.iter().rev() {
            element.sync_state_with_parent().map_err(|e| e.to_string())?;
        }
        pad.link(&head[0].static_pad("sink").ok_or("kuyruğun girişi yok")?)
            .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }

    /// `pad` -> queue -> `chain`, linked, in the pipeline. With `start`, set
    /// going and fed: its end is linked later (`tail`).
    fn head(
        &self,
        pad: &gst::Pad,
        chain: &[gst::Element],
        elements: &Arc<Mutex<Vec<gst::Element>>>,
        start: bool,
    ) -> Result<Vec<gst::Element>, String> {
        // Each stream on a thread of its own. Without this, tsdemux pushes
        // picture and sound from one thread: the sound sink's wait for the
        // clock held the decoder's input up and MPP's full input held the
        // sound up -- measured live, sound reached its sink 1.35 s after its
        // PTS against a latency of 0.83 s, and the device played silence.
        let queue = make("queue")?;
        queue.set_property("max-size-buffers", 0u32);
        queue.set_property("max-size-bytes", 0u32);
        queue.set_property("max-size-time", 3_000_000_000u64);
        let chain: Vec<gst::Element> = std::iter::once(queue).chain(chain.iter().cloned()).collect();
        let refs: Vec<&gst::Element> = chain.iter().collect();
        self.adopt(&refs)?;
        elements.lock().unwrap_or_else(|p| p.into_inner()).extend(chain.iter().cloned());
        gst::Element::link_many(chain.iter()).map_err(|e| e.to_string())?;
        log_caps(chain.iter().skip(1));
        if start {
            for element in chain.iter().rev() {
                element.sync_state_with_parent().map_err(|e| e.to_string())?;
            }
            pad.link(&chain[0].static_pad("sink").ok_or("kuyruğun girişi yok")?)
                .map_err(|e| format!("{e:?}"))?;
        }
        Ok(chain)
    }

    /// `from` -> `tail` -> a new input of `selector`, which is switched to.
    fn tail(
        &self,
        from: &gst::Pad,
        tail: &[gst::Element],
        selector: &gst::Element,
        elements: &Arc<Mutex<Vec<gst::Element>>>,
        pads: &Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>,
    ) -> Result<(), String> {
        if !tail.is_empty() {
            let refs: Vec<&gst::Element> = tail.iter().collect();
            self.adopt(&refs)?;
            elements.lock().unwrap_or_else(|p| p.into_inner()).extend(tail.iter().cloned());
            gst::Element::link_many(tail.iter()).map_err(|e| e.to_string())?;
            log_caps(tail.iter());
        }
        let input = selector.request_pad_simple("sink_%u").ok_or("seçicide giriş yok")?;
        pads.lock().unwrap_or_else(|p| p.into_inner()).push((selector.clone(), input.clone()));
        let out = match tail.last() {
            Some(last) => last.static_pad("src").ok_or("zincirin çıkışı yok")?,
            None => from.clone(),
        };
        out.link(&input).map_err(|e| format!("{e:?}"))?;
        for element in tail.iter().rev() {
            element.sync_state_with_parent().map_err(|e| e.to_string())?;
        }
        if let Some(first) = tail.first() {
            from.link(&first.static_pad("sink").ok_or("zincirin girişi yok")?).map_err(|e| format!("{e:?}"))?;
        }
        selector.set_property("active-pad", &input);
        Ok(())
    }

    /// Take a branch down without holding anything else up: its reader is
    /// told to stop, its elements go to NULL on a thread of their own, and
    /// the selector inputs it had are given back.
    fn take_down(&self, branch: Branch) {
        // Off the selectors at once: what the old channel still has decoded
        // is not shown after the viewer has asked for the next one. The panel
        // keeps the last frame and the sound device plays silence until the
        // new channel's first.
        for (_, pad) in branch.pads.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            if let Some(peer) = pad.peer() {
                let _ = peer.unlink(pad);
            }
        }
        let pipeline = self.pipeline.clone();
        let _ = std::thread::Builder::new().name(format!("teardown-{}", branch.generation)).spawn(move || {
            let elements = std::mem::take(&mut *branch.elements.lock().unwrap_or_else(|p| p.into_inner()));
            for element in &elements {
                let _ = element.set_state(gst::State::Null);
            }
            for element in &elements {
                let _ = pipeline.remove(element);
            }
            for (selector, pad) in branch.pads.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
                selector.release_request_pad(&pad);
            }
        });
    }

    /// Whether `object` is part of the channel being played: an error from a
    /// branch being taken down is that branch's business, not the viewer's.
    pub fn is_current(&self, object: &gst::Object) -> bool {
        let current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        let Some(branch) = current.as_ref() else { return false };
        branch
            .elements
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|element| object == element.upcast_ref::<gst::Object>() || object.has_as_ancestor(element))
    }

    pub fn current_path(&self) -> Option<String> {
        let info = self.shared.info.lock().unwrap_or_else(|p| p.into_inner());
        (!info.path.is_empty()).then(|| info.path.clone())
    }

    /// `aid`: an audio stream by its order, or none. Played from the branch's
    /// own start: the same FIFO, a new branch.
    pub fn set_aid(self: &Arc<Self>, aid: Option<usize>) {
        let before = std::mem::replace(&mut *self.aid.lock().unwrap_or_else(|p| p.into_inner()), aid);
        if before == aid {
            return;
        }
        match aid {
            None => self.detach_audio_sink(),
            Some(_) => {
                if self.audio_sink.lock().unwrap_or_else(|p| p.into_inner()).is_none()
                    && let Err(error) = self.attach_audio_sink()
                {
                    eprintln!("mediabox-live: ses aygıtı açılamadı: {error}");
                }
            }
        }
        if (before.is_some() && aid.is_some() || before.is_none())
            && let Some(path) = self.current_path()
            && let Err(error) = self.open(&path)
        {
            (self.fail)(Some(error));
        }
    }

    pub fn aid(&self) -> Option<usize> {
        *self.aid.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_volume(&self, gain: Option<f64>, muted: Option<bool>) {
        let mut audio = self.audio.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(gain) = gain {
            audio.gain = gain;
        }
        if let Some(muted) = muted {
            audio.muted = muted;
        }
        let elements = self
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|branch| branch.elements.lock().unwrap_or_else(|p| p.into_inner()).clone())
            .unwrap_or_default();
        for element in elements {
            if element.factory().is_some_and(|factory| factory.name() == "volume") {
                element.set_property("volume", audio.gain);
                element.set_property("mute", audio.muted);
            }
        }
    }

    pub fn set_audio_delay(&self, seconds: f64) {
        let seconds = seconds.clamp(-60.0, 60.0);
        *self.audio_delay.lock().unwrap_or_else(|p| p.into_inner()) = seconds;
        if let Some(sink) = self.audio_sink.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            sink.set_property("ts-offset", (seconds * 1e9) as i64);
        }
    }

    pub fn audio_delay(&self) -> f64 {
        *self.audio_delay.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The sound device is opened for IEC 61937 bursts (AC-3 passed through
    /// or encoded), not PCM.
    pub fn bitstream(&self) -> bool {
        self.bitstream.load(Ordering::SeqCst)
    }

    /// What the sound device was opened with: (format, rate, channels).
    pub fn audio_caps(&self) -> Option<(String, i32, i32)> {
        let sink = self.audio_sink.lock().unwrap_or_else(|p| p.into_inner()).clone()?;
        let caps = sink.static_pad("sink")?.current_caps()?;
        let structure = caps.structure(0)?;
        let format = structure.get::<String>("format").unwrap_or_else(|_| structure.name().to_string());
        Some((format, structure.get::<i32>("rate").unwrap_or(0), structure.get::<i32>("channels").unwrap_or(0)))
    }

    pub fn audio(&self) -> Audio {
        self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A live channel does not stand still: paused, nothing is shown; let go,
    /// it opens again where the broadcast is.
    pub fn set_paused(self: &Arc<Self>, paused: bool) {
        if self.shared.paused.swap(paused, Ordering::SeqCst) == paused {
            return;
        }
        if paused {
            let _ = self.pipeline.set_state(gst::State::Paused);
        } else {
            let _ = self.pipeline.set_state(gst::State::Playing);
            if let Some(path) = self.current_path()
                && let Err(error) = self.open(&path)
            {
                (self.fail)(Some(error));
            }
        }
    }

    /// The pipeline's running time: where the broadcast is on this clock.
    pub fn running_time(&self) -> Option<f64> {
        let clock = self.pipeline.clock()?;
        let base = self.pipeline.base_time()?;
        Some(clock.time().saturating_sub(base).nseconds() as f64 / 1e9)
    }

    /// The pipeline's latency, as its elements report it now.
    pub fn latency(&self) -> Option<f64> {
        let mut query = gst::query::Latency::new();
        self.pipeline.query(&mut query).then_some(())?;
        let (_, min, _) = query.result();
        Some(min.nseconds() as f64 / 1e9)
    }

    pub fn recalculate_latency(&self) {
        let _ = self.pipeline.recalculate_latency();
    }

    pub fn shutdown(&self) {
        if let Some(feed) = self.feed.lock().unwrap_or_else(|p| p.into_inner()).take() {
            feed.stop.store(true, Ordering::SeqCst);
            *feed.into.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        self.current.lock().unwrap_or_else(|p| p.into_inner()).take();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Read the receiver's FIFO into the current branch's source, as fast as it
/// comes: every buffer is stamped with the time it arrived. The end of the
/// FIFO is the stream's end only while it is the FIFO being read: the
/// receiver lets the reader of a FIFO it has moved away from go that way.
fn read_stream(path: &str, into: &Mutex<Option<gst_app::AppSrc>>, stop: &AtomicBool, shared: &Shared) {
    let ended = |why: String| {
        if !stop.load(Ordering::SeqCst) {
            eprintln!("mediabox-live: {why}");
            shared.eof.store(true, Ordering::SeqCst);
        }
    };
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return ended(format!("{path} açılamadı: {error}")),
    };
    let mut chunk = vec![0u8; 188 * 348];
    loop {
        let read = match file.read(&mut chunk) {
            Ok(0) => return ended(format!("yayın akışı bitti ({path})")),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return ended(format!("{path} okunamadı: {error}")),
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        // Cloned out of the lock: a full source blocks the push, and the
        // next channel must still be able to take the reader over.
        let Some(source) = into.lock().unwrap_or_else(|p| p.into_inner()).clone() else { continue };
        // A source taken down under the push refuses it; the reader goes on
        // for the branch after it.
        let _ = source.push_buffer(gst::Buffer::from_slice(chunk[..read].to_vec()));
    }
}

/// One AC-3 frame as an IEC 61937 burst (IEC 61937-3; ffmpeg's spdifenc):
/// the four preamble words, the frame as 16-bit words in the order the
/// interface sends them, zeros to the repetition period of 1536 stereo
/// samples. As 16-bit little-endian samples.
fn iec61937_ac3(frame: &[u8]) -> Option<Vec<u8>> {
    const BURST: usize = 1536 * 4;
    if frame.len() < 6 || frame.len() + 8 > BURST || frame[0] != 0x0B || frame[1] != 0x77 {
        return None;
    }
    let bsmod = u16::from(frame[5] & 0x07);
    let mut out = Vec::with_capacity(BURST);
    for word in [0xF872u16, 0x4E1F, 0x0001 | (bsmod << 8), (frame.len() * 8) as u16] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for pair in frame.chunks(2) {
        out.push(*pair.get(1).unwrap_or(&0));
        out.push(pair[0]);
    }
    out.resize(BURST, 0);
    Some(out)
}

/// What the stream says it is, as each element hands it on: the parser's
/// caps carry the SPS/VUI colour and the sound's channels.
fn log_caps<'a>(elements: impl Iterator<Item = &'a gst::Element>) {
    for element in elements {
        if let Some(out) = element.static_pad("src") {
            let name = element.factory().map(|factory| factory.name().to_string()).unwrap_or_default();
            out.connect_notify(Some("caps"), move |pad, _| {
                if let Some(caps) = pad.current_caps() {
                    eprintln!("mediabox-live: akış {name}: {caps}");
                }
            });
        }
    }
}

fn codec_name(name: &str, version: Option<i32>) -> String {
    match (name, version) {
        ("video/x-h264", _) => "h264".into(),
        ("video/x-h265", _) => "hevc".into(),
        ("video/mpeg", Some(1)) => "mpeg1video".into(),
        ("video/mpeg", _) => "mpeg2video".into(),
        ("audio/mpeg", Some(1)) => "mp2".into(),
        ("audio/mpeg", _) => "aac".into(),
        ("audio/x-ac3", _) => "ac3".into(),
        ("audio/x-eac3", _) => "eac3".into(),
        (other, _) => other.to_string(),
    }
}

/// What a frame's caps say, worked out once per caps.
struct Described {
    fourcc: u32,
    format: String,
    width: u32,
    height: u32,
    display: (u32, u32),
    colour: u32,
    eotf: u8,
    fps: Option<f64>,
    interlace: String,
}

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

fn describe(caps: &gst::CapsRef) -> Option<Described> {
    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let fourcc = match info.format() {
        gst_video::VideoFormat::Nv12 => fourcc(b"NV12"),
        gst_video::VideoFormat::Nv16 => fourcc(b"NV16"),
        gst_video::VideoFormat::Nv1210le40 => fourcc(b"NV15"),
        other => {
            eprintln!("mediabox-live: düzlemin almadığı biçim: {other:?}");
            return None;
        }
    };
    let colorimetry = info.colorimetry();
    let encoding = match colorimetry.matrix() {
        gst_video::VideoColorMatrix::Bt709 => 1,
        gst_video::VideoColorMatrix::Bt2020 => 2,
        _ => 0,
    };
    let full = u32::from(colorimetry.range() == gst_video::VideoColorRange::Range0_255);
    let eotf: u8 = match colorimetry.transfer() {
        gst_video::VideoTransferFunction::Smpte2084 => 2,
        gst_video::VideoTransferFunction::AribStdB67 => 3,
        _ => 0,
    };
    let (width, height) = (info.width(), info.height());
    // mpv's mp_image_params_get_dsize: the larger side is stretched.
    let par = info.par();
    let (pn, pd) = (par.numer().max(1) as u64, par.denom().max(1) as u64);
    let display = if pn > pd {
        ((u64::from(width) * pn / pd) as u32, height)
    } else {
        (width, (u64::from(height) * pd / pn) as u32)
    };
    let fps = info.fps();
    let fps = (fps.numer() > 0 && fps.denom() > 0).then(|| fps.numer() as f64 / fps.denom() as f64);
    Some(Described {
        fourcc,
        format: format!("{:?}", info.format()),
        width,
        height,
        display,
        colour: encoding | (full << 4) | (u32::from(eotf) << 8),
        eotf,
        fps,
        interlace: format!("{:?}", info.interlace_mode()),
    })
}

fn picture(found: &Described, buffer: &gst::BufferRef) -> Option<Picture> {
    let meta = buffer.meta::<gst_video::VideoMeta>();
    let meta = meta?;
    let (planes, offsets, strides) = (meta.n_planes() as usize, meta.offset(), meta.stride());
    let planes = planes.min(4);
    let mut picture = Picture {
        fourcc: found.fourcc,
        width: found.width,
        height: found.height,
        planes,
        fds: [-1; 4],
        pitches: [0; 4],
        offsets: [0; 4],
        modifier: 0,
        colour: found.colour,
        display: found.display,
    };
    for plane in 0..planes {
        let offset = offsets[plane];
        let (range, skip) = buffer.find_memory(offset..offset + 1)?;
        let memory = buffer.peek_memory(range.start);
        let dmabuf = memory.downcast_memory_ref::<gst_allocators::DmaBufMemory>()?;
        picture.fds[plane] = dmabuf.fd();
        // The plane's offset inside its own memory, plus where that memory
        // starts inside the dma-buf.
        picture.offsets[plane] = (skip + memory.offset()) as u32;
        picture.pitches[plane] = strides[plane] as u32;
    }
    Some(picture)
}
