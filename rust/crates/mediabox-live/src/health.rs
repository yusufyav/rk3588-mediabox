//! The live monitor, the player's side: what only the player can see of a
//! channel's health, written for the interface (`mediabox-tv`, key M on live
//! television) to put beside what only it can see -- the panel.
//!
//! - The broadcast's clock: every chunk of the stream is stamped with the
//!   broadcaster's time (`stamp`); how early the earliest chunk of the last
//!   two seconds came against its stamp is where the broadcast's present is
//!   on the pipeline's clock (`edge_ns`). A picture is live when it is shown
//!   at its PTS on that clock (ISO/IEC 13818-1 2.4.2, STC = PTS).
//! - The sound as it reaches the sound sink: every buffer is to begin where
//!   the one before ended. A gap is sound missing, an overlap sound doubled.
//! - The sound device, from the kernel (`/proc/asound/.../status`): how much
//!   sound was queued at the least (`delay`), and every time the device was
//!   started again (`trigger_time` changed): an underrun (XRUN) recovered by
//!   alsasink, or the device opened again -- either one heard as a break.
//! - The sound clock: the sink plays by the device's clock, slaved to the
//!   pipeline's (`slave-method` skew). The pipeline's clock now less the
//!   sound clock now is how late the sound plays against its schedule.
//!
//! Written to `PATH` every `PERIOD`; cumulative counters, the reader takes
//! differences.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

pub const PATH: &str = "/run/mediabox-live/health.json";
pub const PERIOD: std::time::Duration = std::time::Duration::from_millis(250);
/// How far back the broadcast's present is looked for.
const EDGE_WINDOW_NS: u64 = 2_000_000_000;
/// A sound buffer this far off where the one before ended is a break: the
/// stream's PTS are 90 kHz exact, the parsers' durations sample-exact.
const AUDIO_TOLERANCE_NS: i64 = 2_000_000;

#[derive(Default)]
pub struct Health {
    edge: Mutex<Edge>,
    audio: Mutex<AudioFlow>,
    /// The pipeline's latency the sinks wait by, this channel's.
    pub latency_ns: AtomicI64,
    /// Pictures the player did not send: late, out of order, not a dma-buf.
    pub video_dropped: AtomicU64,
    /// The sound sink's `ts-offset`, for how early sound reaches it.
    pub audio_offset_ns: AtomicI64,
    lead: Mutex<Lead>,
    /// audiobasesink saying it repaired the sound (`sink_said`), past the
    /// first second of a channel: samples dropped as late, timestamps that
    /// jumped (silence or sound lost between).
    pub sink_late: AtomicU64,
    pub sink_discont: AtomicU64,
    demux: Mutex<DemuxLate>,
    /// The pipeline's base time, ns, for the streaming threads.
    pub base_ns: AtomicU64,
    /// tsdemux's time for a sound PES less the reader's for it, ns: the
    /// worst this channel (`sound_map`).
    map: Mutex<(u64, i64)>,
    map_said: Mutex<u64>,
    ts: Mutex<Ts>,
    /// Buffers a source refused: the sound's (`route_sound`) and the
    /// stream's; and elements that would not go down. Each said once a
    /// channel on stderr, every one on the timeline.
    pub sound_push_errors: AtomicU64,
    pub ts_push_errors: AtomicU64,
    pub teardown_errors: AtomicU64,
    push_said: Mutex<(u64, u8)>,
    /// The sound turned down to nothing or muted by the viewer: silence
    /// then is not a fault (`pcm`).
    pub quiet_by_choice: std::sync::atomic::AtomicBool,
    silence: Mutex<Silence>,
    /// Pictures the plane gave back because the interface still held every
    /// buffer (`plane::Plane::dropped_full`), set by the reporter.
    pub plane_full_dropped: AtomicU64,
}

/// Sound that decoded to silence: the broadcast's own, or a stream that
/// carried nothing, as against sound lost on its way to the device. A run
/// of `SILENCE_NS` or more is counted, with how long it was.
#[derive(Default)]
struct Silence {
    generation: u64,
    /// The current silent run: where it began (PTS) and how long it is.
    start_pts: Option<u64>,
    run_ns: u64,
    told: bool,
    count: u64,
    total_ns: u64,
    longest_ns: u64,
}

