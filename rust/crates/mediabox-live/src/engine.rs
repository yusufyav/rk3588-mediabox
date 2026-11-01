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
use crate::stamp::{Class, Packetizer, Stamp, Stamped};

pub struct Options {
    /// The element that decodes video: `mppvideodec` on the appliance.
    pub video_decoder: String,
    pub audio: Audio,
    /// tsdemux's `latency`: how far a PTS may run ahead of its PCR.
    pub demux_latency_ms: u32,
    /// From the sink's wake-up to the light on the panel: the interface
    /// commits at its next vertical blank.
    pub render_delay_ms: u32,
    /// A frame later than this is not sent; `None`: none is dropped here,
    /// the panel decides (a frame past its vblank goes up at the next, the
    /// one it replaces goes back unseen). Dropped here at 20 ms, the second
    /// picture of a channel -- held up behind the first, which waited 1.7 s
    /// in the sink for its time -- was thrown away 29 ms before the panel
    /// would have shown it (2026-10-31, TRT WORLD, TRT 4K: a picture missing
    /// at the start of one opening in ten).
    pub max_lateness_ms: Option<u32>,
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
    /// MEDIABOX_LIVE_OPEN_TRACE: the decoded frames of a channel's opening
    /// still to be logged with their lead over the clock.
    pub open_probe: AtomicU64,
    /// A frame (or, with no video, sound) has come out since the last open.
    pub playing: AtomicBool,
    pub eof: AtomicBool,
    pub paused: AtomicBool,
    pub frames: AtomicU64,
    /// Frames out of the decoder, this channel's and the ones before.
    pub decoded: AtomicU64,
    /// Frames not shown because they were not after the one before.
    pub out_of_order: AtomicU64,
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

/// A channel's streams as the demultiplexer finds them, and when its sound
/// may start: with its picture, as a receiver starts both. Sound comes out of the demultiplexer at once and the picture only
/// at its first access point, so left to itself the sound played first and
/// the picture joined it seconds later (measured 2026-10-25, every channel;
/// a Sony BRAVIA starts both together). Until the first key frame is
/// known, sound is not passed; after, only from that frame's time on, which
/// both sinks keep on the one clock.
struct Programme {
    generation: u64,
    opened: Instant,
    /// Sound has started; how many of its buffers were not played before.
    sounding: AtomicBool,
    held: AtomicU64,
    /// The audio streams met so far, in PMT order.
    audio_order: Mutex<usize>,
    /// A picture stream is played (the first one; others are drained).
    video_taken: AtomicBool,
    /// The PTS of the channel's first key frame.
    picture: Mutex<Option<u64>>,
    /// `picture` or `no_picture` has been set.
    known: std::sync::Condvar,
    /// Sound has waited for the key frame (traced once).
    waited: AtomicBool,
    /// The programme has no picture (a radio service): its sound waits for
    /// nothing.
    no_picture: AtomicBool,
    /// When sound was first held back: a picture that never comes (its
    /// stream scrambled, say) does not keep the sound away for good.
    held_since: Mutex<Option<Instant>>,
}

/// How long sound waits for a picture that does not come.
const PICTURE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

impl Programme {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            opened: Instant::now(),
            sounding: AtomicBool::new(false),
            held: AtomicU64::new(0),
            audio_order: Mutex::new(0),
            video_taken: AtomicBool::new(false),
            picture: Mutex::new(None),
            known: std::sync::Condvar::new(),
            waited: AtomicBool::new(false),
            no_picture: AtomicBool::new(false),
            held_since: Mutex::new(None),
        }
    }

    /// The first key frame is known, or that there is none to wait for.
    fn picture_known(&self) {
        let _picture = self.picture.lock().unwrap_or_else(|p| p.into_inner());
        self.known.notify_all();
    }

    /// Whether a sound frame at `pts` is played.
    ///
    /// Until the key frame's time is known a frame waits here, on the
    /// sound's own thread (its queue takes what comes meanwhile), rather
    /// than being dropped: in an opening's burst the sound reaches here
    /// before the key frame reaches its parser, and the frames after the
    /// key frame's time went with those before it -- the sound joined the
    /// picture 316 ms late (measured 2026-11-01: 35 frames dropped in the
    /// 2.3 ms before the key frame was parsed). `stop`: the branch is being
    /// taken down, and waits no more.
    fn sound(&self, pts: Option<u64>, stop: &dyn Fn() -> bool) -> bool {
        if self.sounding.load(Ordering::SeqCst) {
            return true;
        }
        let why = loop {
            if self.no_picture.load(Ordering::SeqCst) {
                break Some("görüntüsüz servis");
            }
            let picture = self.picture.lock().unwrap_or_else(|p| p.into_inner());
            match *picture {
                Some(first) if pts.is_none_or(|pts| pts >= first) => break Some("ilk anahtar karenin zamanından"),
                Some(_) => break None,
                None => {
                    let waited = self.held_since.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(Instant::now).elapsed();
                    if waited >= PICTURE_WAIT {
                        break Some("görüntü gelmedi");
                    }
                    if stop() {
                        break None;
                    }
                    if !self.waited.swap(true, Ordering::Relaxed) {
                        mediabox_core::timeline!(
                            "audio-gate",
                            "gen={} wait=1 pts={}",
                            self.generation,
                            pts.map_or(-1, |p| p as i64)
                        );
                    }
                    let _ = self.known.wait_timeout(picture, std::time::Duration::from_millis(10));
                }
            }
        };
        let Some(why) = why else {
            if self.held.fetch_add(1, Ordering::Relaxed) == 0 {
                mediabox_core::timeline!(
                    "audio-gate",
                    "gen={} open=0 pts={}",
                    self.generation,
                    pts.map_or(-1, |p| p as i64)
                );
            }
            return false;
        };
        self.sounding.store(true, Ordering::SeqCst);
        mediabox_core::timeline!(
            "audio-gate",
            "gen={} open=1 pts={} held={} why={}",
            self.generation,
            pts.map_or(-1, |p| p as i64),
            self.held.load(Ordering::Relaxed),
            why.replace(' ', "_")
        );
        eprintln!(
            "mediabox-live: ses başlıyor ({why}), açılıştan {} ms sonra; önceki {} ses tamponu çalınmadı",
            self.opened.elapsed().as_millis(),
            self.held.load(Ordering::Relaxed)
        );
        true
    }
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

