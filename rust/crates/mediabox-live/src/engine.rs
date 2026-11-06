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
use crate::stamp::{Class, LiveEdge, Packetizer, Piece, Stamp, Stamped};

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
    /// The live monitor's player side (`health`).
    pub health: crate::health::Health,
    pub faults: std::sync::OnceLock<crate::health::Faults>,
    /// The sound being played, fed packet by packet by the reader (`es`):
    /// its part of the channel's branch.
    pub sound: Mutex<Option<SoundParts>>,
    /// The running times this reader gave the last sound PES starts, for
    /// tsdemux's own to be held against (`sound_source`).
    pub sound_starts: Mutex<std::collections::VecDeque<u64>>,
    pub info: Mutex<Info>,
    /// Another audio stream chosen while a channel plays (`set_aid`): its
    /// generation and order, for the reader to take the sound to
    /// (`es::Reader::choose`) -- the picture goes on as it is.
    pub aid_request: Mutex<Option<(u64, usize)>>,
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
    /// Its streams' state: what a sound built again is gated by.
    programme: Arc<Programme>,
}

/// The sound's part of a channel's branch: from its own source, fed by the
/// reader (`route_sound`), to its input of the sound selector. Built again,
/// alone, when the PMT gives the sound another codec (`sound_changed`);
/// taken down with its branch.
pub struct SoundParts {
    pub generation: u64,
    /// The PID the reader gives it from.
    pub pid: u16,
    /// The codec it was built for.
    pub codec: Option<crate::es::Codec>,
    pub source: gst_app::AppSrc,
    elements: Arc<Mutex<Vec<gst::Element>>>,
    pads: Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>,
}

/// The picture's part of a channel's branch: from tsdemux's pad of one
/// program generation to its input of the picture selector.
///
/// tsdemux puts a programme whose PMT changed up again: every stream on a
/// new pad, named with the next program generation, the new pads added
/// before the old ones are drained, sent EOS and removed (GStreamer 1.26
/// tsdemux.c, gst_ts_demux_program_started; mpegtsbase.c,
/// mpegts_base_apply_pmt). The picture is then handed over, not taken for a
/// second stream: the new generation's pad gets a part of its own, held
/// until its first key frame; the selector switches to it; the old pad's
/// data and its EOS go nowhere; the old part is taken down. The panel keeps
/// the last picture shown until the new one's first -- no black, as for a
/// channel change -- and nothing decoded by the old part is shown after.
/// The times stay the stream's: the same PCRs stamp both generations
/// (`stamp::Stamp`), and the new pad's segment is tsdemux's on them.
struct VideoParts {
    program: Option<String>,
    pad: gst::Pad,
    elements: Arc<Mutex<Vec<gst::Element>>>,
    pads: Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>,
}

/// The program generation of a tsdemux pad: the second field of its name
/// ("video_1_0100": generation 1; tsdemux.c, activate_pad_for_stream).
fn program_generation(pad: &gst::Pad) -> Option<String> {
    pad.name().split('_').nth(1).map(str::to_string)
}

/// Each sound source its own name: one built again for the same PID lives
/// beside the old one until that is taken down.
static SOUND_SOURCES: AtomicU64 = AtomicU64::new(0);

/// The caps tsdemux gives a sound stream of `codec` (GStreamer 1.26.2
/// tsdemux.c, create_pad_for_stream: MPEG audio 1433, ADTS 1736-1741, LATM
/// 1744-1747, AC-3 and E-AC-3 1990-2035), for a sound branch built for the
/// codec the PMT names (`es::Codec`).
pub fn sound_caps(codec: crate::es::Codec) -> Option<gst::Caps> {
    use crate::es::Codec;
    Some(match codec {
        Codec::MpegAudio => gst::Caps::builder("audio/mpeg").field("mpegversion", 1i32).build(),
        Codec::AacAdts => gst::Caps::builder("audio/mpeg").field("mpegversion", 4i32).field("stream-format", "adts").build(),
        Codec::AacLatm => gst::Caps::builder("audio/mpeg").field("mpegversion", 4i32).field("stream-format", "loas").build(),
        Codec::Ac3 => gst::Caps::builder("audio/x-ac3").build(),
        Codec::Eac3 => gst::Caps::builder("audio/x-eac3").build(),
        Codec::Dts | Codec::Other(_) => return None,
    })
}

/// The codec of sound of `caps`: `sound_caps` read back.
pub fn codec_of(caps: &gst::Caps) -> Option<crate::es::Codec> {
    use crate::es::Codec;
    let s = caps.structure(0)?;
    Some(match (s.name().as_str(), s.get::<i32>("mpegversion").ok(), s.get::<&str>("stream-format").ok()) {
        ("audio/mpeg", Some(1), _) => Codec::MpegAudio,
        ("audio/mpeg", _, Some("loas")) => Codec::AacLatm,
        ("audio/mpeg", _, _) => Codec::AacAdts,
        ("audio/x-ac3", _, _) => Codec::Ac3,
        ("audio/x-eac3", _, _) => Codec::Eac3,
        _ => return None,
    })
}

/// The parser for sound of `caps`.
pub fn parser_for(caps: &gst::Caps) -> Option<&'static str> {
    let structure = caps.structure(0)?;
    match (structure.name().as_str(), structure.get::<i32>("mpegversion").ok()) {
        ("audio/mpeg", Some(1)) => Some("mpegaudioparse"),
        ("audio/mpeg", _) => Some("aacparse"),
        ("audio/x-ac3" | "audio/x-eac3", _) => Some("ac3parse"),
        _ => None,
    }
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
    /// The program generation of the audio pads counted (tsdemux's pad
    /// names, `stream`).
    audio_program: Mutex<Option<String>>,
    /// The picture being played (`stream`, `retire_video`).
    video: Mutex<Option<VideoParts>>,
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
    /// The sound began with a frame that started before the key frame's
    /// time: its samples before that time are cut once decoded (`trim`).
    /// 0: nothing to cut.
    trim_from: AtomicU64,
}

/// How long sound waits for a picture that does not come.
/// The most a picture may be late and still be shown, whatever its
/// duration (`attach_video`): it is one frame's time.
const LATE_DROP_NS: u64 = 200_000_000;

const PICTURE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