/// Peak below this (-60 dBFS) is silence.
const SILENCE_PEAK: f32 = 0.001;
/// A silent run this long is counted.
const SILENCE_NS: u64 = 200_000_000;

/// The transport stream reader's counts (`es::Counts`): the channels before
/// this one summed, and this one's; each kind of event said once a channel.
#[derive(Default)]
struct Ts {
    generation: u64,
    before: crate::es::Counts,
    now: crate::es::Counts,
    state: Option<crate::es::SoundState>,
    /// The sound's codec as its PMT names it.
    codec: Option<crate::es::Codec>,
    told: u32,
}

/// One bit per kind of `es::Event`, for saying each once a channel.
fn kind(event: &crate::es::Event) -> u32 {
    use crate::es::Event::*;
    1 << match event {
        TransportError { .. } => 0,
        Scrambled { .. } => 1,
        ContinuityLoss { .. } => 2,
        DuplicateMismatch { .. } => 3,
        Duplicate { .. } => 4,
        ExcessiveDuplicate { .. } => 5,
        MalformedAdaptation { .. } => 6,
        MalformedPes { .. } => 7,
        ForeignPes { .. } => 8,
        InvalidTimestamp { .. } => 9,
        AnnouncedDiscontinuity { .. } => 10,
        TimeBaseDiscontinuity { .. } => 11,
        Recovered { .. } => 12,
        PsiError { .. } => 13,
        PmtVersion { .. } => 14,
        PcrPidChanged { .. } => 15,
        SoundReset { .. } => 16,
    }
}

/// How far behind its PTS each stream comes out of the demultiplexer, this
/// channel's worst: the least latency the sinks can wait by and still have
/// it before its time (a sound PES comes out only once the next one starts).
#[derive(Default)]
struct DemuxLate {
    generation: u64,
    audio: Option<i64>,
    video: Option<i64>,
}

/// How long before it plays a sound buffer reached the sink.
#[derive(Default)]
struct Lead {
    least: Option<i64>,
    /// Buffers that came after their time: the device played silence for
    /// them and the sink then dropped them.
    late: u64,
}

#[derive(Default)]
struct Edge {
    generation: u64,
    /// (arrived, arrived - stamp), ns on the running time.
    window: VecDeque<(u64, i64)>,
}

#[derive(Default)]
struct AudioFlow {
    generation: u64,
    /// When this channel's first sound came.
    since: Option<std::time::Instant>,
    end_ns: Option<u64>,
    buffers: u64,
    gaps: u64,
    gap_ns: u64,
    overlaps: u64,
    overlap_ns: u64,
}

impl Health {
    /// A sound buffer reached the sink `lead` ns before it is to play
    /// (its running time + latency + offset, less the running time now).
    pub fn audio_lead(&self, lead: i64) {
        let settled = self.settled();
        let mut l = self.lead.lock().unwrap_or_else(|p| p.into_inner());
        l.least = Some(l.least.map_or(lead, |x| x.min(lead)));
        if lead < 0 && settled {
            l.late += 1;
            mediabox_core::timeline!("audio-late", "lead_ms={:.1}", lead as f64 / 1e6);
            eprintln!("mediabox-live: ses çalınacağı andan {:.1} ms sonra geldi", -lead as f64 / 1e6);
        }
    }

    /// Past the first second of the channel's sound: what the sink does
    /// with a channel's first buffers (clipping to the segment, the first
    /// resync) is its start, not a break.
    fn settled(&self) -> bool {
        self.audio.lock().unwrap_or_else(|p| p.into_inner()).since.is_some_and(|at| at.elapsed() >= std::time::Duration::from_secs(1))
    }