/// One display slot per decoded picture.
///
/// While a newly opened transport stream settles, `tsdemux` can briefly map
/// an otherwise steady 50 Hz programme to PTS steps between roughly 12 and
/// 36 ms, and swing it more than 100 ms off for a moment (NTV HD after a
/// retune, measured 2026-10-31). MPP's HEVC decoder hands its first pictures
/// out with one PTS for dozens of them (TRT 4K: about fifty). Passed to the
/// panel, two different pictures land on one vblank and the next one is
/// repeated, or pictures are thrown away as not later than the one before.
/// So each picture takes the next slot of a grid at the programme's picture
/// period; a large timestamp discontinuity re-anchors the grid.
///
/// The period: the decoder's duration, or half of it when the pictures come
/// at half of it -- MPP deinterlaces MPEG-2 576i into one picture a field,
/// 20 ms apart, each still saying 40 ms (TRT HABER SD, measured). Taken as
/// 40, every second field was thrown away and 576i played at 25 pictures.
///
/// The grid is not let wander from the programme clock, which the sound is
/// played by, beyond EBU R37's detectability (sound no more than 40 ms ahead
/// of the picture, 60 ms behind it) for long: past either for two seconds
/// on end -- a picture the decoder lost, the board's clock against the
/// broadcaster's over hours -- it catches up: ahead of its sound, the grid
/// moves on by the fewest whole periods back within them (a picture held
/// once); behind it, pictures are left out until it is within them --
/// never put on or before a slot already used. A swing that comes and goes
/// inside two seconds (the opening's) moves nothing.
#[derive(Default)]
struct PresentationTimeline {
    generation: u64,
    /// The slot the last picture took.
    last: Option<u64>,
    last_source: Option<u64>,
    /// The latest steps between source timestamps, for the period.
    steps: std::collections::VecDeque<u64>,
    /// Consecutive pictures off the grid beyond R37, and how long.
    beyond_ns: u64,
    /// Behind the sound for too long: pictures are left out until not.
    catching_up: bool,
}

impl PresentationTimeline {
    const MIN_PERIOD_NS: u64 = 5_000_000;
    const MAX_PERIOD_NS: u64 = 200_000_000;
    /// A difference this large is a timeline break, not jitter.
    const REANCHOR_NS: u64 = 2_000_000_000;
    /// EBU R37: the picture later than its sound by more than this is seen.
    const LATE_NS: u64 = 40_000_000;
    /// ... and earlier than its sound by more than this.
    const EARLY_NS: u64 = 60_000_000;
    /// How long beyond those before the grid moves.
    const PERSIST_NS: u64 = 2_000_000_000;
    /// Off the grid by more than this is a jump in the stream, not a drift.
    const JUMP_NS: u64 = 200_000_000;
    const STEPS: usize = 9;

    /// A channel's first picture timed nowhere near its first key frame:
    /// MPP's MPEG-2 decoder gave its first two pictures 80 ms and 158 s for
    /// a key frame at 316 s (TRT HABER SD, 2026-10-31), and the second was
    /// put up two and a half minutes "late", before the picture proper. Not
    /// shown; the first picture is the first one within two seconds of it.
    fn bogus_first(&self, generation: u64, source_pts: u64, key: Option<u64>) -> bool {
        (self.generation != generation || self.last.is_none())
            && key.is_some_and(|key| source_pts.abs_diff(key) > Self::REANCHOR_NS)
    }

    /// The picture period: `duration`, or half of it when the source
    /// timestamps step by half of it (one picture a field).
    fn period(&self, duration: u64) -> u64 {
        if self.steps.is_empty() {
            return duration;
        }
        let mut steps: Vec<u64> = self.steps.iter().copied().collect();
        steps.sort_unstable();
        let median = steps[steps.len() / 2];
        let half = duration / 2;
        if half >= Self::MIN_PERIOD_NS && median.abs_diff(half) * 4 <= half { half } else { duration }
    }