impl Programme {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            opened: Instant::now(),
            sounding: AtomicBool::new(false),
            held: AtomicU64::new(0),
            audio_order: Mutex::new(0),
            audio_program: Mutex::new(None),
            video: Mutex::new(None),
            video_taken: AtomicBool::new(false),
            picture: Mutex::new(None),
            known: std::sync::Condvar::new(),
            waited: AtomicBool::new(false),
            no_picture: AtomicBool::new(false),
            held_since: Mutex::new(None),
            trim_from: AtomicU64::new(0),
        }
    }

    /// The first key frame is known, or that there is none to wait for.
    fn picture_known(&self) {
        let _picture = self.picture.lock().unwrap_or_else(|p| p.into_inner());
        self.known.notify_all();
    }

    /// Whether a sound frame at `pts`, `duration` long, is played.
    ///
    /// The sound starts where the picture does: with the frame whose time
    /// takes in the first key frame's, its samples before that time cut
    /// once decoded (`trim`). Started with the first frame from the key
    /// frame's time on, it began up to a frame later than the picture --
    /// MPEG audio's 24 ms: 20 of 63 openings had it more than 20 ms after
    /// the first picture (FAZ 1M, 2026-11-06). `whole`: frames that cannot
    /// be cut (a bitstream) start from the key frame's time on.
    ///
    /// Until the key frame's time is known a frame waits here, on the
    /// sound's own thread (its queue takes what comes meanwhile), rather
    /// than being dropped: in an opening's burst the sound reaches here
    /// before the key frame reaches its parser, and the frames after the
    /// key frame's time went with those before it -- the sound joined the
    /// picture 316 ms late (measured 2026-11-01: 35 frames dropped in the
    /// 2.3 ms before the key frame was parsed). `stop`: the branch is being
    /// taken down, and waits no more.
    fn sound(&self, pts: Option<u64>, duration: Option<u64>, whole: bool, stop: &dyn Fn() -> bool) -> bool {
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
                Some(first)
                    if !whole
                        && let (Some(pts), Some(duration)) = (pts, duration)
                        && pts + duration > first =>
                {
                    self.trim_from.store(first, Ordering::SeqCst);
                    break Some("ilk anahtar karenin zamanını içeren çerçeveden");
                }
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

/// How long past its PTS on the broadcast's clock a picture and its sound
/// are presented: the receiver's own margin, nothing of the broadcast's.
///
/// The sound is fed packet by packet (`es`), so it comes before its PTS on
/// every service; the picture comes 270-600 ms before its own. Measured on
/// the Plus with the Kanky, 2026-11-02, 30 s on each of TRT 1, 10803, NTV,
/// SD 16300 and 10901 with the live monitor: at 200 and at 150 ms no sound
/// came late, none was repaired by the sink, and the sound's latest was
/// 3-51 ms ahead of its PTS; behind the broadcast 148-192 ms. With
/// tsdemux's sound it had taken 700 ms (`DEMUX_LATENCY_MS`).
pub const LATENCY_MS: u64 = 150;

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

    /// `step` as a whole number of `period`s, within a millisecond (MPP
    /// gives the broadcast's PTS back in microseconds); 0 when it is not.
    fn whole_periods(step: u64, period: u64) -> u64 {
        if period == 0 {
            return 0;
        }
        let n = (step + period / 2) / period;
        if n > 0 && step.abs_diff(n * period) <= 1_000_000 { n } else { 0 }
    }

    /// The picture period: `duration`, or half of it when the source
    /// timestamps step by half of it (one picture a field).
    fn period(&self, duration: u64) -> u64 {
        if self.steps.is_empty() {
            return duration;
        }
        let half = duration / 2;
        // One picture a field shows from a single step: an odd number of
        // fields exactly (MPP's first gap after a key frame is three, 60 ms
        // for a picture saying 40 -- TV8 HD, 2026-11-03). Taken as 40 then,
        // the opening's pictures went 40, 40, 20. Fields are 20 or 16.7 ms
        // (50 or 60 Hz): a half below 15 ms is no field.
        if half >= 15_000_000
            && self.steps.iter().any(|&step| {
                let n = (step + half / 2) / half;
                n % 2 == 1 && step.abs_diff(n * half) <= 1_000_000
            })
        {
            return half;
        }
        let mut steps: Vec<u64> = self.steps.iter().copied().collect();
        steps.sort_unstable();
        let median = steps[steps.len() / 2];
        if half >= Self::MIN_PERIOD_NS && median.abs_diff(half) * 4 <= half { half } else { duration }
    }

    /// The slot for a picture at `source_pts`; `None`: left out, to catch
    /// up with the sound.
    fn schedule(&mut self, generation: u64, source_pts: u64, duration_ns: Option<u64>) -> Option<u64> {
        if self.generation != generation {
            *self = Self { generation, ..Self::default() };
        }
        let previous_source = self.last_source;
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
                } else if previous_source == Some(last) && Self::whole_periods(source_pts - last.min(source_pts), period) >= 2 {
                    // Pictures the decoder never gave, from a picture shown
                    // at its own time: the next one is shown at its own time
                    // too. Measured on the Plus, 2026-11-02: MPP's
                    // deinterlacer gives no picture for the two fields after
                    // a channel's first key frame (60 ms, KANAL D, TV8, CNN
                    // TURK, NHK), and a TS packet lost on the picture's PID
                    // costs everything up to the next I picture (1.02-1.1 s,
                    // TRT WORLD). Held on the grid, the pictures after the
                    // gap were put that much before their time: 40 ms steps
                    // at every opening, and after a lost GOP the sink threw
                    // away two seconds of good pictures as late (99 a time).
                    self.beyond_ns = 0;
                    self.catching_up = false;
                    source_pts
                } else if early && source_pts - next > Self::JUMP_NS {
                    // A jump forward is not jitter: to the slot nearest its
                    // time at once. Waiting `PERSIST_NS` with pictures put
                    // more than the latency before their time loses them all.
                    self.beyond_ns = 0;
                    self.catching_up = false;
                    let off = source_pts - next;
                    next + (off + period / 2) / period * period
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
        // The whole pipeline's latency, set rather than asked of its elements
        // (tsdemux would say 700 ms): what a picture and its sound wait past
        // their PTS on the broadcast's clock (`LATENCY_MS`).
        let ms = std::env::var("MEDIABOX_LIVE_LATENCY_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(LATENCY_MS);
        pipeline.set_latency(gst::ClockTime::from_mseconds(ms));
        eprintln!("mediabox-live: boru hattı gecikmesi {ms} ms");

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
            let faults = crate::health::Faults::get(&shared.faults);
            let fault_count = AtomicU64::new(0);
            // The stall's clock starts at the sound's first buffer: one in
            // the channel's first second (`Health::settled`) is its start,
            // which the diagnosis does not judge.
            let fault_stalled: Mutex<Option<Instant>> = Mutex::new(None);
            audio_selector.static_pad("src").expect("input-selector has a src pad").add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                let n = fault_count.fetch_add(1, Ordering::Relaxed) + 1;
                if crate::health::Faults::ON && faults.audio_drop > 0 && n % faults.audio_drop == 0 {
                    mediabox_core::timeline!("fault", "audio-drop n={n}");
                    return gst::PadProbeReturn::Drop;
                }
                if crate::health::Faults::ON && faults.audio_stall_ms > 0 {
                    let mut at = fault_stalled.lock().unwrap_or_else(|p| p.into_inner());
                    let since = *at.get_or_insert_with(Instant::now);
                    if since.elapsed() >= std::time::Duration::from_secs(5) {
                        *at = Some(Instant::now());
                        mediabox_core::timeline!("fault", "audio-stall ms={}", faults.audio_stall_ms);
                        std::thread::sleep(std::time::Duration::from_millis(faults.audio_stall_ms));
                    }
                }
                if let (Some(buffer), Some(pipeline)) = (info.buffer(), weak_pipeline.upgrade())
                    && let (Some(pts), Some(base)) = (buffer.pts(), pipeline.base_time())
                {
                    let now = pipeline_clock.time().saturating_sub(base).nseconds() as i64;
                    shared.audio_lead_ns.store(pts.nseconds() as i64 - now, Ordering::Relaxed);
                    let generation = shared.generation.load(Ordering::SeqCst);
                    shared.health.audio(generation, pts.nseconds(), buffer.duration().map(|d| d.nseconds()));
                    let latency = shared.health.latency_ns.load(Ordering::Relaxed);
                    if latency > 0 {
                        shared.health.audio_lead(pts.nseconds() as i64 + latency + shared.health.audio_offset_ns.load(Ordering::Relaxed) - now);
                    }
                    mediabox_core::timeline!(
                        "a-out",
                        "gen={generation} pts={} rt={now} dur={}",
                        pts.nseconds(),
                        buffer.duration().map_or(-1, |d| d.nseconds() as i64)
                    );
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
            audio: {
                let (gain, muted) = (options.audio.gain, options.audio.muted);
                shared.health.diagnose(|d| {
                    d.context.gain = gain;
                    d.context.muted = muted;
                });
                Mutex::new(options.audio.clone())
            },
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
        engine.attach_audio_sink("start")?;
        engine.attach_video(&video_sink, plane, trace);
        engine
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("pipeline başlatılamadı: {e}"))?;
        engine.watch_health();
        listen_to_audio_sink(&engine.shared);
        Ok(engine)
    }

    /// tsdemux's pad for the sound played: drained (the reader gives the
    /// sound, `route_sound`: tsdemux gives a PES only once the whole of it
    /// is in, up to 283 ms after its first frame's time), its PES times held
    /// against the reader's (`Health::sound_map`) -- the two must agree.
    fn watch_sound_pad(&self, pad: &gst::Pad) {
        let shared = Arc::downgrade(&self.shared);
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let (Some(pts), Some(shared)) = (info.buffer().and_then(|b| b.pts()), shared.upgrade()) {
                let theirs = pts.nseconds() as i64;
                let ours = shared
                    .sound_starts
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .iter()
                    .map(|&at| at as i64)
                    .min_by_key(|&at| (at - theirs).abs());
                if let Some(ours) = ours {
                    shared.health.sound_map(theirs - ours);
                }
            }
            gst::PadProbeReturn::Ok
        });
    }

    /// The reader followed the PMT to another sound (`es::Switch`). The
    /// same codec goes on into the same branch; another codec gets the
    /// sound's part of the branch built again for it and the old part taken
    /// down -- the channel goes on, picture and all. The source to feed, or
    /// None when there is none for it (said).
    pub fn sound_changed(self: &Arc<Self>, generation: u64, switch: &crate::es::Switch) -> Option<gst_app::AppSrc> {
        let programme = {
            let current = self.current.lock().unwrap_or_else(|p| p.into_inner());
            let branch = current.as_ref().filter(|b| b.generation == generation)?;
            Arc::clone(&branch.programme)
        };
        let old = {
            let mut sound = self.shared.sound.lock().unwrap_or_else(|p| p.into_inner());
            if !switch.rebuild
                && let Some(parts) = sound.as_mut().filter(|p| p.generation == generation)
            {
                parts.pid = switch.pid;
                eprintln!("mediabox-live: ses PMT ile PID {:#06x}'e geçti ({})", switch.pid, switch.codec.name());
                return Some(parts.source.clone());
            }
            sound.take()
        };
        if let Some(old) = old {
            self.take_down_sound(old);
        }
        let Some(caps) = sound_caps(switch.codec) else {
            eprintln!("mediabox-live: ses PMT ile {} oldu: oynatılamıyor", switch.codec.name());
            return None;
        };
        match self.sound_branch(generation, &caps, &programme) {
            Ok(mut parts) => {
                parts.pid = switch.pid;
                let source = parts.source.clone();
                *self.shared.sound.lock().unwrap_or_else(|p| p.into_inner()) = Some(parts);
                eprintln!("mediabox-live: ses PMT ile {} oldu, PID {:#06x}: ses kolu yeniden kuruldu", switch.codec.name(), switch.pid);
                Some(source)
            }
            Err(error) => {
                eprintln!("mediabox-live: ses kolu {} için kurulamadı: {error}", switch.codec.name());
                None
            }
        }
    }

    /// The picture of a program generation tsdemux has replaced: nothing
    /// more of its pad goes in -- its drained data and its EOS, which would
    /// end the picture -- and its part is taken down.
    fn retire_video(&self, generation: u64, old: VideoParts) {
        old.pad.add_probe(gst::PadProbeType::DATA_DOWNSTREAM, |_, info| match &info.data {
            Some(gst::PadProbeData::Event(event)) if matches!(event.type_(), gst::EventType::FlushStart | gst::EventType::FlushStop) => gst::PadProbeReturn::Ok,
            _ => gst::PadProbeReturn::Drop,
        });
        mediabox_core::timeline!("video-handover", "gen={generation} retired={:?}", old.program);
        self.take_down_lists(format!("video-down-{generation}"), vec![(old.elements, old.pads)]);
    }

    fn take_down_sound(&self, parts: SoundParts) {
        self.take_down_lists(format!("sound-down-{}", parts.generation), vec![(parts.elements, parts.pads)]);
    }

    /// How late each picture leaves the demultiplexer against its PTS
    /// (`Health::demuxed`); the sound's is taken where the reader gives it
    /// (`push_sound`).
    fn measure_demuxed(&self, generation: u64, pad: &gst::Pad) {
        let name = pad.name();
        let audio = false;
        if !name.starts_with("video") {
            return;
        }
        let shared = Arc::downgrade(&self.shared);
        // The pipeline's clock, taken here: asked for from the streaming
        // thread (`Element::clock`), it waits on the pipeline's lock, which
        // a state change holds while it waits for this thread -- and nothing
        // played (2026-11-02). The same clock the pipeline uses (`new`).
        let clock = gst::SystemClock::obtain();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let (Some(buffer), Some(shared)) = (info.buffer(), shared.upgrade())
                && let Some(pts) = buffer.pts()
            {
                // The base time as the health thread read it (`watch_health`):
                // `Element::base_time` takes the pipeline's lock too.
                let base = shared.health.base_ns.load(Ordering::Relaxed);
                if base == 0 {
                    return gst::PadProbeReturn::Ok;
                }
                let now = clock.time().nseconds() as i64 - base as i64;
                shared.health.demuxed(generation, audio, now - pts.nseconds() as i64);
            }
            gst::PadProbeReturn::Ok
        });
    }

    /// The live monitor's player side (`health`): the sound device sampled
    /// every 10 ms -- a queue running low lasts a moment -- and a report
    /// every `health::PERIOD`, while the engine lives.
    fn watch_health(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new().name("live-health".into()).spawn(move || {
            let mut device = crate::health::Device::default();
            let mut device_for: Option<String> = None;
            let mut last_report = Instant::now();
            let mut last_probe = Instant::now();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(10));
                let Some(engine) = weak.upgrade() else { return };
                let name = engine.audio.lock().unwrap_or_else(|p| p.into_inner()).device.clone();
                if name != device_for {
                    device = name.as_deref().map(crate::health::Device::for_name).unwrap_or_default();
                    device_for = name;
                }
                device.sample();
                if last_report.elapsed() < crate::health::PERIOD {
                    continue;
                }
                last_report = Instant::now();
                if let Some(base) = engine.pipeline.base_time() {
                    engine.shared.health.base_ns.store(base.nseconds(), Ordering::Relaxed);
                }
                let latency = engine.shared.health.latency_ns.load(Ordering::Relaxed);
                let video_offset = engine.video_sink.property::<i64>("ts-offset");
                let sink = engine.audio_sink.lock().unwrap_or_else(|p| p.into_inner()).clone();
                let audio_offset = sink.as_ref().map_or(0, |sink| sink.property::<i64>("ts-offset"));
                engine.shared.health.audio_offset_ns.store(audio_offset, Ordering::Relaxed);
                // How late the device plays against the schedule: the
                // pipeline's clock less the sound clock, which is the
                // device's played samples mapped onto it (audiobasesink's
                // calibration). Only while sound is flowing.
                let audio_late = sink.as_ref().filter(|_| device.is_running()).and_then(|sink| {
                    let audio_clock = sink.provide_clock()?;
                    let pipeline_clock = engine.pipeline.clock()?;
                    (sink.current_state() == gst::State::Playing)
                        .then(|| pipeline_clock.time().nseconds() as i64 - audio_clock.time().nseconds() as i64)
                });
                let generation = engine.shared.generation.load(Ordering::SeqCst);
                if let Some(probe) = crate::clock_probe::get()
                    && last_probe.elapsed() >= std::time::Duration::from_secs(1)
                {
                    last_probe = Instant::now();
                    probe.flush(engine.probe_levels(generation, audio_late, &device));
                }
                engine.shared.health.plane_full_dropped.store(engine.plane.dropped_full(), Ordering::Relaxed);
                let report = engine.shared.health.report(
                    generation,
                    latency + video_offset,
                    latency + audio_offset,
                    audio_late,
                    &mut device,
                );
                crate::health::write(&report);
            }
        });
    }

    /// What the clock probe samples once a second: the transport source's
    /// and every queue's fill, the sound device's delay and the health
    /// counters the sound and picture are judged by.
    fn probe_levels(&self, generation: u64, audio_late: Option<i64>, device: &crate::health::Device) -> serde_json::Value {
        let mut queues = serde_json::Map::new();
        let mut source = serde_json::Value::Null;
        for element in self.pipeline.iterate_recurse().into_iter().flatten() {
            let factory = element.factory().map(|f| f.name().to_string()).unwrap_or_default();
            if factory == "queue" {
                queues.insert(
                    element.name().to_string(),
                    serde_json::json!([
                        element.property::<u64>("current-level-time"),
                        element.property::<u32>("current-level-buffers"),
                        element.property::<u32>("current-level-bytes"),
                    ]),
                );
            } else if factory == "appsrc" && element.name() == format!("ts{generation}").as_str() {
                source = serde_json::json!([element.property::<u64>("current-level-bytes"), element.property::<u64>("current-level-time"), element.property::<u64>("current-level-buffers")]);
            }
        }
        let h = &self.shared.health;
        serde_json::json!({
            "generation": generation,
            "appsrc": source,
            "queues": queues,
            "audio_late": audio_late,
            "device": device.probe(),
            "video_dropped": h.video_dropped.load(Ordering::Relaxed),
            "video_lag": self.shared.video_lag_ns.load(Ordering::Relaxed),
            "sound": h.probe_counters(),
        })
    }

    pub fn bus(&self) -> gst::Bus {
        self.pipeline.bus().expect("a pipeline has a bus")
    }

    /// The sound device, opened once and kept: alsasink on the plan's PCM,
    /// slaved to the pipeline's clock.
    /// `why`: "start", "mode" (PCM or a bitstream), "reopen" (the device
    /// lost, or asked again), "aid" (sound chosen back on).
    fn attach_audio_sink(&self, why: &str) -> Result<(), String> {
        let audio = self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone();
        self.shared.health.sink_opened(audio.device.clone(), why);
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
                // The device's sample clock is the board's crystal, the
                // pipeline's CLOCK_MONOTONIC, which NTP steers off it: they
                // part by some 17 ppm. audiobasesink's default (`skew`) lets
                // that grow to half its drift-tolerance (40 ms) and then
                // moves the clock's calibration by the whole of it, which
                // the next buffer's alignment to the one before (under the
                // 40 ms alignment-threshold) takes back: on TRT 1 the sound
                // stood 2 to 22 ms off its time and jumped by 10 or 20 ms
                // 14 times in 65 minutes (FAZ 1L, 2026-11-05). `resample`
                // puts each buffer's samples where the slaved clock says,
                // its end unaligned, so a sample comes or goes now and then
                // and the sound stays on its time (gstaudiobasesink.c 1.26,
                // render: "no stop time align needed: we are slaved"). A
                // bitstream's bursts are not samples to stretch: it keeps
                // the default.
                if !self.bitstream.load(Ordering::SeqCst) {
                    sink.set_property_from_str("slave-method", "resample");
                }
                sink
            }
            None => make("fakesink")?,
        };
        sink.set_property("sync", true);
        sink.set_property("async", false);
        sink.set_property("enable-last-sample", false);
        sink.set_property("ts-offset", self.audio_offset());
        if let Some(pad) = sink.static_pad("sink") {
            probe_layer(&pad, crate::clock_probe::Layer::Sink, None, &self.shared);
        }
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
        // A bitstream is not samples: the PCM layers do not apply to it.
        self.shared.health.diagnose(|d| d.context.passthrough = bitstream);
        if self.bitstream.swap(bitstream, Ordering::SeqCst) == bitstream {
            return;
        }
        self.detach_audio_sink();
        if let Err(error) = self.attach_audio_sink("mode") {
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
        if let Err(error) = self.attach_audio_sink("reopen") {
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
        let faults = crate::health::Faults::get(&shared.faults);
        let mut fault_frames = 0u64;
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
                        shared.health.video_dropped.fetch_add(1, Ordering::Relaxed);
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    last = Some((generation, pts));
                    let Some(picture) = described.as_ref().and_then(|found| picture(found, buffer)) else {
                        // Not a dma-buf the plane takes: a software decoder.
                        // Traced, never copied -- the whole point is that
                        // nothing is.
                        trace.event('X', 0, pts / 1000);
                        mediabox_core::timeline!("drop", "gen={generation} pts_us={} why=not-dmabuf", pts / 1000);
                        shared.health.video_dropped.fetch_add(1, Ordering::Relaxed);
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    // When it is due on the panel, on the clock's own time
                    // (CLOCK_MONOTONIC): from its PTS, not from when this
                    // thread woke, so frames due 20 ms apart are 20 ms apart.
                    let mut due = 0;
                    if let (Some(clock), Some(base)) = (sink.clock(), sink.base_time()) {
                        let now = clock.time().saturating_sub(base).nseconds() as i64;
                        shared.video_lag_ns.store(now - pts as i64, Ordering::Relaxed);
                        if let Some(probe) = crate::clock_probe::get() {
                            probe.video(pts, now as u64);
                        }
                        // The latency the sinks wait by, asked once a channel.
                        if latency.is_none_or(|(asked_in, _)| asked_in != generation) {
                            // The pipeline's: the one every sink was given.
                            let mut query = gst::query::Latency::new();
                            let ns = match sink.parent().and_then(|parent| parent.downcast::<gst::Pipeline>().ok()) {
                                // A latency set on the pipeline is the one its sinks were given.
                                Some(pipeline) if pipeline.latency().is_some() => pipeline.latency().map_or(0, |l| l.nseconds()),
                                Some(pipeline) if pipeline.query(&mut query) => query.result().1.nseconds(),
                                _ => 0,
                            };
                            latency = Some((generation, ns));
                            shared.health.latency_ns.store(ns as i64, Ordering::Relaxed);
                            mediabox_core::timeline!("sink-latency", "gen={generation} ns={ns} base={}", base.nseconds());
                        }
                        // And the sink's offset: a channel that started
                        // behind the broadcast is shown that much earlier.
                        let offset = sink.property::<i64>("ts-offset");
                        due = (base.nseconds() + pts + latency.map_or(0, |(_, ns)| ns)).saturating_add_signed(offset);
                        // A picture already well past its time is the past:
                        // live television does not show it. After the stream
                        // had stopped (the player held, measured with SIGSTOP
                        // for 3 s, 2026-11-01) everything queued behind came
                        // out at once, 3.7 s late, and the panel ran through
                        // a second of old pictures before it was live again.
                        // Frames on time are sent at least 5 ms early (every
                        // opening measured that day), so this never touches
                        // them; the sink's own max-lateness stays off, which
                        // dropped a channel's second frame (QoS).
                        // One frame's time late is already one the panel
                        // would show at the vblank of the next: the frames
                        // a channel's start came ahead of (`set_behind`)
                        // were let through within 200 ms and piled up on
                        // one vblank, ten of them a channel, the start a
                        // short fast-forward (2026-11-01).
                        let frame = buffer.duration().map_or(20_000_000, |d| d.nseconds()).clamp(10_000_000, LATE_DROP_NS);
                        let late = clock.time().nseconds().saturating_sub(due);
                        if late > frame {
                            trace.event('L', 0, pts / 1000);
                            mediabox_core::timeline!("drop", "gen={generation} pts_us={} why=late late_ms={}", pts / 1000, late / 1_000_000);
                            shared.health.video_dropped.fetch_add(1, Ordering::Relaxed);
                            return Ok(gst::FlowSuccess::Ok);
                        }
                    }
                    fault_frames += 1;
                    if crate::health::Faults::ON && faults.skip > 0 && fault_frames % faults.skip == 0 {
                        mediabox_core::timeline!("fault", "skip gen={generation} pts_us={}", pts / 1000);
                        return Ok(gst::FlowSuccess::Ok);
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
        // No offset is carried from a channel before (`set_behind`).
        let fault = crate::health::Faults::get(&self.shared.faults).video_late_ms * 1_000_000;
        self.video_sink.set_property("ts-offset", fault);
        if let Some(sink) = self.audio_sink.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            sink.set_property("ts-offset", self.audio_offset());
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
            let engine = Arc::downgrade(self);
            std::thread::Builder::new()
                .name("ts-reader".into())
                .spawn(move || {
                    let on_behind = |generation: u64, behind: u64| {
                        if let Some(engine) = engine.upgrade() {
                            engine.set_behind(generation, behind);
                        }
                    };
                    let on_sound = |generation: u64, switch: &crate::es::Switch| engine.upgrade().and_then(|engine| engine.sound_changed(generation, switch));
                    read_stream(&reader_path, &reader_into, &reader_stop, &shared, &on_behind, &on_sound);
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
        let branch_programme = Arc::clone(&programme);
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
            demux.connect_pad_added(move |_, pad| {
                let Some(engine) = weak.upgrade() else { return };
                if mediabox_core::timeline::enabled() {
                    trace_demux_pad(generation, pad);
                }
                engine.measure_demuxed(generation, pad);
                if engine.shared.generation.load(Ordering::SeqCst) != generation {
                    engine.discard(pad, &elements);
                    return;
                }
                if let Err(error) = engine.stream(generation, pad, &elements, &programme) {
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
        Ok((Branch { generation, elements, pads, programme: branch_programme }, source))
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
        programme: &Arc<Programme>,
    ) -> Result<(), String> {
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let Some(structure) = caps.structure(0) else {
            return Err("biçimi yok".into());
        };
        let name = structure.name().as_str();
        let version = structure.get::<i32>("mpegversion").ok();
        if name.starts_with("video/") {
            // One picture stream per program generation: a second one of the
            // same generation is drained; one of a new generation takes over
            // from the one before (`VideoParts`).
            let program = program_generation(pad);
            let replacing = {
                let video = programme.video.lock().unwrap_or_else(|p| p.into_inner());
                match video.as_ref() {
                    Some(parts) if parts.program == program => {
                        drop(video);
                        self.discard(pad, elements);
                        return Ok(());
                    }
                    Some(_) => true,
                    None => false,
                }
            };
            programme.video_taken.store(true, Ordering::SeqCst);
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
            // This part's own first key frame: what its first decoded
            // picture is held against (`PresentationTimeline::bogus_first`).
            // Not the channel's: after a PMT change the new part starts
            // seconds after the channel did, and every picture it gave was
            // taken for a bogus first one and thrown away (a fixture whose
            // PMT moves the picture 8 s in, through MPP and the plane,
            // 2026-11-03: 387 pictures, none shown).
            let branch_key: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
            // The first key frame says when the channel starts.
            if let Some(out) = parser.static_pad("src") {
                let programme = Arc::clone(programme);
                let branch_key = Arc::clone(&branch_key);
                out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    let Some(buffer) = info.buffer() else { return gst::PadProbeReturn::Ok };
                    if buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                        return gst::PadProbeReturn::Ok;
                    }
                    if let Some(pts) = buffer.pts() {
                        branch_key.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(pts.nseconds());
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
                let branch_key = Arc::clone(&branch_key);
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
                            let key = *branch_key.lock().unwrap_or_else(|p| p.into_inner());
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
            // A handed-over picture starts at its first key frame: what comes
            // before it depends on pictures the new part never had.
            if replacing && let Some(out) = parser.static_pad("src") {
                let started = AtomicBool::new(false);
                out.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if started.load(Ordering::Relaxed) {
                        return gst::PadProbeReturn::Ok;
                    }
                    let Some(buffer) = info.buffer() else { return gst::PadProbeReturn::Ok };
                    if buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                        return gst::PadProbeReturn::Drop;
                    }
                    started.store(true, Ordering::Relaxed);
                    mediabox_core::timeline!("video-handover", "gen={generation} first-key pts={:?}", buffer.pts());
                    gst::PadProbeReturn::Ok
                });
            }
            let video_elements: Arc<Mutex<Vec<gst::Element>>> = Arc::new(Mutex::new(Vec::new()));
            let video_pads: Arc<Mutex<Vec<(gst::Element, gst::Pad)>>> = Arc::new(Mutex::new(Vec::new()));
            self.chain(pad, &[parser, decoder, ahead], &self.video_selector, &video_elements, &video_pads)?;
            let new = VideoParts { program: program.clone(), pad: pad.clone(), elements: video_elements, pads: video_pads };
            let old = programme.video.lock().unwrap_or_else(|p| p.into_inner()).replace(new);
            if let Some(old) = old {
                eprintln!("mediabox-live: PMT değişti: görüntü program nesli {:?} -> {:?}, ilk anahtar kareden", old.program, program);
                self.retire_video(generation, old);
            }
            self.shared.info.lock().unwrap_or_else(|p| p.into_inner()).video_codec = Some(codec_name(name, version));
            return Ok(());
        }
        if !name.starts_with("audio/") {
            self.discard(pad, elements);
            return Ok(());
        }
        // tsdemux puts a programme whose PMT changed up again: every stream
        // on a new pad, the program generation the second field of its name
        // (GStreamer 1.26.2 tsdemux.c: activate_pad_for_stream 2078,
        // gst_ts_demux_program_started 2400-2470; mpegtsbase.c 1218-1247).
        // Its audio pads are counted from the first again, the tracks too.
        let program_generation = program_generation(pad);
        let order = {
            let mut seen = programme.audio_program.lock().unwrap_or_else(|p| p.into_inner());
            let mut order = programme.audio_order.lock().unwrap_or_else(|p| p.into_inner());
            if *seen != program_generation {
                if seen.is_some() {
                    *order = 0;
                    self.shared.info.lock().unwrap_or_else(|p| p.into_inner()).audio_tracks.clear();
                }
                *seen = program_generation;
            }
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
        // The sound already plays: the reader follows the PMT itself
        // (`es::Reader`, `sound_changed`); the programme's new pad is
        // drained, not built a second branch.
        if self.shared.sound.lock().unwrap_or_else(|p| p.into_inner()).as_ref().is_some_and(|s| s.generation == generation) {
            self.discard(pad, elements);
            return Ok(());
        }
        if parser_for(&caps).is_none() {
            self.discard(pad, elements);
            return Ok(());
        }
        let pid = pad
            .name()
            .rsplit('_')
            .next()
            .and_then(|hex| u16::from_str_radix(hex, 16).ok())
            .ok_or_else(|| format!("{}: PID okunamadı", pad.name()))?;
        let mut parts = self.sound_branch(generation, &caps, programme)?;
        parts.pid = pid;
        self.watch_sound_pad(pad);
        self.discard(pad, elements);
        *self.shared.sound.lock().unwrap_or_else(|p| p.into_inner()) = Some(parts);
        eprintln!("mediabox-live: ses PID {pid:#06x} paket paket okunuyor");
        self.shared.info.lock().unwrap_or_else(|p| p.into_inner()).aid = Some(order);
        Ok(())
    }

    /// The sound's part of a channel's branch for sound of `caps`: its own
    /// source (fed by the reader, `route_sound`), a queue, the parser --
    /// gated on the picture (`Programme::sound`) -- and the decoder or the
    /// bitstream path, to a new input of the sound selector.
    fn sound_branch(self: &Arc<Self>, generation: u64, caps: &gst::Caps, programme: &Arc<Programme>) -> Result<SoundParts, String> {
        let structure = caps.structure(0).ok_or("biçimi yok")?;
        let name = structure.name().as_str();
        let version = structure.get::<i32>("mpegversion").ok();
        let parser = parser_for(caps).ok_or_else(|| format!("{name} için ayrıştırıcı yok"))?;
        let codec = codec_name(name, version);
        let elements: Arc<Mutex<Vec<gst::Element>>> = Arc::new(Mutex::new(Vec::new()));
        let pads: Arc<Mutex<Vec<(gst::Element, gst::Pad)>>> = Arc::new(Mutex::new(Vec::new()));
        let source = gst_app::AppSrc::builder()
            .name(format!("sound-{generation}-{}", SOUND_SOURCES.fetch_add(1, Ordering::Relaxed)))
            .caps(caps)
            .format(gst::Format::Time)
            .is_live(true)
            .build();
        source.set_property("max-bytes", 4u64 << 20);
        self.adopt(&[source.upcast_ref()])?;
        elements.lock().unwrap_or_else(|p| p.into_inner()).push(source.clone().upcast());
        let from = source.static_pad("src").expect("appsrc has a src pad");
        self.shared.info.lock().unwrap_or_else(|p| p.into_inner()).passthrough = false;
        let audio = self.audio.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let passthrough_codec = match name {
            "audio/x-ac3" => Some(AudioCodec::Ac3),
            "audio/x-eac3" => Some(AudioCodec::Eac3),
            _ => None,
        };
        // AC-3 goes out as it came when the setting passes it through; E-AC-3
        // needs another burst type and is made AC-3 below.
        let passthrough = passthrough_codec == Some(AudioCodec::Ac3)
            && audio.passthrough.contains(&AudioCodec::Ac3)
            && audio.device.is_some();
        let mut chain = vec![make(parser)?];
        // Gated frame by frame, after the parser (`Programme::sound`): a
        // PES can hold up to 350 ms of sound, and the one holding the key
        // frame's time began before it -- gated whole, the sound joined the
        // picture up to 350 ms late (measured 2026-11-01, 32 openings).
        {
            let programme = Arc::clone(programme);
            let shared = Arc::clone(&self.shared);
            let faults = crate::health::Faults::get(&shared.faults);
            let jumped = Mutex::new(Instant::now());
            let framed = chain[0].static_pad("src").ok_or("ayrıştırıcının çıkışı yok")?;
            probe_layer(&framed, crate::clock_probe::Layer::Parser, Some(generation), &shared);
            framed.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                if crate::health::Faults::ON
                    && faults.pts_jump_ms > 0
                    && let Some(buffer) = info.buffer_mut()
                    && let Some(pts) = buffer.pts()
                {
                    let mut at = jumped.lock().unwrap_or_else(|p| p.into_inner());
                    if at.elapsed() >= std::time::Duration::from_secs(10) {
                        *at = Instant::now();
                        mediabox_core::timeline!("fault", "pts-jump ms={}", faults.pts_jump_ms);
                        buffer.make_mut().set_pts(pts + gst::ClockTime::from_mseconds(faults.pts_jump_ms));
                    }
                }
                let buffer = info.buffer();
                let pts = buffer.and_then(|buffer| buffer.pts()).map(|pts| pts.nseconds());
                // B: the frame on the sound's timeline, before the gate.
                if let Some(buffer) = buffer {
                    let duration = buffer.duration().map(|d| d.nseconds());
                    let discont = buffer.flags().contains(gst::BufferFlags::DISCONT);
                    shared.health.diagnose(|d| d.frame(generation, pts, duration, discont));
                }
                let stop = || pad.pad_flags().contains(gst::PadFlags::FLUSHING);
                let duration = buffer.and_then(|buffer| buffer.duration()).map(|d| d.nseconds());
                if programme.sound(pts, duration, passthrough, &stop) {
                    gst::PadProbeReturn::Ok
                } else {
                    shared.health.diagnose(|d| d.c.frames_gated += 1);
                    gst::PadProbeReturn::Drop
                }
            });
        }
        if passthrough {
            let head = self.head(&from, &chain, &elements, false)?;
            let framed = head.last().expect("a chain has elements").static_pad("src").ok_or("ayrıştırıcının çıkışı yok")?;
            self.sound_mode(true);
            self.bitstream_tail(&framed, None, &elements, &pads)?;
            for element in head.iter().rev() {
                element.sync_state_with_parent().map_err(|e| e.to_string())?;
            }
            from.link(&head[0].static_pad("sink").ok_or("kuyruğun girişi yok")?).map_err(|e| format!("{e:?}"))?;
            source.sync_state_with_parent().map_err(|e| e.to_string())?;
            eprintln!("mediabox-live: ses AC-3 olduğu gibi (IEC 61937)");
            let mut info = self.shared.info.lock().unwrap_or_else(|p| p.into_inner());
            info.audio_codec = Some(codec);
            info.passthrough = true;
            return Ok(SoundParts { generation, pid: 0, codec: codec_of(caps), source, elements, pads });
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
        let head = self.head(&from, &chain, &elements, false)?;
        let decoded = head.last().expect("a chain has elements").static_pad("src").ok_or("çözücünün çıkışı yok")?;
        self.watch_decoder(head.last().expect("a chain has elements"), generation);
        {
            let programme = Arc::clone(programme);
            decoded.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
                let from = programme.trim_from.swap(0, Ordering::SeqCst);
                if from == 0 {
                    return gst::PadProbeReturn::Ok;
                }
                let caps = pad.current_caps();
                let Some(buffer) = info.buffer_mut() else { return gst::PadProbeReturn::Ok };
                match caps.as_ref().and_then(|caps| trim(buffer, from, caps)) {
                    Some(0) | None => gst::PadProbeReturn::Ok,
                    Some(cut) => {
                        mediabox_core::timeline!("audio-trim", "gen={generation} from={from} frames={cut}");
                        gst::PadProbeReturn::Ok
                    }
                }
            });
        }
        if mediabox_core::timeline::enabled() {
            decoded.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(buffer) = info.buffer() {
                    mediabox_core::timeline!(
                        "a-dec",
                        "gen={generation} pts={} dur={}",
                        buffer.pts().map_or(-1, |t| t.nseconds() as i64),
                        buffer.duration().map_or(-1, |t| t.nseconds() as i64)
                    );
                }
                gst::PadProbeReturn::Ok
            });
        }
        // How many channels come out is known on the decoder's first caps,
        // and that decides the rest: the sound setting's Dolby Digital
        // encoding for three channels or more -- mpv's
        // `lavcac3enc=tospdif=yes:minch=N`, which this replaces -- and
        // PCM below that.
        let weak = Arc::downgrade(self);
        let (elements_tail, pads_tail) = (Arc::clone(&elements), Arc::clone(&pads));
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
            let tail = match engine.sound_tail(&audio, generation) {
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
        from.link(&head[0].static_pad("sink").ok_or("kuyruğun girişi yok")?).map_err(|e| format!("{e:?}"))?;
        source.sync_state_with_parent().map_err(|e| e.to_string())?;
        self.shared.info.lock().unwrap_or_else(|p| p.into_inner()).audio_codec = Some(codec);
        Ok(SoundParts { generation, pid: 0, codec: codec_of(caps), source, elements, pads })
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
    /// The decoded sound is measured where it goes into the volume
    /// (`sound_diag::Meter`): in the sink's format, before the viewer's
    /// volume and mute, which change what is heard and not what was decoded.
    fn sound_tail(&self, audio: &Audio, generation: u64) -> Result<Vec<gst::Element>, String> {
        let volume = make("volume")?;
        volume.set_property("volume", audio.gain);
        volume.set_property("mute", audio.muted);
        let shared = Arc::clone(&self.shared);
        let input = volume.static_pad("sink").ok_or("ses düzeyinin girişi yok")?;
        probe_layer(&input, crate::clock_probe::Layer::Convert, Some(generation), &self.shared);
        let told = AtomicBool::new(false);
        input.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let Some(buffer) = info.buffer() else { return gst::PadProbeReturn::Ok };
            let caps = pad.current_caps();
            let format = caps.as_ref().and_then(|caps| caps.structure(0)).map_or(Err("no caps"), |s| {
                crate::sound_diag::PcmFormat::from_caps(
                    s.name().as_str(),
                    s.get::<&str>("format").ok(),
                    s.get::<i32>("rate").ok(),
                    s.get::<i32>("channels").ok(),
                    s.get::<&str>("layout").ok(),
                )
            });
            if !told.swap(true, Ordering::Relaxed) {
                mediabox_core::timeline!(
                    "sound-measured-at",
                    "gen={generation} pad=volume.sink caps={}",
                    caps.as_ref().map_or_else(String::new, |c| c.to_string())
                );
            }
            let Ok(map) = buffer.map_readable() else { return gst::PadProbeReturn::Ok };
            shared.health.pcm(
                generation,
                format,
                buffer.pts().map(|t| t.nseconds()),
                buffer.duration().map(|d| d.nseconds()),
                buffer.flags().contains(gst::BufferFlags::DISCONT),
                map.as_slice(),
            );
            gst::PadProbeReturn::Ok
        });
        Ok(vec![make("audioconvert")?, make("audioresample")?, volume])
    }

    /// C: the decoder's units in, its PCM out (buffers, sample frames from
    /// the bytes and the caps), and every new format it puts out. With the
    /// `pcm-zero` fault, `MS` of its output made zero every ten seconds.
    fn watch_decoder(&self, decoder: &gst::Element, generation: u64) {
        let shared = Arc::clone(&self.shared);
        if let Some(input) = decoder.static_pad("sink") {
            input.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                shared.health.diagnose(|d| d.c.dec_in += 1);
                gst::PadProbeReturn::Ok
            });
        }
        let Some(output) = decoder.static_pad("src") else { return };
        probe_layer(&output, crate::clock_probe::Layer::Decoder, Some(generation), &self.shared);
        let shared = Arc::clone(&self.shared);
        let faults = crate::health::Faults::get(&shared.faults);
        let zeroing = Mutex::new((Instant::now(), None::<Instant>));
        output.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM, move |pad, info| {
            if let Some(gst::PadProbeData::Event(event)) = &info.data {
                if let gst::EventView::Caps(caps) = event.view() {
                    mediabox_core::timeline!("decoder-caps", "gen={generation} caps={}", caps.caps());
                    shared.health.diagnose(|d| d.c.dec_reconfigured += 1);
                }
                return gst::PadProbeReturn::Ok;
            }
            if crate::health::Faults::ON
                && faults.pcm_zero_ms > 0
                && let Some(buffer) = info.buffer_mut()
            {
                let mut z = zeroing.lock().unwrap_or_else(|p| p.into_inner());
                if z.1.is_none() && z.0.elapsed() >= std::time::Duration::from_secs(10) {
                    *z = (Instant::now(), Some(Instant::now()));
                    mediabox_core::timeline!("fault", "pcm-zero ms={}", faults.pcm_zero_ms);
                }
                if let Some(since) = z.1 {
                    if since.elapsed() < std::time::Duration::from_millis(faults.pcm_zero_ms) {
                        if let Ok(mut map) = buffer.make_mut().map_writable() {
                            map.as_mut_slice().fill(0);
                        }
                    } else {
                        z.1 = None;
                    }
                }
            }
            let Some(buffer) = info.buffer() else { return gst::PadProbeReturn::Ok };
            let frame_bytes = pad.current_caps().and_then(|caps| {
                let s = caps.structure(0)?;
                let bytes = match s.get::<&str>("format").ok()? {
                    "S16LE" => 2,
                    "S32LE" | "F32LE" => 4,
                    _ => return None,
                };
                Some(bytes * s.get::<i32>("channels").ok()? as usize)
            });
            let frames = frame_bytes.map_or(0, |b| (buffer.size() / b) as u64);
            shared.health.diagnose(|d| {
                d.c.dec_out_buffers += 1;
                d.c.dec_out_frames += frames;
            });
            gst::PadProbeReturn::Ok
        });
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
        let sound = {
            let mut sound = self.shared.sound.lock().unwrap_or_else(|p| p.into_inner());
            if sound.as_ref().is_some_and(|s| s.generation == branch.generation) { sound.take() } else { None }
        };
        let video = branch.programme.video.lock().unwrap_or_else(|p| p.into_inner()).take();
        let mut lists = vec![(branch.elements, branch.pads)];
        lists.extend(sound.map(|s| (s.elements, s.pads)));
        lists.extend(video.map(|v| (v.elements, v.pads)));
        self.take_down_lists(format!("teardown-{}", branch.generation), lists);
    }

    /// Off the selectors at once: what the old channel (or the old sound)
    /// still has decoded is not played after it was left. The panel keeps
    /// the last frame and the sound device plays silence until the new
    /// one's first. To NULL and out of the pipeline on a thread of its own,
    /// with whatever was added to the lists meanwhile.
    #[allow(clippy::type_complexity)]
    fn take_down_lists(&self, name: String, lists: Vec<(Arc<Mutex<Vec<gst::Element>>>, Arc<Mutex<Vec<(gst::Element, gst::Pad)>>>)>) {
        for (_, pads) in &lists {
            for (_, pad) in pads.lock().unwrap_or_else(|p| p.into_inner()).iter() {
                if let Some(peer) = pad.peer() {
                    let _ = peer.unlink(pad);
                }
            }
        }
        let pipeline = self.pipeline.clone();
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new().name(name.clone()).spawn(move || {
            for (elements, pads) in lists {
                let elements = std::mem::take(&mut *elements.lock().unwrap_or_else(|p| p.into_inner()));
                for element in &elements {
                    if let Err(error) = element.set_state(gst::State::Null) {
                        shared.health.teardown_failed(&format!("{}: NULL: {error}", element.name()));
                    }
                }
                for element in &elements {
                    if let Err(error) = pipeline.remove(element) {
                        shared.health.teardown_failed(&format!("{}: çıkarılamadı: {error}", element.name()));
                    }
                }
                for (selector, pad) in pads.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
                    selector.release_request_pad(&pad);
                }
            }
        });
        if let Err(error) = spawned {
            self.shared.health.teardown_failed(&format!("{name}: iş parçacığı açılamadı: {error}"));
        }
    }

    /// Whether `object` is part of the channel being played: an error from a
    /// branch being taken down is that branch's business, not the viewer's.
    pub fn is_current(&self, object: &gst::Object) -> bool {
        self.current_elements().iter().any(|element| object == element.upcast_ref::<gst::Object>() || object.has_as_ancestor(element))
    }

    /// The elements of the channel being played, its sound's part with
    /// them.
    fn current_elements(&self) -> Vec<gst::Element> {
        let current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        let Some(branch) = current.as_ref() else { return Vec::new() };
        let mut elements = branch.elements.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(video) = branch.programme.video.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            elements.extend(video.elements.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned());
        }
        if let Some(sound) = self.shared.sound.lock().unwrap_or_else(|p| p.into_inner()).as_ref().filter(|s| s.generation == branch.generation) {
            elements.extend(sound.elements.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned());
        }
        elements
    }

    pub fn current_path(&self) -> Option<String> {
        let info = self.shared.info.lock().unwrap_or_else(|p| p.into_inner());
        (!info.path.is_empty()).then(|| info.path.clone())
    }

    /// `aid`: an audio stream by its order, or none.
    ///
    /// From one stream to another while a channel plays, only the sound
    /// changes hands: the reader takes it to the stream chosen
    /// (`es::Reader::choose`) and the sound's part of the branch is built
    /// again for it (`sound_changed`) -- the picture's part, its decoder and
    /// the generation stay. It used to open the channel again from its FIFO:
    /// a new generation, the picture started again from a key frame, a
    /// second and more without one at every change (TRT WORLD, MP2 -> AC-3,
    /// 2026-11-02). From none, or with nothing playing, the channel is
    /// opened again as before.
    pub fn set_aid(self: &Arc<Self>, aid: Option<usize>) {
        let before = std::mem::replace(&mut *self.aid.lock().unwrap_or_else(|p| p.into_inner()), aid);
        if before == aid {
            return;
        }
        if let (Some(_), Some(order)) = (before, aid) {
            let generation = self.shared.generation.load(Ordering::SeqCst);
            let playing = self
                .shared
                .sound
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .is_some_and(|sound| sound.generation == generation);
            if playing {
                *self.shared.aid_request.lock().unwrap_or_else(|p| p.into_inner()) = Some((generation, order));
                mediabox_core::timeline!("aid-request", "gen={generation} order={order}");
                return;
            }
        }
        match aid {
            None => self.detach_audio_sink(),
            Some(_) => {
                if self.audio_sink.lock().unwrap_or_else(|p| p.into_inner()).is_none()
                    && let Err(error) = self.attach_audio_sink("aid")
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
        self.shared.health.user(audio.gain, audio.muted);
        for element in self.current_elements() {
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
            sink.set_property("ts-offset", self.audio_offset());
        }
    }

    /// The sound sink's `ts-offset`: the viewer's sound delay.
    fn audio_offset(&self) -> i64 {
        let fault = crate::health::Faults::get(&self.shared.faults).audio_late_ms * 1_000_000;
        (*self.audio_delay.lock().unwrap_or_else(|p| p.into_inner()) * 1e9) as i64 + fault
    }

    /// Channel `generation` started `behind` ns behind the broadcast
    /// (`stamp::LiveEdge`), for the timeline only. The receiver hands a new
    /// channel nothing from before its present (`RapCache::tables`), so this
    /// is the measure that it does; nothing is shifted to make up for it --
    /// shifting the sinks (2026-11-01) played the sound ahead of a picture
    /// that then lost its first second.
    pub fn set_behind(&self, generation: u64, behind: u64) {
        if self.shared.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        mediabox_core::timeline!("behind", "gen={generation} ns={behind}");
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

    /// The pipeline's latency, as its elements report it now: its minimum.
    pub fn latency(&self) -> Option<f64> {
        self.latency_query().map(|(_, min, _)| min as f64 / 1e9)
    }

    /// The latency query of the whole pipeline: live, minimum and maximum
    /// (ns; `None` for no maximum), as `gst_pipeline_do_latency` asks it
    /// before it compares it with the latency set (`LATENCY_MS`).
    pub fn latency_query(&self) -> Option<(bool, u64, Option<u64>)> {
        let mut query = gst::query::Latency::new();
        self.pipeline.query(&mut query).then_some(())?;
        let (live, min, max) = query.result();
        Some((live, min.nseconds(), max.map(|max| max.nseconds())))
    }

    /// What each element says the latency is up to its output: the query on
    /// its first source pad, which every element answers with its own
    /// latency added to its upstream's. The step from one element to the
    /// next downstream is the element's own share. Sinks have no source pad
    /// and are left out; the pipeline's figure is theirs.
    pub fn latency_by_element(&self) -> Vec<(String, bool, u64, Option<u64>)> {
        let mut out = Vec::new();
        for element in self.pipeline.iterate_recurse().into_iter().flatten() {
            let Some(pad) = element.src_pads().into_iter().next() else { continue };
            let mut query = gst::query::Latency::new();
            if pad.query(&mut query) {
                let (live, min, max) = query.result();
                out.push((element.name().to_string(), live, min.nseconds(), max.map(|max| max.nseconds())));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The latency set on the pipeline (`LATENCY_MS`), ns.
    pub fn configured_latency(&self) -> Option<u64> {
        self.pipeline.latency().map(|latency| latency.nseconds())
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

/// The clock probe on `pad` (`clock_probe::Layer`), when it is on: each
/// buffer's PTS against the samples it holds -- from the caps for PCM, from
/// its duration for coded frames. `generation`: the branch's, or the
/// current one for the sink's own pad.
fn probe_layer(pad: &gst::Pad, layer: crate::clock_probe::Layer, generation: Option<u64>, shared: &Arc<Shared>) {
    if crate::clock_probe::get().is_none() {
        return;
    }
    let shared = Arc::clone(shared);
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let (Some(probe), Some(buffer)) = (crate::clock_probe::get(), info.buffer()) else { return gst::PadProbeReturn::Ok };
        let pcm = pad.current_caps().and_then(|caps| {
            let s = caps.structure(0)?;
            if s.name() != "audio/x-raw" {
                return None;
            }
            let bytes = match s.get::<&str>("format").ok()? {
                "S16LE" => 2,
                "S32LE" | "F32LE" => 4,
                _ => return None,
            };
            let frame = bytes * s.get::<i32>("channels").ok()? as usize;
            Some(((buffer.size() / frame) as u64, s.get::<i32>("rate").ok()? as u32))
        });
        let generation = generation.unwrap_or_else(|| shared.generation.load(Ordering::Relaxed));
        probe.layer(
            layer,
            generation,
            buffer.pts().map(|t| t.nseconds()),
            buffer.duration().map(|d| d.nseconds()),
            pcm,
            buffer.flags().contains(gst::BufferFlags::DISCONT),
        );
        gst::PadProbeReturn::Ok
    });
}

/// One read of the FIFO, and the time (CLOCK_MONOTONIC, the pipeline's
/// clock, ns) its `read` returned at.
fn read_chunk(file: &mut File, path: &str, chunk: &mut [u8]) -> Result<(usize, u64), String> {
    loop {
        let before = crate::clock_probe::mono();
        let read = match file.read(chunk) {
            Ok(0) => return Err(format!("yayın akışı bitti ({path})")),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("{path} okunamadı: {error}")),
        };
        let at = crate::clock_probe::mono();
        if let Some(probe) = crate::clock_probe::get() {
            probe.read(read, at - before, at, crate::clock_probe::raw());
        }
        return Ok((read, at));
    }
}

/// Read the receiver's FIFO into the current branch's source, as fast as it
/// comes: every buffer is stamped with the time it arrived. The end of the
/// FIFO is the stream's end only while it is the FIFO being read: the
/// receiver lets the reader of a FIFO it has moved away from go that way.
fn read_stream(
    path: &str,
    into: &Mutex<Option<gst_app::AppSrc>>,
    stop: &AtomicBool,
    shared: &Shared,
    on_behind: &dyn Fn(u64, u64),
    on_sound: &dyn Fn(u64, &crate::es::Switch) -> Option<gst_app::AppSrc>,
) {
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
    // How far this channel's start lies behind the broadcast (`LiveEdge`).
    let mut edge = LiveEdge::default();
    // The sound, packet by packet (`es`): this channel's reader of its
    // tables and sound PID, and the last PCR with the running time it was
    // stamped with, to put PTS on that clock.
    let mut reader: Option<(u64, crate::es::Reader)> = None;
    let faults = crate::health::Faults::get(&shared.faults);
    let mut ts_dropped = 0u64;
    let mut reference: Option<(u64, u64)> = None;
    // The clock stepped (`Stamped::stepped`): the sound's next buffer is a
    // DISCONT.
    let mut sound_discont = false;
    // What each branch was given, on the timeline (`ts_trace`): told by
    // what is in the packets, so that a stream before the branch's reaching
    // it is seen.
    let mut content = mediabox_core::ts_trace::Content::default();
    loop {
        let (read, read_at) = match read_chunk(&mut file, path, &mut chunk) {
            Ok(read) => read,
            Err(why) => return ended(why),
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
            edge = LiveEdge::default();
            reader = None;
            reference = None;
            content.reset();
        }
        // When the read returned, on the source's running time: taken at
        // the read, never after a push that waited.
        let arrived = source.base_time().map(|base| gst::ClockTime::from_nseconds(read_at.saturating_sub(base.nseconds())));
        let pieces = packetizer.cut(&chunk[..read]);
        for piece in pieces {
            content.trace("r", format_args!("gen={generation}"), &piece.data);
            if let Some(arrived) = arrived
                && let Some(behind) = edge.observe(arrived.nseconds(), piece.pcr)
            {
                on_behind(generation, behind);
            }
            let sound = shared
                .sound
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .filter(|s| s.generation == generation)
                .map(|s| (s.pid, s.codec, s.source.clone()));
            // Every packet goes through the channel's reader, the tables and
            // the clock's as much as the sound's; a new channel, a new one.
            if reader.as_ref().is_none_or(|(g, _)| *g != generation) {
                reader = Some((generation, crate::es::Reader::new()));
            }
            let (_, es) = reader.as_mut().expect("just made");
            let mut out = crate::es::Output::default();
            // The viewer chose another audio stream: the sound goes to it
            // from its next PES, the picture as it is.
            let chosen = {
                let mut request = shared.aid_request.lock().unwrap_or_else(|p| p.into_inner());
                match *request {
                    Some((asked_in, order)) if asked_in == generation => request.take().map(|_| order),
                    Some((asked_in, _)) if asked_in < generation => request.take().and(None),
                    _ => None,
                }
            };
            if let Some(order) = chosen {
                match es.choose(order, &mut out) {
                    Some(switch) => {
                        eprintln!("mediabox-live: ses izi {} seçildi: PID {:#06x} {}", order + 1, switch.pid, switch.codec.name());
                        shared.info.lock().unwrap_or_else(|p| p.into_inner()).aid = Some(order);
                        mediabox_core::timeline!("aid-switch", "gen={generation} order={order} pid={} codec={}", switch.pid, switch.codec.name());
                    }
                    None => eprintln!("mediabox-live: ses izi {} yok ya da zaten çalıyor", order + 1),
                }
            }
            // The sound to start with is the demultiplexer pad's; from then
            // on the reader follows the PMT (`es::Switch`).
            if let Some((pid, codec, _)) = &sound
                && es.sound_pid().is_none()
            {
                es.select(*pid, *codec, &mut out);
            }
            // The `ts-drop` fault: every Nth packet of the sound's PID
            // never reaches the reader, as if the receiver had lost it.
            let fed = match (faults.ts_drop, es.sound_pid()) {
                (n, Some(pid)) if crate::health::Faults::ON && n > 0 => {
                    let mut kept = Vec::with_capacity(piece.data.len());
                    for packet in piece.data.chunks(crate::stamp::PACKET) {
                        let on_sound = packet.len() == crate::stamp::PACKET && ((u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2])) == pid;
                        if on_sound {
                            ts_dropped += 1;
                            if ts_dropped.is_multiple_of(n) {
                                mediabox_core::timeline!("fault", "ts-drop pid={pid} n={ts_dropped}");
                                continue;
                            }
                        }
                        kept.extend_from_slice(packet);
                    }
                    es.feed(&kept)
                }
                _ => es.feed(&piece.data),
            };
            out.chunks.extend(fed.chunks);
            out.events.extend(fed.events);
            shared.health.ts(generation, &out.events, es.counts(), es.sound_state(), es.codec());
            let chunks = out.chunks;
            let pcr = piece.pcr;
            let (at, stepped) = push_piece(&source, &mut stamp, generation, &mut pushed, piece, arrived, &shared.health);
            if let Some(at) = at
                && let Some(pcr) = pcr
            {
                reference = Some((at, pcr));
            }
            sound_discont |= stepped;
            if let (Some((_, _, source)), Some(arrived)) = (&sound, arrived)
                && !chunks.is_empty()
            {
                push_sound(source, chunks, (reference, std::mem::take(&mut sound_discont)), generation, arrived.nseconds(), shared, on_sound);
            }
        }
    }
}

/// One piece of the stream into the source, timed by `stamp` from its
/// arrival.
fn push_piece(
    source: &gst_app::AppSrc,
    stamp: &mut Stamp,
    generation: u64,
    pushed: &mut u64,
    piece: Piece,
    arrived: Option<gst::ClockTime>,
    health: &crate::health::Health,
) -> (Option<u64>, bool) {
    let mut stamped = None;
    let mut step_taken = false;
    {
            let bytes = piece.data.len();
            let pcr = piece.pcr;
            // The broadcaster's own word for a new time base (2.4.3.5), on
            // the timeline beside the jump `Stamp` reckons.
            let indicated = piece.discontinuity;
            let mut buffer = gst::Buffer::from_mut_slice(piece.data);
            if let Some(arrived) = arrived {
                let Stamped { at, class, discontinuity, stepped } = stamp.time(generation, arrived.nseconds(), pcr, indicated);
                if let Some(step) = stepped {
                    // A new phase for tsdemux too: on a DISCONT it pushes out
                    // what it holds and starts its PCR observations again
                    // (mpegtsbase), rather than taking the step over its 2 s
                    // window and 125 PCRs of averaging.
                    buffer.get_mut().expect("a new buffer is writable").set_flags(gst::BufferFlags::DISCONT);
                    step_taken = true;
                    eprintln!("mediabox-live: yayın {:.0} ms geç gelmeye başladı; saat o kadar ileri alındı", step as f64 / 1e6);
                    mediabox_core::timeline!("clock-step", "gen={generation} step_ns={step} at={at}");
                    health.clock_stepped();
                }
                health.chunk(generation, arrived.nseconds(), at);
                if let Some(probe) = crate::clock_probe::get() {
                    probe.chunk(arrived.nseconds(), at);
                    if pcr.is_some()
                        && let Some(trace) = stamp.trace.take()
                    {
                        probe.stamp(trace);
                    }
                }
                stamped = Some(at);
                *pushed += 1;
                // A PCR jump is said on the timeline: the times of this
                // opening are then no measurement (tools/live-trace.py marks
                // it invalid).
                mediabox_core::timeline!(
                    "src",
                    "gen={generation} n={pushed} bytes={bytes} arr={} pts={at} pcr0={} class={} disc={} ind={}",
                    arrived.nseconds(),
                    pcr.map_or(-1, |v| v as i64),
                    if class == Class::Paced { "paced" } else { "live" },
                    u8::from(discontinuity),
                    u8::from(indicated)
                );
                let at = gst::ClockTime::from_nseconds(at);
                let buffer = buffer.get_mut().expect("a new buffer is writable");
                buffer.set_pts(at);
                buffer.set_dts(at);
            }
            // A source taken down under the push refuses it; the reader goes
            // on for the branch after it. Counted, not dropped unsaid.
            let held = crate::clock_probe::get().map(|_| crate::clock_probe::mono());
            if let Err(error) = source.push_buffer(buffer) {
                health.push_failed(generation, "ts", error);
            }
            if let (Some(probe), Some(from)) = (crate::clock_probe::get(), held) {
                probe.push(crate::clock_probe::mono() - from);
            }
    }
    (stamped, step_taken)
}

/// The samples of decoded `buffer` before `from` (running time, ns) cut
/// off its front, its PTS and duration moved with them: how many frames
/// were cut. None: the caps are not interleaved PCM this knows, or the
/// buffer has no time.
pub fn trim(buffer: &mut gst::Buffer, from: u64, caps: &gst::CapsRef) -> Option<u64> {
    let s = caps.structure(0)?;
    let rate = u64::try_from(s.get::<i32>("rate").ok()?).ok()?;
    let channels = u64::try_from(s.get::<i32>("channels").ok()?).ok()?;
    let format = s.get::<&str>("format").ok()?;
    if s.get::<&str>("layout").is_ok_and(|layout| layout != "interleaved") || rate == 0 {
        return None;
    }
    let width = match format {
        "S8" | "U8" => 1,
        "S16LE" | "S16BE" | "U16LE" | "U16BE" => 2,
        "S24LE" | "S24BE" | "U24LE" | "U24BE" => 3,
        "S32LE" | "S32BE" | "U32LE" | "U32BE" | "F32LE" | "F32BE" | "S24_32LE" | "S24_32BE" => 4,
        "F64LE" | "F64BE" => 8,
        _ => return None,
    };
    let pts = buffer.pts()?.nseconds();
    if pts >= from {
        return Some(0);
    }
    let bpf = (width * channels) as usize;
    let frames = buffer.size() / bpf;
    // The first sample at or after `from`.
    let cut = ((u128::from(from - pts) * u128::from(rate)).div_ceil(1_000_000_000) as usize).min(frames);
    let at = pts + (cut as u64 * 1_000_000_000) / rate;
    let buffer = buffer.make_mut();
    buffer.resize(cut * bpf..).ok()?;
    buffer.set_pts(gst::ClockTime::from_nseconds(at));
    buffer.set_duration(gst::ClockTime::from_nseconds(((frames - cut) as u64 * 1_000_000_000) / rate));
    Some(cut as u64)
}

/// A PTS (90 kHz, 33 bits) on the running time, by the last PCR (27 MHz)
/// and the running time its piece was stamped with: what tsdemux does with
/// the same stamps, without waiting for the PES to end.
pub fn pts_on_clock(pts: u64, reference: (u64, u64)) -> u64 {
    let (at, pcr) = reference;
    const WRAP: i64 = 1 << 33;
    let mut ticks = (pts as i64 - (pcr / 300) as i64).rem_euclid(WRAP);
    if ticks > WRAP / 2 {
        ticks -= WRAP;
    }
    let pcr_rest = (pcr % 300) as i64; // 27 MHz below the 90 kHz base
    let ns = ticks * 1_000_000_000 / 90_000 - pcr_rest * 1000 / 27;
    (at as i64 + ns).max(0) as u64
}

/// The sound's payload into its own source as it comes (`es`): a PES's
/// start with its PTS on the running time, the rest untimed for the parser
/// to time by its frames.
fn push_sound(
    source: &gst_app::AppSrc,
    chunks: Vec<crate::es::Chunk>,
    (reference, discont): (Option<(u64, u64)>, bool),
    generation: u64,
    arrived: u64,
    shared: &Shared,
    on_sound: &dyn Fn(u64, &crate::es::Switch) -> Option<gst_app::AppSrc>,
) {
    let mut target = Some(source.clone());
    if mediabox_core::timeline::enabled()
        && let Some(reference) = reference
    {
        for chunk in chunks.iter().filter(|c| c.start) {
            if let Some(pts) = chunk.pts {
                mediabox_core::timeline!("a-pes", "gen={generation} raw={pts} pts={}", pts_on_clock(pts, reference));
            }
        }
    }
    let bytes: usize = chunks.iter().map(|c| c.data.len()).sum();
    shared.health.diagnose(|d| d.c.ts_payload_bytes += bytes as u64);
    let failed = route_sound(&mut target, chunks, reference, discont, &mut |switch| on_sound(generation, switch), &mut |at| {
        shared.health.diagnose(|d| d.c.ts_pes_starts += 1);
        shared.health.demuxed(generation, true, arrived as i64 - at as i64);
        let mut starts = shared.sound_starts.lock().unwrap_or_else(|p| p.into_inner());
        starts.push_back(at);
        while starts.len() > 64 {
            starts.pop_front();
        }
    });
    for error in failed {
        shared.health.push_failed(generation, "sound", error);
    }
}

/// The reader's chunks into the sound's source, `target`. A chunk that
/// starts a sound the PMT moved the reader to (`es::Switch`) is the first
/// the new source gets: nothing of the old sound goes after it, and
/// `on_switch` gives the source for the new one -- the same for the same
/// codec, a new branch for another. Any break (`es::Break`) is a DISCONT:
/// the parser starts again there. `on_start` is told each PES start's
/// running time. What the source refused is given back: never dropped
/// without a word.
pub fn route_sound(
    target: &mut Option<gst_app::AppSrc>,
    chunks: Vec<crate::es::Chunk>,
    reference: Option<(u64, u64)>,
    discont: bool,
    on_switch: &mut dyn FnMut(&crate::es::Switch) -> Option<gst_app::AppSrc>,
    on_start: &mut dyn FnMut(u64),
) -> Vec<gst::FlowError> {
    let mut failed = Vec::new();
    let mut lost = discont;
    for chunk in chunks {
        if let Some(switch) = &chunk.switch {
            *target = on_switch(switch);
        }
        if chunk.data.is_empty() {
            continue;
        }
        let Some(source) = target.as_ref() else {
            lost = true;
            continue;
        };
        // Before the first PCR there is no clock to put a PTS on.
        let Some(reference) = reference else {
            lost = true;
            continue;
        };
        if chunk.new_time_base {
            mediabox_core::timeline!("sound-time-base", "pts={:?}", chunk.pts);
        }
        let mut buffer = gst::Buffer::from_mut_slice(chunk.data);
        {
            let buffer = buffer.get_mut().expect("a new buffer is writable");
            if let Some(pts) = chunk.pts {
                let at = pts_on_clock(pts, reference);
                if let Some(probe) = crate::clock_probe::get() {
                    probe.pes(pts, at);
                }
                buffer.set_pts(gst::ClockTime::from_nseconds(at));
                on_start(at);
            }
            if chunk.discont.is_some() || std::mem::take(&mut lost) {
                buffer.set_flags(gst::BufferFlags::DISCONT);
            }
        }
        if let Err(error) = source.push_buffer(buffer) {
            failed.push(error);
            lost = true;
        }
    }
    failed
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

    /// Pictures the decoder lost for good (four at once), the grid on the
    /// stream's own time before them: the next picture is shown at its own
    /// time, once -- not held 80 ms before it for two seconds.
    #[test]
    fn a_loss_of_whole_pictures_is_shown_at_its_own_time() {
        let mut source: Vec<u64> = (0..50u64).map(|n| n * 20 * MS).collect();
        source.extend((54..400u64).map(|n| n * 20 * MS));
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        assert_eq!(scheduled, source, "every picture at its own time");
    }

    /// KANAL D HD, generation 3 of 2026-11-02 (a field-coded 1080i H.264
    /// opening): MPP's first pictures, by their PTS. No picture for the two
    /// fields after the key frame. The first picture is held for them, once;
    /// from the second on every picture is at its own time, 20 ms apart --
    /// not two and four 40 ms steps.
    #[test]
    fn the_deinterlacers_first_gap_is_one_hold_not_a_run_of_40_ms_steps() {
        let mut source: Vec<u64> = [90_348_272u64, 90_408_272, 90_428_272, 90_448_272, 90_468_272, 90_488_272].iter().map(|us| us * 1_000).collect();
        let mut t = *source.last().unwrap();
        for _ in 0..100 {
            t += 20 * MS;
            source.push(t);
        }
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        assert_eq!(scheduled, source);
        assert_eq!(steps(&scheduled)[0], 60 * MS);
        assert!(steps(&scheduled)[1..].iter().all(|&s| s == 20 * MS));
    }

    /// TV8 HD (1080i H.264) opening from its recording, 2026-11-03: MPP's
    /// pictures each say 40 ms, one a field, and the first gap after the
    /// key frame is three fields. Every picture at its own time from the
    /// first: 60, then 20 ms -- not 40, 40, 20.
    #[test]
    fn a_field_rate_opening_with_its_first_gap_is_timed_from_the_first_step() {
        let mut source = vec![0u64, 60 * MS];
        source.extend((4..100u64).map(|n| n * 20 * MS));
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 40 * MS);
        assert_eq!(scheduled, source);
    }

    /// TRT WORLD HD, generation 68 of 2026-11-02: a TS packet lost on the
    /// picture's PID (tsdemux: CONTINUITY on 0x08ff), and MPP gave nothing
    /// from the key frame at 912.876903 s to the next I picture at
    /// 913.896903 s. The pictures after the gap at their own time: none of
    /// them put a second early, which the sink threw away as late (99).
    #[test]
    fn a_lost_gop_is_taken_at_once_and_loses_nothing_after_it() {
        let mut source = vec![912_876_903_000u64];
        let mut t = 913_896_903_000u64;
        for _ in 0..200 {
            source.push(t);
            t += 20 * MS;
        }
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        assert_eq!(scheduled, source, "no picture moved off its own time");
    }

    /// Generation 69 (the same channel, after an audio track change): the
    /// gap came nine seconds in, 929.814 -> 930.914 s. The same: taken at
    /// once, nothing left out or moved after it.
    #[test]
    fn a_gap_well_into_a_channel_is_taken_at_once() {
        let mut source: Vec<u64> = (0..450u64).map(|n| 920_814_424_000 + n * 20 * MS).collect();
        let resume = source.last().unwrap() + 1_100 * MS;
        source.extend((0..200u64).map(|n| resume + n * 20 * MS));
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        assert_eq!(scheduled, source);
    }

    /// A jump forward from a grid already off the stream's time (no whole
    /// periods to count by): to the nearest slot at once, never two seconds
    /// of pictures put before their time.
    #[test]
    fn a_jump_forward_off_the_grid_reanchors_at_once() {
        let mut source: Vec<u64> = [0u64, 20, 40, 57, 75, 95].iter().map(|ms| ms * MS).collect();
        source.extend((0..100u64).map(|n| 1_095 * MS + n * 20 * MS));
        let mut timeline = PresentationTimeline::default();
        let scheduled = run(&mut timeline, &source, 20 * MS);
        let after = &scheduled[6..];
        assert!(after.iter().zip(&source[6..]).all(|(s, src)| s.abs_diff(*src) <= 10 * MS), "on its own time within half a period");
        assert!(scheduled.windows(2).all(|w| w[1] > w[0]));
    }

    /// Jitter is not a loss: steps that are not whole periods, or a grid not
    /// on the stream's time, are left to the grid as before.
    #[test]
    fn whole_periods_are_counted_only_within_a_millisecond() {
        assert_eq!(PresentationTimeline::whole_periods(60 * MS, 20 * MS), 3);
        assert_eq!(PresentationTimeline::whole_periods(60 * MS + 900_000, 20 * MS), 3);
        assert_eq!(PresentationTimeline::whole_periods(37 * MS, 20 * MS), 0);
        assert_eq!(PresentationTimeline::whole_periods(1_020 * MS, 20 * MS), 51);
        assert_eq!(PresentationTimeline::whole_periods(0, 20 * MS), 0);
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

/// audiobasesink's own word on the sound it repaired (`Health::sink_said`):
/// its render and alignment debug lines, and only those, are let through to
/// this one log function. Nothing goes to stderr that did not before: the
/// default log function stays as it was for GST_DEBUG's categories, and the
/// sink's category is raised only when GST_DEBUG says nothing of it.
fn listen_to_audio_sink(shared: &Arc<Shared>) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let shared = Arc::downgrade(shared);
    ONCE.call_once(move || {
        let from_env = std::env::var("GST_DEBUG").is_ok_and(|v| v.contains("audiobasesink"));
        if !from_env {
            if std::env::var_os("GST_DEBUG").is_none() {
                gst::log::remove_default_log_function();
            }
            gst::log::set_threshold_for_name("audiobasesink", gst::DebugLevel::Debug);
        }
        gst::log::add_log_function(move |category, level, _file, function, _line, _object, message| {
            if level > gst::DebugLevel::Debug || category.name() != "audiobasesink" {
                return;
            }
            let function = function.as_str();
            if function != "gst_audio_base_sink_render" && function != "gst_audio_base_sink_get_alignment" {
                return;
            }
            let Some(text) = message.get() else { return };
            let text = text.as_str();
            if !(text.starts_with("dropping late sample") || text.starts_with("Unexpected discontinuity")) {
                return;
            }
            if let Some(shared) = shared.upgrade() {
                shared.health.sink_said(text);
            }
        });
    });
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod sound_spec;

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_spec;