    /// One of audiobasesink's own debug lines, from `gst::log` (only those
    /// of `gst_audio_base_sink_render` and `..._get_alignment` reach here).
    pub fn sink_said(&self, message: &str) {
        if !self.settled() {
            return;
        }
        if message.starts_with("dropping late sample") {
            self.sink_late.fetch_add(1, Ordering::Relaxed);
            mediabox_core::timeline!("audio-sink-late", "");
            eprintln!("mediabox-live: ses aygıtı geç örnek attı (duyulur kesik)");
        } else if message.starts_with("Unexpected discontinuity") {
            self.sink_discont.fetch_add(1, Ordering::Relaxed);
            mediabox_core::timeline!("audio-sink-discont", "{message}");
            eprintln!("mediabox-live: ses aygıtı: {message}");
        }
    }

    /// A buffer of channel `generation` left the demultiplexer `late` ns
    /// after its PTS on the running time.
    pub fn demuxed(&self, generation: u64, audio: bool, late: i64) {
        let mut d = self.demux.lock().unwrap_or_else(|p| p.into_inner());
        if d.generation != generation {
            *d = DemuxLate { generation, ..DemuxLate::default() };
        }
        let worst = if audio { &mut d.audio } else { &mut d.video };
        if worst.is_none_or(|w| late > w) {
            *worst = Some(late);
            mediabox_core::timeline!("demux-late", "gen={generation} kind={} ms={:.1}", if audio { "audio" } else { "video" }, late as f64 / 1e6);
        }
    }

    /// tsdemux put a sound PES `diff` ns from where the reader put it. The
    /// first two seconds of a channel are left out: tsdemux's first PES and
    /// the reader's first whole one are not the same PES. Said on the
    /// timeline once a second, the worst kept for the report.
    pub fn sound_map(&self, diff: i64) {
        let generation = self.demux.lock().unwrap_or_else(|p| p.into_inner()).generation;
        let since = self.audio.lock().unwrap_or_else(|p| p.into_inner()).since;
        if since.is_none_or(|at| at.elapsed() < std::time::Duration::from_secs(2)) {
            return;
        }
        let mut m = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if m.0 != generation {
            *m = (generation, 0);
        }
        if diff.abs() > m.1.abs() {
            m.1 = diff;
        }
        let now = mediabox_core::timeline::now_ns();
        let mut said = self.map_said.lock().unwrap_or_else(|p| p.into_inner());
        if now >= *said + 1_000_000_000 {
            *said = now;
            mediabox_core::timeline!("sound-map", "gen={generation} diff_us={}", diff / 1000);
        }
    }

    /// A source of channel `generation` refused a buffer (`what`: "sound" or
    /// "ts"). A source being taken down refuses with FLUSHING or EOS; that is
    /// counted too, the reason with it.
    pub fn push_failed(&self, generation: u64, what: &str, error: gst::FlowError) {
        let counter = if what == "sound" { &self.sound_push_errors } else { &self.ts_push_errors };
        counter.fetch_add(1, Ordering::Relaxed);
        mediabox_core::timeline!("push-failed", "gen={generation} what={what} error={error:?}");
        let bit = if what == "sound" { 1 } else { 2 };
        let mut said = self.push_said.lock().unwrap_or_else(|p| p.into_inner());
        if said.0 != generation {
            *said = (generation, 0);
        }
        if said.1 & bit == 0 {
            said.1 |= bit;
            eprintln!("mediabox-live: {what} kaynağı tamponu reddetti: {error:?} (kanalda ilk; sayısı health.json)");
        }
    }

    /// An element of a part being taken down would not go down.
    pub fn teardown_failed(&self, what: &str) {
        self.teardown_errors.fetch_add(1, Ordering::Relaxed);
        mediabox_core::timeline!("teardown-failed", "{what}");
        eprintln!("mediabox-live: söküm hatası: {what}");
    }