    /// The slot for a picture at `source_pts`; `None`: left out, to catch
    /// up with the sound.
    fn schedule(&mut self, generation: u64, source_pts: u64, duration_ns: Option<u64>) -> Option<u64> {
        if self.generation != generation {
            *self = Self { generation, ..Self::default() };
        }
        if let Some(previous) = self.last_source.replace(source_pts)
            && source_pts > previous
            && source_pts - previous <= Self::MAX_PERIOD_NS
        {
            if self.steps.len() == Self::STEPS {
                self.steps.pop_front();
            }
            self.steps.push_back(source_pts - previous);
        }
        let Some(duration) = duration_ns.filter(|period| (Self::MIN_PERIOD_NS..=Self::MAX_PERIOD_NS).contains(period)) else {
            self.last = None;
            return Some(source_pts);
        };
        let period = self.period(duration);
        let scheduled = match self.last {
            None => source_pts,
            Some(last) => {
                let next = last + period;
                let late = next > source_pts && next - source_pts > Self::LATE_NS;
                let early = source_pts > next && source_pts - next > Self::EARLY_NS;
                if source_pts.abs_diff(next) > Self::REANCHOR_NS {
                    self.beyond_ns = 0;
                    self.catching_up = false;
                    source_pts
                } else if late && self.catching_up {
                    // Left out; the next picture is a period less behind.
                    return None;
                } else if late || early {
                    self.beyond_ns += period;
                    if self.beyond_ns < Self::PERSIST_NS {
                        next
                    } else if late {
                        self.beyond_ns = 0;
                        self.catching_up = true;
                        return None;
                    } else {
                        self.beyond_ns = 0;
                        // A jump in the stream: to the slot nearest its time.
                        // A drift or a lost picture: the fewest whole periods
                        // back within the limit -- one, as a rule.
                        let off = source_pts - next;
                        let periods = if off > Self::JUMP_NS {
                            (off + period / 2) / period
                        } else {
                            (off - Self::EARLY_NS).div_ceil(period)
                        };
                        next + periods * period
                    }
                } else {
                    self.beyond_ns = 0;
                    self.catching_up = false;
                    next
                }
            }
        };
        self.last = Some(scheduled);
        Some(scheduled)
    }
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
            // No QoS upstream: a frame a few ms past its sink time is still
            // in time for the panel, which shows it at the vblank at or after
            // its due time (plane.rs, as MediaCodec.releaseOutputBuffer with
            // a render timestamp). With QoS on, GstVideoDecoder threw away the
            // frame after every key frame's decode -- "Dropping frame due to
            // QoS", 3-17 ms past its deadline, once a second on TRT HABER HD
            // (measured 2026-10-31): a picture missing from the panel each time.
            .qos(false)
            .build();
        video_sink.set_async(false);
        video_sink.set_max_lateness(options.max_lateness_ms.map_or(-1, |ms| i64::from(ms) * 1_000_000));
        video_sink.set_render_delay(gst::ClockTime::from_mseconds(u64::from(options.render_delay_ms)));
        // A channel's first decoded frames, as they reach the sink: PTS
        // minus the clock's running time (the sink wants it above minus
        // the latency), when MEDIABOX_LIVE_OPEN_TRACE asks for it.
        {
            let shared = Arc::clone(&shared);
            let pipeline_clock = clock.clone();
            let weak_pipeline = pipeline.downgrade();
            video_sink.static_pad("sink").expect("appsink has a sink pad").add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if shared.open_probe.load(Ordering::Relaxed) > 0
                    && let (Some(buffer), Some(pipeline)) = (info.buffer(), weak_pipeline.upgrade())
                    && let (Some(pts), Some(base)) = (buffer.pts(), pipeline.base_time())
                {
                    shared.open_probe.fetch_sub(1, Ordering::Relaxed);
                    let now = pipeline_clock.time().saturating_sub(base).nseconds() as i64;
                    eprintln!("mediabox-live: açılış kare pts {} ms öndelik {:.3} s", pts.mseconds(), (pts.nseconds() as i64 - now) as f64 / 1e9);
                }
                gst::PadProbeReturn::Ok
            });
        }

        pipeline
            .add_many([&video_selector, video_sink.upcast_ref(), &audio_selector])
            .map_err(|e| e.to_string())?;
        video_selector.link(&video_sink).map_err(|e| e.to_string())?;
        // A radio service has no picture: its sound is what says it plays.
        {
            let shared = Arc::clone(&shared);
            let pipeline_clock = clock.clone();
            let weak_pipeline = pipeline.downgrade();
            let audio_traced = AtomicU64::new(0);
            audio_selector.static_pad("src").expect("input-selector has a src pad").add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let (Some(buffer), Some(pipeline)) = (info.buffer(), weak_pipeline.upgrade())
                    && let (Some(pts), Some(base)) = (buffer.pts(), pipeline.base_time())
                {
                    let now = pipeline_clock.time().saturating_sub(base).nseconds() as i64;
                    shared.audio_lead_ns.store(pts.nseconds() as i64 - now, Ordering::Relaxed);
                    let generation = shared.generation.load(Ordering::SeqCst);
                    if audio_traced.swap(generation, Ordering::Relaxed) != generation {
                        mediabox_core::timeline!(
                            "audio-sink",
                            "gen={generation} pts={} rt={now} base={} dur={}",
                            pts.nseconds(),
                            base.nseconds(),
                            buffer.duration().map_or(-1, |d| d.nseconds() as i64)
                        );
                    }
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
        let mut latency: Option<(u64, u64)> = None;
        let engine = Arc::downgrade(self);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let Some(buffer) = sample.buffer() else { return Ok(gst::FlowSuccess::Ok) };
                    let pts = buffer.pts().map(|pts| pts.nseconds()).unwrap_or(0);
                    if mediabox_core::timeline::enabled()
                        && let (Some(clock), Some(base)) = (sink.clock(), sink.base_time())
                    {
                        mediabox_core::timeline::write(
                            "sink",
                            format_args!("gen={} pts={pts} rt={}", buffer.offset_end(), clock.time().saturating_sub(base).nseconds()),
                        );
                    }
                    if buffer.offset_end() != shared.generation.load(Ordering::SeqCst) {
                        trace.event('M', 0, pts / 1000);
                        mediabox_core::timeline!("drop", "gen={} pts_us={} why=stale", buffer.offset_end(), pts / 1000);
                        return Ok(gst::FlowSuccess::Ok);
                    }
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
                                "mediabox-live: görüntü {}x{} {} {:?} fps, {}; ekrana {}x{} oranında, yanlardan {} sütun kırpık",
                                found.width, found.height, found.format, found.fps, found.interlace, found.display.0, found.display.1, found.crop_x
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
                        mediabox_core::timeline!("drop", "gen={generation} pts_us={} why=order last_us={}", pts / 1000, shown_pts / 1000);
                        shared.out_of_order.fetch_add(1, Ordering::Relaxed);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    last = Some((generation, pts));
                    let Some(picture) = described.as_ref().and_then(|found| picture(found, buffer)) else {
                        // Not a dma-buf the plane takes: a software decoder.
                        // Traced, never copied -- the whole point is that
                        // nothing is.
                        trace.event('X', 0, pts / 1000);
                        mediabox_core::timeline!("drop", "gen={generation} pts_us={} why=not-dmabuf", pts / 1000);
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    // When it is due on the panel, on the clock's own time
                    // (CLOCK_MONOTONIC): from its PTS, not from when this
                    // thread woke, so frames due 20 ms apart are 20 ms apart.
                    let mut due = 0;
                    if let (Some(clock), Some(base)) = (sink.clock(), sink.base_time()) {
                        let now = clock.time().saturating_sub(base).nseconds() as i64;
                        shared.video_lag_ns.store(now - pts as i64, Ordering::Relaxed);
                        // The latency the sinks wait by, asked once a channel.
                        if latency.is_none_or(|(asked_in, _)| asked_in != generation) {
                            // The pipeline's: the one every sink was given.
                            let mut query = gst::query::Latency::new();
                            let ns = match sink.parent().and_then(|parent| parent.downcast::<gst::Element>().ok()) {
                                Some(pipeline) if pipeline.query(&mut query) => query.result().1.nseconds(),
                                _ => 0,
                            };
                            latency = Some((generation, ns));
                            mediabox_core::timeline!("sink-latency", "gen={generation} ns={ns} base={}", base.nseconds());
                        }
                        due = base.nseconds() + pts + latency.map_or(0, |(_, ns)| ns);
                    }
                    match plane.send(&picture, sample.clone(), generation, pts, due) {
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
        let opened_ns = mediabox_core::timeline::now_ns();
        mediabox_core::timeline!(
            "open",
            "gen={generation} path={path} base={} latency_ms={} render_delay_ms={}",
            self.pipeline.base_time().map_or(0, |base| base.nseconds()),
            self.options.demux_latency_ms,
            self.options.render_delay_ms
        );
        if std::env::var_os("MEDIABOX_LIVE_OPEN_TRACE").is_some() {
            self.shared.open_probe.store(60, Ordering::Relaxed);
        }
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
            mediabox_core::timeline!(
                "blanked",
                "gen={generation} ms={:.1}",
                (mediabox_core::timeline::now_ns() - opened_ns) as f64 / 1e6
            );
        }
        let (branch, source) = self.build(generation)?;
        mediabox_core::timeline!(
            "built",
            "gen={generation} ms={:.1}",
            (mediabox_core::timeline::now_ns() - opened_ns) as f64 / 1e6
        );
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
            // Stamped by `read_stream`, on the broadcast's PCR at first.
            .do_timestamp(false)
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
        let programme = Arc::new(Programme::new(generation));
        {
            let programme = Arc::clone(&programme);
            demux.connect_no_more_pads(move |_| {
                if !programme.video_taken.load(Ordering::SeqCst) {
                    programme.no_picture.store(true, Ordering::SeqCst);
                    programme.picture_known();
                }
            });
        }
        {
            let elements = Arc::clone(&elements);
            let pads = Arc::clone(&pads);
            demux.connect_pad_added(move |_, pad| {
                let Some(engine) = weak.upgrade() else { return };
                if mediabox_core::timeline::enabled() {
                    trace_demux_pad(generation, pad);
                }
                if engine.shared.generation.load(Ordering::SeqCst) != generation {
                    engine.discard(pad, &elements);
                    return;
                }
                if let Err(error) = engine.stream(generation, pad, &elements, &pads, &programme) {
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
        programme: &Arc<Programme>,
    ) -> Result<(), String> {
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let Some(structure) = caps.structure(0) else {
            return Err("biçimi yok".into());
        };
        let name = structure.name().as_str();
        let version = structure.get::<i32>("mpegversion").ok();
        if name.starts_with("video/") {
            if programme.video_taken.swap(true, Ordering::SeqCst) {
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
            // What the broadcaster says of the picture's framing (AFD, bar
            // data), said when it changes: a 4:3 frame can carry a 16:9
            // picture.
            if let Some(out) = parser.static_pad("src") {
                let said: Mutex<Option<String>> = Mutex::new(None);
                out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    let Some(buffer) = info.buffer() else { return gst::PadProbeReturn::Ok };
                    let afd = buffer.meta::<gst_video::VideoAFDMeta>().map(|meta| format!("{:?}", meta.afd()));
                    let bar = buffer.meta::<gst_video::VideoBarMeta>().map(|meta| {
                        format!("{} {} {}", if meta.is_letterbox() { "üst/alt" } else { "sol/sağ" }, meta.bar_data1(), meta.bar_data2())
                    });
                    let now = format!("AFD {} çubuk {}", afd.as_deref().unwrap_or("yok"), bar.as_deref().unwrap_or("yok"));
                    let mut said = said.lock().unwrap_or_else(|p| p.into_inner());
                    if said.as_deref() != Some(now.as_str()) {
                        eprintln!("mediabox-live: çerçeve: {now}");
                        *said = Some(now);
                    }
                    gst::PadProbeReturn::Ok
                });
            }
            // The first key frame says when the channel starts.
            if let Some(out) = parser.static_pad("src") {
                let programme = Arc::clone(programme);
                out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    let Some(buffer) = info.buffer() else { return gst::PadProbeReturn::Ok };
                    if buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                        return gst::PadProbeReturn::Ok;
                    }
                    if let Some(pts) = buffer.pts() {
                        programme.picture.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(pts.nseconds());
                        programme.picture_known();
                        mediabox_core::timeline!(
                            "parse-key",
                            "gen={} pts={} dts={}",
                            programme.generation,
                            pts.nseconds(),
                            buffer.dts().map_or(-1, |dts| dts.nseconds() as i64)
                        );
                        return gst::PadProbeReturn::Remove;
                    }
                    gst::PadProbeReturn::Ok
                });
            }
            if mediabox_core::timeline::enabled()
                && let Some(out) = parser.static_pad("src")
            {
                out.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                    if let Some(gst::PadProbeData::Event(event)) = &info.data
                        && let gst::EventView::Caps(caps) = event.view()
                    {
                        let caps = caps.caps().to_string().replace(' ', "");
                        mediabox_core::timeline!("parse-caps", "gen={generation} caps={caps}");
                    }
                    gst::PadProbeReturn::Ok
                });
            }
            let decoder = make(&self.options.video_decoder)?;
            if mediabox_core::timeline::enabled()
                && let Some(input) = decoder.static_pad("sink")
            {
                input.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if let Some(buffer) = info.buffer() {
                        mediabox_core::timeline!(
                            "dec-in",
                            "gen={generation} pts={} dts={} key={}",
                            buffer.pts().map_or(-1, |t| t.nseconds() as i64),
                            buffer.dts().map_or(-1, |t| t.nseconds() as i64),
                            u8::from(!buffer.flags().contains(gst::BufferFlags::DELTA_UNIT))
                        );
                    }
                    gst::PadProbeReturn::Ok
                });
            }
            // MPP marks a frame interlaced, and starts its deinterlacer, only
            // when the parser is not in fast mode -- ffmpeg-rockchip turns it
            // off for interlaced video (rkmppdec.c), gstreamer-rockchip leaves
            // it on. A broadcast is interlaced more often than not.
            if decoder.find_property("fast-mode").is_some() {
                decoder.set_property("fast-mode", false);
            }
            // A frame the decoder hands over after a newer channel was asked
            // for is the old channel's, whatever it says: never shown, never
            // counted as the new one's first. The ones it let through carry
            // their channel (`offset_end`), for the sink to check again: a
            // frame already on its way when the channel changed was shown
            // and counted as the next channel's first (measured 2026-10-25,
            // CEM TV's 1920x1080 frame "0 ms" into TV 5).
            if let Some(out) = decoder.static_pad("src") {
                let shared = Arc::clone(&self.shared);
                let programme = Arc::clone(programme);
                let presentation = Mutex::new(PresentationTimeline::default());
                out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if mediabox_core::timeline::enabled()
                        && let Some(buffer) = info.buffer()
                    {
                        // The video flags (interlaced, TFF, RFF, one field)
                        // are bits 20 and up of the buffer's flags.
                        mediabox_core::timeline::write(
                            "dec-out",
                            format_args!(
                                "gen={generation} pts={} dur={} flags={:#x} stale={}",
                                buffer.pts().map_or(-1, |t| t.nseconds() as i64),
                                buffer.duration().map_or(-1, |t| t.nseconds() as i64),
                                buffer.flags().bits(),
                                u8::from(shared.generation.load(Ordering::SeqCst) != generation)
                            ),
                        );
                    }
                    if shared.generation.load(Ordering::SeqCst) != generation {
                        return gst::PadProbeReturn::Drop;
                    }
                    shared.decoded.fetch_add(1, Ordering::Relaxed);
                    if let Some(buffer) = info.buffer_mut() {
                        let buffer = buffer.make_mut();
                        buffer.set_offset_end(generation);
                        if let Some(source_pts) = buffer.pts() {
                            let source_pts = source_pts.nseconds();
                            let key = *programme.picture.lock().unwrap_or_else(|p| p.into_inner());
                            if presentation.lock().unwrap_or_else(|p| p.into_inner()).bogus_first(generation, source_pts, key) {
                                mediabox_core::timeline!("drop", "gen={generation} pts_us={} why=bogus-pts", source_pts / 1000);
                                return gst::PadProbeReturn::Drop;
                            }
                            let Some(scheduled) = presentation
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .schedule(
                                    generation,
                                    source_pts,
                                    buffer.duration().map(|duration| duration.nseconds()),
                                )
                            else {
                                mediabox_core::timeline!("drop", "gen={generation} pts_us={} why=catch-up", source_pts / 1000);
                                return gst::PadProbeReturn::Drop;
                            };
                            if scheduled != source_pts {
                                mediabox_core::timeline!(
                                    "schedule",
                                    "gen={generation} pts={source_pts} presentation={scheduled} delta={}",
                                    scheduled as i128 - source_pts as i128
                                );
                                buffer.set_pts(gst::ClockTime::from_nseconds(scheduled));
                            }
                        }
                    }
                    gst::PadProbeReturn::Ok
                });
            }
            // Between the decoder and the sink, a few pictures: mppvideodec
            // pushes each one from its output task holding the decoder's
            // stream lock (gst_mpp_dec_loop -> finish_frame), and the sink
            // holds a channel's first picture until its time, 1.7 s. All
            // that while handle_frame waited on the lock and MPP was given
            // nothing to decode: the pictures after the first were decoded
            // only once it went, and a 4K HEVC picture takes 25-40 ms -- the
            // first five of TRT 4K came past their time and the panel put
            // them back unseen (2026-10-31, three openings in three). Pushed
            // into a queue the lock is let go at once and MPP decodes ahead.
            let ahead = make("queue")?;
            ahead.set_property("max-size-buffers", 3u32);
            ahead.set_property("max-size-bytes", 0u32);
            ahead.set_property("max-size-time", 0u64);
            self.chain(pad, &[parser, decoder, ahead], &self.video_selector, elements, pads)?;
            self.shared.info.lock().unwrap_or_else(|p| p.into_inner()).video_codec = Some(codec_name(name, version));
            return Ok(());
        }
        if !name.starts_with("audio/") {
            self.discard(pad, elements);
            return Ok(());
        }
        let order = {
            let mut order = programme.audio_order.lock().unwrap_or_else(|p| p.into_inner());
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
        // Gated frame by frame, after the parser (`Programme::sound`): on the demultiplexer's
        // pad a buffer is a whole PES packet, up to 350 ms of sound on some
        // channels, and the one holding the key frame's time began before
        // it and was dropped -- the sound joined the picture up to 350 ms
        // late (measured 2026-11-01, 32 openings: the delay was each
        // channel's first passed PES minus the key frame's PTS).
        {
            let programme = Arc::clone(programme);
            let framed = chain[0].static_pad("src").ok_or("ayrıştırıcının çıkışı yok")?;
            framed.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                let pts = info.buffer().and_then(|buffer| buffer.pts()).map(|pts| pts.nseconds());
                let stop = || pad.pad_flags().contains(gst::PadFlags::FLUSHING);
                if programme.sound(pts, &stop) { gst::PadProbeReturn::Ok } else { gst::PadProbeReturn::Drop }
            });
        }
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
    let mut packetizer = Packetizer::default();
    let mut stamp = Stamp::default();
    let mut pushed = 0u64;
    let mut key: Option<gst_app::AppSrc> = None;
    let mut generation = 0u64;
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
        if key.as_ref() != Some(&source) {
            key = Some(source.clone());
            generation = shared.generation.load(Ordering::SeqCst);
            pushed = 0;
            packetizer.new_programme();
        }
        let arrived = source.current_running_time();
        for piece in packetizer.cut(&chunk[..read]) {
            let bytes = piece.data.len();
            let mut buffer = gst::Buffer::from_mut_slice(piece.data);
            if let Some(arrived) = arrived {
                let Stamped { at, class, discontinuity } = stamp.time(generation, arrived.nseconds(), piece.pcr);
                pushed += 1;
                // A PCR jump is said on the timeline: the times of this
                // opening are then no measurement (tools/live-trace.py marks
                // it invalid).
                mediabox_core::timeline!(
                    "src",
                    "gen={generation} n={pushed} bytes={bytes} arr={} pts={at} pcr0={} class={} disc={}",
                    arrived.nseconds(),
                    piece.pcr.map_or(-1, |v| v as i64),
                    if class == Class::Paced { "paced" } else { "live" },
                    u8::from(discontinuity)
                );
                let at = gst::ClockTime::from_nseconds(at);
                let buffer = buffer.get_mut().expect("a new buffer is writable");
                buffer.set_pts(at);
                buffer.set_dts(at);
            }
            // A source taken down under the push refuses it; the reader goes
            // on for the branch after it.
            let _ = source.push_buffer(buffer);
        }
    }
}
/// A demultiplexer pad on the timeline: when it came, its first buffer,
/// every segment and every discontinuity on it.
fn trace_demux_pad(generation: u64, pad: &gst::Pad) {
    let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
    let name = caps
        .structure(0)
        .map(|s| s.name().to_string())
        .unwrap_or_default();
    let kind = if name.starts_with("video/") {
        "video"
    } else if name.starts_with("audio/") {
        "audio"
    } else {
        "other"
    };
    mediabox_core::timeline!(
        "pad",
        "gen={generation} kind={kind} pad={} caps={name}",
        pad.name()
    );
    let first = AtomicBool::new(true);
    let pad_name = pad.name().to_string();
    pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
        move |_, info| {
            match &info.data {
                Some(gst::PadProbeData::Buffer(buffer)) => {
                    let discont = buffer.flags().contains(gst::BufferFlags::DISCONT);
                    if first.swap(false, Ordering::Relaxed) || discont {
                        mediabox_core::timeline!(
                            if discont {
                                "demux-discont"
                            } else {
                                "demux-first"
                            },
                            "gen={generation} kind={kind} pad={pad_name} pts={} dts={}",
                            buffer.pts().map_or(-1, |t| t.nseconds() as i64),
                            buffer.dts().map_or(-1, |t| t.nseconds() as i64)
                        );
                    }
                }
                Some(gst::PadProbeData::Event(event)) => {
                    if let gst::EventView::Segment(segment) = event.view()
                        && let Some(segment) = segment.segment().downcast_ref::<gst::ClockTime>()
                    {
                        mediabox_core::timeline!(
                            "demux-segment",
                            "gen={generation} kind={kind} pad={pad_name} start={} time={} base={}",
                            segment.start().map_or(-1, |t| t.nseconds() as i64),
                            segment.time().map_or(-1, |t| t.nseconds() as i64),
                            segment.base().map_or(-1, |t| t.nseconds() as i64)
                        );
                    }
                }
                _ => {}
            }
            gst::PadProbeReturn::Ok
        },
    );
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
    /// Columns left off at the left and again at the right (SD blanking).
    crop_x: u32,
    colour: u32,
    eotf: u8,
    fps: Option<f64>,
    interlace: String,
}

/// The columns at each side of a 720-sample standard-definition line that
/// are not picture: ITU-R BT.601 samples 720, the 4:3 or 16:9 picture is
/// the 704 (EBU: 702, 52 us) in the middle, and the rest is horizontal
/// blanking, to be left off when shown. Shown, TRT BELGESEL's 576i had a
/// dark strip at the edge where a television's own tuner shows none.
const SD_BLANKING_COLUMNS: u32 = 8;

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
    let mut display = if pn > pd {
        ((u64::from(width) * pn / pd) as u32, height)
    } else {
        (width, (u64::from(height) * pd / pn) as u32)
    };
    // The aspect a stream signals is the picture's, the 704 middle columns
    // of a 720-wide SD line: the blanking at its sides is left off.
    let crop_x = if width == 720 && matches!(info.format(), gst_video::VideoFormat::Nv12 | gst_video::VideoFormat::Nv16) {
        SD_BLANKING_COLUMNS
    } else {
        0
    };
    // A picture signalled 4:3 fills the 16:9 panel widened, uncut: a
    // television's "Full" ("enlarges the picture horizontally to fill the
    // screen when the original source is 4:3", Sony BRAVIA). Measured
    // against its HD simulcast on the same shot (TRT BELGESEL, 2026-10-25):
    // the SD 576i carries the whole 16:9 picture squeezed into its 4:3
    // frame. Widened uncut it matched the HD picture best (SSIM 0.868 on
    // the static set); with any rows cut off it matched worse (8 rows:
    // 0.697), Kodi's Wide Zoom (cut, partly widened) 0.652, and shown 4:3
    // with bars 0.477.
    let aspect = f64::from(display.0) / f64::from(display.1.max(1));
    if (aspect - 4.0 / 3.0).abs() < 0.04 {
        display = (height * 16 / 9, height);
    }

    let fps = info.fps();
    let fps = (fps.numer() > 0 && fps.denom() > 0).then(|| fps.numer() as f64 / fps.denom() as f64);
    Some(Described {
        fourcc,
        format: format!("{:?}", info.format()),
        width,
        height,
        display,
        crop_x,
        colour: encoding | (full << 4) | (u32::from(eotf) << 8),
        eotf,
        fps,
        interlace: format!("{:?}", info.interlace_mode()),
    })
}