    /// What the transport stream reader saw in a piece of channel
    /// `generation`, and its counts so far. Every event goes on the
    /// timeline; the first of each kind in a channel, and the sound's state
    /// when it changes, on stderr -- no more, however broken the stream.
    pub fn ts(&self, generation: u64, events: &[crate::es::Event], counts: crate::es::Counts, state: Option<crate::es::SoundState>, codec: Option<crate::es::Codec>) {
        let mut ts = self.ts.lock().unwrap_or_else(|p| p.into_inner());
        if ts.generation != generation {
            ts.before = ts.before.plus(&ts.now);
            ts.now = crate::es::Counts::default();
            ts.generation = generation;
            ts.state = None;
            ts.told = 0;
        }
        ts.now = counts;
        ts.codec = codec;
        for event in events {
            mediabox_core::timeline!("ts", "gen={generation} {event:?}");
            let bit = kind(event);
            if ts.told & bit == 0 {
                ts.told |= bit;
                eprintln!("mediabox-live: yayın akışı: {event:?} (kanalda ilk; sayıları health.json \"ts\")");
            }
        }
        if state != ts.state {
            if let Some(state) = state.filter(|s| *s != crate::es::SoundState::Playing) {
                eprintln!("mediabox-live: ses verilmiyor: {}", state.name());
            }
            mediabox_core::timeline!("sound-state", "gen={generation} state={}", state.map_or("none", |s| s.name()));
            ts.state = state;
        }
    }

    /// A chunk of channel `generation` arrived at `arrived` with the stamp
    /// `at` (both running time, ns).
    pub fn chunk(&self, generation: u64, arrived: u64, at: u64) {
        let mut edge = self.edge.lock().unwrap_or_else(|p| p.into_inner());
        if edge.generation != generation {
            edge.generation = generation;
            edge.window.clear();
        }
        edge.window.push_back((arrived, arrived as i64 - at as i64));
        while edge.window.front().is_some_and(|&(t, _)| t + EDGE_WINDOW_NS < arrived) {
            edge.window.pop_front();
        }
    }

    fn edge_ns(&self) -> Option<i64> {
        let edge = self.edge.lock().unwrap_or_else(|p| p.into_inner());
        edge.window.iter().map(|&(_, d)| d).min()
    }

    /// The loudest sample of a decoded sound buffer (`peak`, 0..1) of
    /// channel `generation` on its way into the sink. Sound turned down or
    /// muted by the viewer is not counted.
    pub fn pcm(&self, generation: u64, pts: u64, duration: u64, peak: f32) {
        let mut s = self.silence.lock().unwrap_or_else(|p| p.into_inner());
        if s.generation != generation {
            s.generation = generation;
            s.start_pts = None;
            s.run_ns = 0;
            s.told = false;
        }
        if peak < SILENCE_PEAK && !self.quiet_by_choice.load(Ordering::Relaxed) {
            if s.start_pts.is_none() {
                s.start_pts = Some(pts);
            }
            s.run_ns += duration;
            if s.run_ns >= 1_000_000_000 && !s.told {
                s.told = true;
                mediabox_core::timeline!("audio-silent", "gen={generation} from_pts={} ms={:.0}", s.start_pts.unwrap_or(0), s.run_ns as f64 / 1e6);
                eprintln!("mediabox-live: çözülen ses 1 s'dir sessiz (pts {} itibarıyla)", s.start_pts.unwrap_or(0));
            }
            return;
        }
        if let Some(start) = s.start_pts.take() {
            let run = std::mem::take(&mut s.run_ns);
            s.told = false;
            if run >= SILENCE_NS {
                s.count += 1;
                s.total_ns += run;
                s.longest_ns = s.longest_ns.max(run);
                mediabox_core::timeline!("audio-silence", "gen={generation} pts={start} ms={:.0}", run as f64 / 1e6);
                eprintln!("mediabox-live: çözülen seste {:.0} ms sessizlik (pts {start}): yayının ya da akışın kendi sessizliği", run as f64 / 1e6);
            }
        }
    }