fn picture(found: &Described, buffer: &gst::BufferRef) -> Option<Picture> {
    let meta = buffer.meta::<gst_video::VideoMeta>()?;
    let (planes, offsets, strides) = (meta.n_planes() as usize, meta.offset(), meta.stride());
    let planes = planes.min(4);
    let mut picture = Picture {
        fourcc: found.fourcc,
        width: found.width - 2 * found.crop_x,
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
        // starts inside the dma-buf, plus the columns cut off its left: one
        // byte a column in either plane of NV12 and NV16 (the chroma's two
        // bytes cover two columns).
        picture.offsets[plane] = (skip + memory.offset()) as u32 + found.crop_x;
        picture.pitches[plane] = strides[plane] as u32;
    }
    Some(picture)
}

#[cfg(test)]
mod presentation_tests {
    use super::PresentationTimeline;

    const MS: u64 = 1_000_000;

    /// The slots of the pictures not left out.
    fn run(timeline: &mut PresentationTimeline, source: &[u64], duration: u64) -> Vec<u64> {
        source.iter().filter_map(|&pts| timeline.schedule(1, pts, Some(duration))).collect()
    }

    fn steps(scheduled: &[u64]) -> Vec<u64> {
        scheduled.windows(2).map(|w| w[1] - w[0]).collect()
    }

    #[test]
    fn opening_pts_jitter_keeps_one_picture_per_fifty_hz_slot() {
        let mut timeline = PresentationTimeline::default();
        let source: Vec<u64> = [0, 20, 40, 57, 75, 95, 115, 137, 159, 172, 185, 206, 227].iter().map(|ms| ms * MS).collect();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        assert_eq!(scheduled, (0..source.len() as u64).map(|n| n * 20 * MS).collect::<Vec<_>>());
    }

    /// NTV HD after a retune (2026-10-31, generation 14), as tsdemux timed
    /// it while its clock estimate swung: steps of 9 to 68 ms for 20 ms
    /// pictures, ending 108 ms ahead of the grid. The swing moves nothing
    /// for two seconds; what stays beyond R37 after them is caught up once,
    /// by whole periods.
    #[test]
    fn the_measured_retune_swing_moves_nothing_for_two_seconds() {
        let measured_us = [
            190_481_800u64, 190_512_900, 190_544_000, 190_552_900, 190_561_800, 190_599_000, 190_636_200, 190_678_700,
            190_746_600, 190_779_800, 190_789_900, 190_809_900, 190_829_900, 190_849_900,
        ];
        let mut source: Vec<u64> = measured_us.iter().map(|us| us * 1_000).collect();
        let mut t = *source.last().unwrap();
        for _ in 0..200 {
            t += 20 * MS;
            source.push(t);
        }
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        let odd: Vec<(usize, u64)> = steps(&scheduled).into_iter().enumerate().filter(|&(_, s)| s != 20 * MS).collect();
        assert_eq!(odd.len(), 1, "{odd:?}");
        assert!(odd[0].0 >= 100, "not within two seconds: {odd:?}");
        assert_eq!(odd[0].1, 80 * MS, "108 ms ahead: three periods, back within 60");
        let settled = source.last().unwrap() - scheduled.last().unwrap();
        assert!(settled <= 60 * MS, "within R37: {settled}");
    }