    /// A sound buffer of channel `generation` on its way into the sink.
    pub fn audio(&self, generation: u64, pts: u64, duration: Option<u64>) {
        let mut flow = self.audio.lock().unwrap_or_else(|p| p.into_inner());
        if flow.generation != generation {
            // Counters run on; where the next buffer is to begin does not.
            flow.generation = generation;
            flow.end_ns = None;
            flow.since = Some(std::time::Instant::now());
        }
        flow.buffers += 1;
        if let Some(end) = flow.end_ns {
            let off = pts as i64 - end as i64;
            if off > AUDIO_TOLERANCE_NS {
                flow.gaps += 1;
                flow.gap_ns += off as u64;
                mediabox_core::timeline!("audio-gap", "gen={generation} pts={pts} ms={:.1}", off as f64 / 1e6);
                eprintln!("mediabox-live: ses akışında {:.1} ms boşluk (pts {pts})", off as f64 / 1e6);
            } else if off < -AUDIO_TOLERANCE_NS {
                flow.overlaps += 1;
                flow.overlap_ns += off.unsigned_abs();
                mediabox_core::timeline!("audio-overlap", "gen={generation} pts={pts} ms={:.1}", -off as f64 / 1e6);
                eprintln!("mediabox-live: ses akışında {:.1} ms örtüşme (pts {pts})", -off as f64 / 1e6);
            }
        }
        flow.end_ns = duration.map(|d| pts + d);
    }
}

/// The sound device's status in the kernel.
#[derive(Default)]
pub struct Device {
    path: Option<std::path::PathBuf>,
    params: Option<std::path::PathBuf>,
    trigger: Option<String>,
    pub restarts: u64,
    /// Least sound queued in the device since the last report, frames.
    least: Option<u64>,
    rate: u64,
    buffer: u64,
    running: bool,
}

impl Device {
    /// For the ALSA name the sink opens ("hdmi:CARD=rockchiphdmi0,DEV=0",
    /// "hw:1,0").
    pub fn for_name(name: &str) -> Self {
        let (card, dev) = card_and_device(name);
        let Some(card) = card else { return Self::default() };
        let base = std::path::PathBuf::from(format!("/proc/asound/{card}/pcm{}p/sub0", dev.unwrap_or(0)));
        Self { path: Some(base.join("status")), params: Some(base.join("hw_params")), ..Self::default() }
    }

    /// Read the status once; often (every few ms), as a queue running low
    /// lasts a moment.
    pub fn sample(&mut self) {
        let Some(path) = &self.path else { return };
        let Ok(text) = std::fs::read_to_string(path) else {
            self.running = false;
            return;
        };
        let field = |name: &str| {
            text.lines().find_map(|line| line.strip_prefix(name).map(|rest| rest.trim_start_matches([' ', ':', '\t']).trim().to_string()))
        };
        self.running = field("state").is_some_and(|s| s == "RUNNING");
        if !self.running {
            return;
        }
        if let Some(trigger) = field("trigger_time") {
            if self.trigger.as_ref().is_some_and(|t| *t != trigger) {
                self.restarts += 1;
                mediabox_core::timeline!("audio-restart", "trigger={trigger}");
                eprintln!("mediabox-live: ses aygıtı yeniden başladı (XRUN ya da yeniden açılış), trigger_time {trigger}");
            }
            self.trigger = Some(trigger);
        }
        if let Some(delay) = field("delay").and_then(|d| d.parse::<u64>().ok()) {
            self.least = Some(self.least.map_or(delay, |l| l.min(delay)));
        }
        if self.rate == 0
            && let Some(params) = self.params.as_ref().and_then(|p| std::fs::read_to_string(p).ok())
        {
            let value = |name: &str| {
                params.lines().find_map(|l| l.strip_prefix(name)).and_then(|r| r.trim_start_matches([' ', ':']).split_whitespace().next()?.parse::<u64>().ok())
            };
            self.rate = value("rate").unwrap_or(0);
            self.buffer = value("buffer_size").unwrap_or(0);
        }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    fn take_least_ms(&mut self) -> Option<f64> {
        let least = self.least.take()?;
        (self.rate > 0).then(|| least as f64 * 1000.0 / self.rate as f64)
    }
}

fn card_and_device(name: &str) -> (Option<String>, Option<u32>) {
    let rest = name.split_once(':').map_or(name, |(_, rest)| rest);
    let mut card = None;
    let mut dev = None;
    for (i, part) in rest.split(',').enumerate() {
        match part.split_once('=') {
            Some(("CARD", v)) => card = Some(v.to_string()),
            Some(("DEV", v)) => dev = v.parse().ok(),
            Some(_) => {}
            None if i == 0 => card = Some(part.to_string()),
            None if i == 1 => dev = part.parse().ok(),
            None => {}
        }
    }
    // A card by number is /proc/asound/cardN.
    let card = card.map(|c| if c.chars().all(|ch| ch.is_ascii_digit()) { format!("card{c}") } else { c });
    (card, dev)
}

/// What is written for the interface.
pub struct Report {
    pub at_ns: u64,
    pub generation: u64,
    /// The broadcast's present on the running time is the stamp plus this.
    pub edge_ns: Option<i64>,
    /// A picture of PTS p is due at base + p + this (latency + offset).
    pub video_due_ns: i64,
    /// A sound sample of PTS p plays at base + p + this + audio_late_ns.
    pub audio_due_ns: i64,
    /// Pipeline clock now less the sound clock now: how late the sound
    /// device plays against its schedule. None: no sound playing.
    pub audio_late_ns: Option<i64>,
    pub audio_buffers: u64,
    pub audio_gaps: u64,
    pub audio_gap_ms: f64,
    pub audio_overlaps: u64,
    pub audio_overlap_ms: f64,
    pub audio_restarts: u64,
    /// Least sound queued in the device since the report before, ms.
    pub audio_queue_least_ms: Option<f64>,
    pub audio_queue_ms: Option<f64>,
    pub video_dropped: u64,
    pub audio_lead_least_ms: Option<f64>,
    pub audio_late: u64,
    pub sink_late: u64,
    pub sink_discont: u64,
    pub audio_demux_late_ms: Option<f64>,
    pub video_demux_late_ms: Option<f64>,
    pub sound_map_ms: f64,
    /// The transport stream reader's counts, every channel so far.
    pub ts: crate::es::Counts,
    /// The sound against its PMT (`es::SoundState`), this channel's.
    pub sound_state: &'static str,
    pub sound_codec: Option<&'static str>,
    pub sound_push_errors: u64,
    pub ts_push_errors: u64,
    pub teardown_errors: u64,
    pub audio_silences: u64,
    pub audio_silence_ms: f64,
    pub audio_silence_longest_ms: f64,
    pub plane_full_dropped: u64,
}

impl Health {
    pub fn report(
        &self,
        generation: u64,
        video_due_ns: i64,
        audio_due_ns: i64,
        audio_late_ns: Option<i64>,
        device: &mut Device,
    ) -> Report {
        let (lead_least, late) = {
            let mut l = self.lead.lock().unwrap_or_else(|p| p.into_inner());
            (l.least.take(), l.late)
        };
        // Read on its own: a guard taken inside the struct below would live
        // to the end of it, and a second lock of the same mutex there waited
        // on the first for ever -- and the demultiplexer with it (2026-11-02).
        let (audio_demux_late, video_demux_late) = {
            let d = self.demux.lock().unwrap_or_else(|p| p.into_inner());
            (d.audio, d.video)
        };
        let map_diff = self.map.lock().unwrap_or_else(|p| p.into_inner()).1;
        let (ts, sound_state, sound_codec) = {
            let t = self.ts.lock().unwrap_or_else(|p| p.into_inner());
            (t.before.plus(&t.now), t.state.map_or("none", |s| s.name()), t.codec.map(|c| c.name()))
        };
        let (audio_silences, audio_silence_ms, audio_silence_longest_ms) = {
            let s = self.silence.lock().unwrap_or_else(|p| p.into_inner());
            (s.count, s.total_ns as f64 / 1e6, s.longest_ns as f64 / 1e6)
        };
        let flow = self.audio.lock().unwrap_or_else(|p| p.into_inner());
        Report {
            audio_silences,
            audio_silence_ms,
            audio_silence_longest_ms,
            plane_full_dropped: self.plane_full_dropped.load(Ordering::Relaxed),
            sound_map_ms: map_diff as f64 / 1e6,
            ts,
            sound_state,
            sound_codec,
            sound_push_errors: self.sound_push_errors.load(Ordering::Relaxed),
            ts_push_errors: self.ts_push_errors.load(Ordering::Relaxed),
            teardown_errors: self.teardown_errors.load(Ordering::Relaxed),
            audio_lead_least_ms: lead_least.map(|l| l as f64 / 1e6),
            audio_late: late,
            sink_late: self.sink_late.load(Ordering::Relaxed),
            sink_discont: self.sink_discont.load(Ordering::Relaxed),
            audio_demux_late_ms: audio_demux_late.map(|l| l as f64 / 1e6),
            video_demux_late_ms: video_demux_late.map(|l| l as f64 / 1e6),
            at_ns: mediabox_core::timeline::now_ns(),
            generation,
            edge_ns: self.edge_ns(),
            video_due_ns,
            audio_due_ns,
            audio_late_ns,
            audio_buffers: flow.buffers,
            audio_gaps: flow.gaps,
            audio_gap_ms: flow.gap_ns as f64 / 1e6,
            audio_overlaps: flow.overlaps,
            audio_overlap_ms: flow.overlap_ns as f64 / 1e6,
            audio_restarts: device.restarts,
            audio_queue_least_ms: device.take_least_ms(),
            audio_queue_ms: (device.rate > 0).then(|| device.buffer as f64 * 1000.0 / device.rate as f64),
            video_dropped: self.video_dropped.load(Ordering::Relaxed),
        }
    }
}

/// The counts that are faults, as they stood when channel `generation`
/// started (its first report), and this channel's share of them.
fn this_channel(r: &Report) -> serde_json::Value {
    static START: Mutex<Option<(u64, [u64; 10])>> = Mutex::new(None);
    let now = [
        r.audio_gaps,
        r.audio_overlaps,
        r.audio_late,
        r.sink_late,
        r.sink_discont,
        r.audio_restarts,
        r.audio_silences,
        r.video_dropped,
        r.plane_full_dropped,
        r.ts.cc_loss,
    ];
    let mut start = START.lock().unwrap_or_else(|p| p.into_inner());
    if start.is_none_or(|(generation, _)| generation != r.generation) {
        *start = Some((r.generation, now));
    }
    let base = start.map_or(now, |(_, base)| base);
    let d = |i: usize| now[i].saturating_sub(base[i]);
    serde_json::json!({
        "generation": r.generation,
        "audio_gaps": d(0), "audio_overlaps": d(1), "audio_late": d(2), "sink_late": d(3),
        "sink_discont": d(4), "audio_restarts": d(5), "audio_silences": d(6),
        "video_dropped": d(7), "plane_full_dropped": d(8), "cc_loss": d(9),
    })
}

pub fn write(report: &Report) {
    let r = report;
    let mut json = serde_json::json!({
        "at_ns": r.at_ns, "generation": r.generation, "edge_ns": r.edge_ns,
        "video_due_ns": r.video_due_ns, "audio_due_ns": r.audio_due_ns, "audio_late_ns": r.audio_late_ns,
        "audio_buffers": r.audio_buffers, "audio_gaps": r.audio_gaps, "audio_gap_ms": r.audio_gap_ms,
        "audio_overlaps": r.audio_overlaps, "audio_overlap_ms": r.audio_overlap_ms,
        "audio_restarts": r.audio_restarts, "audio_queue_least_ms": r.audio_queue_least_ms,
        "audio_queue_ms": r.audio_queue_ms, "video_dropped": r.video_dropped,
        "audio_lead_least_ms": r.audio_lead_least_ms, "audio_late": r.audio_late,
        "sink_late": r.sink_late, "sink_discont": r.sink_discont,
        "audio_demux_late_ms": r.audio_demux_late_ms, "video_demux_late_ms": r.video_demux_late_ms,
        "sound_map_ms": r.sound_map_ms,
        "sound_state": r.sound_state, "sound_codec": r.sound_codec,
    });
    // Apart: in one json! the macro runs past its recursion limit.
    json["ts"] = serde_json::json!({
            "tei": r.ts.tei, "scrambled": r.ts.scrambled, "cc_loss": r.ts.cc_loss,
            "duplicate": r.ts.duplicate, "duplicate_excess": r.ts.duplicate_excess,
            "adaptation_bad": r.ts.adaptation_bad, "pes_bad": r.ts.pes_bad, "pes_foreign": r.ts.pes_foreign,
            "timestamp_bad": r.ts.timestamp_bad, "discontinuity_announced": r.ts.discontinuity_announced,
            "timebase_discontinuity": r.ts.timebase_discontinuity, "recovered": r.ts.recovered,
            "pid_reset": r.ts.pid_reset, "pmt_changes": r.ts.pmt_changes,
            "pcr_pid_changes": r.ts.pcr_pid_changes, "psi_bad": r.ts.psi_bad,
        });
    json["sound_push_errors"] = r.sound_push_errors.into();
    json["ts_push_errors"] = r.ts_push_errors.into();
    json["teardown_errors"] = r.teardown_errors.into();
    json["audio_silences"] = r.audio_silences.into();
    json["audio_silence_ms"] = r.audio_silence_ms.into();
    json["audio_silence_longest_ms"] = r.audio_silence_longest_ms.into();
    json["plane_full_dropped"] = r.plane_full_dropped.into();
    // Every count above runs for the life of the process, through every
    // channel change: a fault is never taken back by the next channel.
    // This channel's own share is apart, from where they stood at its start.
    json["counts_are"] = "since-start".into();
    json["this_channel"] = this_channel(r);
    let Ok(json) = serde_json::to_vec(&json) else { return };
    let path = std::path::Path::new(PATH);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, json).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alsa_names_find_their_proc_status() {
        assert_eq!(card_and_device("hdmi:CARD=rockchiphdmi0,DEV=0"), (Some("rockchiphdmi0".into()), Some(0)));
        assert_eq!(card_and_device("hw:1,0"), (Some("card1".into()), Some(0)));
        assert_eq!(card_and_device("hdmi:CARD=rockchiphdmi0,DEV=0,AES0=0x06"), (Some("rockchiphdmi0".into()), Some(0)));
    }