    /// TRT 4K (HEVC) opening as MPP timed it before 0003: some fifty
    /// pictures with one PTS, then on 20 ms apart. Every picture its own
    /// slot, none on or before one already used; the grid, a second behind
    /// the sound, catches up two seconds later by leaving pictures out.
    #[test]
    fn hevc_pictures_with_one_timestamp_each_get_their_own_slot() {
        let mut source = vec![306_348_500 * 1_000u64, 306_363_900 * 1_000, 306_397_800 * 1_000];
        source.extend(std::iter::repeat_n(306_567_400 * 1_000, 50));
        let mut t = 306_580_800 * 1_000u64;
        for _ in 0..300 {
            source.push(t);
            t += 20 * MS;
        }
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        assert!(scheduled.windows(2).all(|w| w[1] >= w[0] + 20 * MS), "never back, never the same slot");
        let left_out = source.len() - scheduled.len();
        assert!((40..=55).contains(&left_out), "caught up: {left_out}");
        let behind = scheduled.last().unwrap() - source.last().unwrap();
        assert!(behind <= 40 * MS, "within R37 at the end: {behind}");
    }

    /// MPEG-2 576i: MPP gives one picture a field, 20 ms apart, each saying
    /// 40 ms. Fifty a second, one slot each.
    #[test]
    fn a_deinterlaced_field_rate_stream_keeps_every_field() {
        let source: Vec<u64> = (0..300u64).map(|n| 318_969_700_000 + n * 20 * MS).collect();
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 40 * MS);
        assert!(steps(&scheduled[2..]).iter().all(|&s| s == 20 * MS), "{:?}", &steps(&scheduled)[..10]);
        assert!(scheduled.windows(2).all(|w| w[1] > w[0]));
        // A progressive 25 Hz stream stays at 40.
        let source: Vec<u64> = (0..50u64).map(|n| n * 40 * MS).collect();
        let mut timeline = PresentationTimeline::default();
        assert!(steps(&run(&mut timeline, &source, 40 * MS)).iter().all(|&s| s == 40 * MS));
    }

    #[test]
    fn a_new_channel_and_a_real_discontinuity_reanchor_the_grid() {
        let mut timeline = PresentationTimeline::default();
        assert_eq!(timeline.schedule(1, 100 * MS, Some(20 * MS)), Some(100 * MS));
        assert_eq!(timeline.schedule(1, 121 * MS, Some(20 * MS)), Some(120 * MS));
        assert_eq!(timeline.schedule(2, 900 * MS, Some(20 * MS)), Some(900 * MS));
        assert_eq!(timeline.schedule(2, 4_000 * MS, Some(20 * MS)), Some(4_000 * MS));
        assert_eq!(timeline.schedule(2, 4_021 * MS, Some(20 * MS)), Some(4_020 * MS));
    }

    /// Pictures the decoder lost for good (four at once): the stream's time
    /// stays 80 ms ahead of the grid, past R37's 60 ms; two seconds later
    /// the grid moves to within it, once, by whole periods.
    #[test]
    fn a_lasting_loss_is_caught_up_after_two_seconds_in_whole_periods() {
        let mut source: Vec<u64> = (0..50u64).map(|n| n * 20 * MS).collect();
        source.extend((54..400u64).map(|n| n * 20 * MS));
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        let moves: Vec<(usize, u64)> =
            steps(&scheduled).into_iter().enumerate().filter(|&(_, s)| s != 20 * MS).collect();
        assert_eq!(moves.len(), 1, "{moves:?}");
        assert_eq!(moves[0].1, 40 * MS, "one period: 80 ms off is 20 beyond the limit");
        assert!((145..=160).contains(&moves[0].0), "two seconds on: {moves:?}");
        assert!(source.last().unwrap() - scheduled.last().unwrap() <= 60 * MS);
        assert_eq!(scheduled.iter().filter(|&&s| s % (20 * MS) != 0).count(), 0, "on the grid");
    }

    /// The board's clock against the broadcaster's, over a long run: the
    /// grid follows with a single slot moved now and then, never seconds.
    #[test]
    fn a_slow_clock_drift_is_followed_by_single_slots() {
        let mut timeline = PresentationTimeline::default();
        // 100 ppm, far beyond a broadcaster's 30: 2 us a picture.
        let mut moved = 0;
        let mut last = timeline.schedule(1, 0, Some(20 * MS)).unwrap();
        for k in 1..200_000u64 {
            let source = k * 20 * MS + k * 2_000;
            let scheduled = timeline.schedule(1, source, Some(20 * MS)).unwrap();
            if scheduled - last != 20 * MS {
                moved += 1;
                assert_eq!(scheduled - last, 40 * MS, "one slot at a time");
            }
            assert!(source - scheduled <= 60 * MS + 2 * 2_000 * 100);
            last = scheduled;
        }
        assert!((10..=25).contains(&moved), "{moved}");
    }

    #[test]
    fn a_first_picture_timed_far_from_its_key_frame_is_not_shown() {
        let mut timeline = PresentationTimeline::default();
        let key = 316_677_462_958;
        assert!(timeline.bogus_first(23, 80 * MS, Some(key)));
        assert!(timeline.bogus_first(23, 158_398_731_000, Some(key)));
        assert!(!timeline.bogus_first(23, 316_717_462_000, Some(key)));
        timeline.schedule(23, 316_717_462_000, Some(40 * MS));
        // Once the channel has a picture up, the key frame says nothing.
        assert!(!timeline.bogus_first(23, 316_717_462_000 + 600_000 * MS, Some(key)));
        // A new channel asks again.
        assert!(timeline.bogus_first(24, 80 * MS, Some(key)));
    }

    #[test]
    fn missing_or_implausible_duration_uses_the_source_timestamp() {
        let mut timeline = PresentationTimeline::default();
        assert_eq!(timeline.schedule(1, 100 * MS, None), Some(100 * MS));
        assert_eq!(timeline.schedule(1, 140 * MS, Some(MS)), Some(140 * MS));
        assert_eq!(timeline.schedule(1, 180 * MS, Some(250 * MS)), Some(180 * MS));
    }
}