    #[test]
    fn a_missing_sound_buffer_is_a_gap_and_a_doubled_one_an_overlap() {
        let health = Health::default();
        health.audio(1, 0, Some(24_000_000));
        health.audio(1, 24_000_000, Some(24_000_000));
        health.audio(1, 72_000_000, Some(24_000_000));
        health.audio(1, 80_000_000, Some(24_000_000));
        let flow = health.audio.lock().unwrap();
        assert_eq!((flow.gaps, flow.gap_ns, flow.overlaps, flow.overlap_ns), (1, 24_000_000, 1, 16_000_000));
    }
}

/// Faults made on purpose to show the monitor sees them, and only for that:
/// MEDIABOX_LIVE_FAULT, comma-separated -- `skip=N` leaves out every Nth
/// picture, `audio-drop=N` every Nth sound buffer, `audio-stall=MS` holds
/// the sound for MS every five seconds, `video-late=MS` / `audio-late=MS`
/// shift a sink. Unset, none.
#[derive(Default, Debug, Clone, Copy)]
pub struct Faults {
    pub skip: u64,
    pub audio_drop: u64,
    pub audio_stall_ms: u64,
    pub video_late_ms: i64,
    pub audio_late_ms: i64,
}

impl Faults {
    pub fn from_env() -> Self {
        let mut f = Self::default();
        let Ok(spec) = std::env::var("MEDIABOX_LIVE_FAULT") else { return f };
        for part in spec.split(',') {
            let Some((k, v)) = part.split_once('=') else { continue };
            let n = v.trim().parse::<i64>().unwrap_or(0);
            match k.trim() {
                "skip" => f.skip = n.max(0) as u64,
                "audio-drop" => f.audio_drop = n.max(0) as u64,
                "audio-stall" => f.audio_stall_ms = n.max(0) as u64,
                "video-late" => f.video_late_ms = n,
                "audio-late" => f.audio_late_ms = n,
                _ => {}
            }
        }
        eprintln!("mediabox-live: BİLEREK KUSUR (monitör sınaması): {f:?}");
        f
    }
}
