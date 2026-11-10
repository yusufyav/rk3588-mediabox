//! The live monitor, the panel's side (key M on live television).
//!
//! Every picture is counted when the kernel says it went up -- the page-flip
//! event's timestamp, not when this process asked for it -- against when the
//! player said it was due (its PTS on the pipeline's clock):
//!
//! - skipped: the next picture shown is more than one picture's time after
//!   the one before (`due` steps by more than a frame): pictures between them
//!   were never on the panel, whoever left them out;
//! - repeated: a picture stayed up a vblank or more longer than its time
//!   (`flip` steps further than `due`): the panel showed it again;
//! - short: a picture went down a vblank or more early;
//! - late: flip time less due time, every picture.
//!
//! The player's side (`/run/mediabox-live/health.json`, mediabox-live's
//! `health`) adds the sound and the broadcast's clock; `text` puts both on
//! the screen.


#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Counts {
    pub frames: u64,
    pub skipped: u64,
    pub repeated: u64,
    pub short: u64,
    pub late_min: i64,
    pub late_max: i64,
    late_sum: i64,
}

impl Counts {
    fn late(&mut self, late: i64) {
        if self.frames == 0 {
            self.late_min = late;
            self.late_max = late;
        } else {
            self.late_min = self.late_min.min(late);
            self.late_max = self.late_max.max(late);
        }
        self.late_sum += late;
        self.frames += 1;
    }

    pub fn late_mean(&self) -> i64 {
        if self.frames == 0 { 0 } else { self.late_sum / self.frames as i64 }
    }

    fn faults(&self) -> u64 {
        self.skipped + self.repeated + self.short
    }
}

/// Why the presenter gave a frame back unseen (`video::Sink`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drop {
    /// It reached this process after its vblank's commit deadline: the
    /// player's side (decoder, sink or socket) was late with it.
    CameLate,
    /// It came in time and was not committed at its vblank: this
    /// process's presenter was late.
    NotCommitted,
    /// It shares its vblank with the frame after it: more frames than
    /// the panel has vblanks.
    Cadence,
    /// Replaced untimed, or its commit refused.
    Other,
}

/// The presentation path's own account, beside what the panel showed: each
/// picture the panel skipped is one the player never sent or one given back
/// here, and this says which.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Path {
    pub came_late: u64,
    pub not_committed: u64,
    pub cadence: u64,
    pub other: u64,
    /// The least margin a frame came with before its vblank's commit
    /// deadline, ns (negative: after it).
    pub margin_min: Option<i64>,
    /// The presenter's worst wake past its timeout, its wait for the
    /// stage lock and one pass's work, ns.
    pub woke_late_max: u64,
    pub lock_max: u64,
    pub pass_max: u64,
}

impl Path {
    pub fn dropped(&self) -> u64 {
        self.came_late + self.not_committed + self.cadence + self.other
    }
}

#[derive(Default)]
pub struct Monitor {
    /// The picture last shown: (due, flip).
    last: Option<(u64, u64)>,
    /// One picture's time: the least step between dues seen in this stream
    /// `FRAME_CONFIRM` times running.
    frame: Option<u64>,
    /// The step seen last and how many times running.
    run: (u64, u32),
    /// This stream's, since its first picture.
    pub stream: Counts,
    /// Since this process started, whatever the channel: a channel change
    /// starts `stream` again and must not take the faults before it with it.
    pub total: Counts,
    /// The second being counted, and the last whole one.
    second: Counts,
    second_from: u64,
    pub last_second: Counts,
    /// When the stream's first picture went up.
    pub stream_from: u64,
    pub period: u64,
    /// The presentation path: this second's, the last whole one's, and the
    /// session's.
    path: Path,
    pub last_path: Path,
    pub path_stream: Path,
    pub path_total: Path,
}

/// Dues further apart than this, or backwards, are another stream.
const NEW_STREAM_NS: i64 = 1_000_000_000;
/// A step shorter than the frame is the frame only once seen this many times
/// running: a 25p stream's one field-length step (a 50i edit point) is not
/// its rate, and taken as it, every 40 ms step after it counted a skip.
const FRAME_CONFIRM: u32 = 3;
/// Steps this close are the same step (a PTS's 90 kHz rounding).
const SAME_STEP_NS: u64 = 1_000_000;

impl Monitor {
    pub fn arrived(&mut self, margin: i64) {
        self.path.margin_min = Some(self.path.margin_min.map_or(margin, |m| m.min(margin)));
    }

    pub fn presenter_dropped(&mut self, why: Drop) {
        for path in [&mut self.path, &mut self.path_stream, &mut self.path_total] {
            match why {
                Drop::CameLate => path.came_late += 1,
                Drop::NotCommitted => path.not_committed += 1,
                Drop::Cadence => path.cadence += 1,
                Drop::Other => path.other += 1,
            }
        }
    }

    pub fn presenter_pass(&mut self, woke_late: Option<u64>, lock: u64, pass: u64) {
        let p = &mut self.path;
        p.woke_late_max = p.woke_late_max.max(woke_late.unwrap_or(0));
        p.lock_max = p.lock_max.max(lock);
        p.pass_max = p.pass_max.max(pass);
    }

    /// A picture due at `due` went up at `flip` (CLOCK_MONOTONIC ns), the
    /// panel refreshing every `period` ns.
    pub fn shown(&mut self, due: u64, flip: u64, period: u64) {
        self.period = period;
        let late = flip as i64 - due as i64;
        if let Some((last_due, last_flip)) = self.last {
            let step = due as i64 - last_due as i64;
            if step <= 0 || step > NEW_STREAM_NS {
                self.start(due, flip, late);
                return;
            }
            let step = step as u64;
            if step >= 5_000_000 {
                self.run = if self.run.0.abs_diff(step) <= SAME_STEP_NS { (self.run.0, self.run.1 + 1) } else { (step, 1) };
                self.frame = match self.frame {
                    None => Some(step),
                    Some(f) if step < f && self.run.1 >= FRAME_CONFIRM => Some(self.run.0.min(step)),
                    Some(f) => Some(f),
                };
            }
            let frame = self.frame.unwrap_or(step);
            let mut now = Counts::default();
            if step * 2 > frame * 3 {
                let skipped = (step + frame / 2) / frame - 1;
                now.skipped = skipped;
                mediabox_core::timeline!("mon-skip", "due={due} step={step} frame={frame} skipped={skipped}");
            }
            let deviation = (flip as i64 - last_flip as i64) - step as i64;
            let half = (period / 2) as i64;
            if period > 0 && deviation >= half {
                now.repeated = (deviation as u64 + period / 2) / period;
                mediabox_core::timeline!("mon-repeat", "due={due} held_ms={:.1} vblanks={}", deviation as f64 / 1e6, now.repeated);
            } else if period > 0 && deviation <= -half {
                now.short = 1;
                mediabox_core::timeline!("mon-short", "due={due} early_ms={:.1}", -deviation as f64 / 1e6);
            }
            for counts in [&mut self.stream, &mut self.second, &mut self.total] {
                counts.skipped += now.skipped;
                counts.repeated += now.repeated;
                counts.short += now.short;
                counts.late(late);
            }
            self.last = Some((due, flip));
            self.roll(flip);
        } else {
            self.start(due, flip, late);
        }
    }

    fn start(&mut self, due: u64, flip: u64, late: i64) {
        self.last = Some((due, flip));
        self.frame = None;
        self.run = (0, 0);
        self.stream = Counts::default();
        self.stream.late(late);
        self.second = Counts::default();
        self.second.late(late);
        self.second_from = flip;
        self.stream_from = flip;
        self.path_stream = Path::default();
        mediabox_core::timeline!("mon-start", "due={due} flip={flip}");
    }

    /// A whole second counted: kept for the screen, and said in the journal
    /// when anything was wrong in it.
    fn roll(&mut self, flip: u64) {
        if flip < self.second_from + 1_000_000_000 {
            return;
        }
        let second = std::mem::take(&mut self.second);
        let path = std::mem::take(&mut self.path);
        if second.faults() > 0 || path.dropped() > 0 {
            eprintln!(
                "mediabox-tv.monitor: son saniyede {} kare: {} atlanan, {} tekrar, {} kısa; geç kalma {:.1}..{:.1} ms; \
                 sunum bıraktı {} (geç gelen {}, işlenmeyen {}, kadans {}, diğer {}); en az pay {}; \
                 sunucu en geç uyanış {:.1} ms, kilit {:.1} ms, tur {:.1} ms; \
                 oturum toplamı {} kare: {} atlanan, {} tekrar, {} kısa; sunum bıraktı {} (geç gelen {}, işlenmeyen {})",
                second.frames,
                second.skipped,
                second.repeated,
                second.short,
                second.late_min as f64 / 1e6,
                second.late_max as f64 / 1e6,
                path.dropped(),
                path.came_late,
                path.not_committed,
                path.cadence,
                path.other,
                path.margin_min.map_or("-".into(), |m| format!("{:.1} ms", m as f64 / 1e6)),
                path.woke_late_max as f64 / 1e6,
                path.lock_max as f64 / 1e6,
                path.pass_max as f64 / 1e6,
                self.total.frames,
                self.total.skipped,
                self.total.repeated,
                self.total.short,
                self.path_total.dropped(),
                self.path_total.came_late,
                self.path_total.not_committed
            );
        }
        self.last_path = path;
        mediabox_core::timeline!(
            "mon-second",
            "frames={} skipped={} repeated={} short={} late_min={} late_max={} late_mean={}",
            second.frames,
            second.skipped,
            second.repeated,
            second.short,
            second.late_min,
            second.late_max,
            second.late_mean()
        );
        self.last_second = second;
        self.second_from = flip;
    }

    pub fn frame(&self) -> Option<u64> {
        self.frame
    }
}

/// Counted on the presentation thread (`platform::presenter`), where the
/// page flips are read; read by the interface for the live monitor.
static MONITOR: std::sync::Mutex<Option<Monitor>> = std::sync::Mutex::new(None);

pub fn shown(due: u64, flip: u64, period: u64) {
    let mut guard = MONITOR.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(Monitor::default).shown(due, flip, period);
}

pub fn arrived(margin: i64) {
    let mut guard = MONITOR.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(Monitor::default).arrived(margin);
}

pub fn presenter_dropped(why: Drop) {
    let mut guard = MONITOR.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(Monitor::default).presenter_dropped(why);
}

pub fn presenter_pass(woke_late: Option<u64>, lock: u64, pass: u64) {
    let mut guard = MONITOR.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(Monitor::default).presenter_pass(woke_late, lock, pass);
}

pub fn with<R>(f: impl FnOnce(&Monitor) -> R) -> R {
    let mut guard = MONITOR.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Monitor::default))
}

/// The player's report (`/run/mediabox-live/health.json`) and the one before
/// it a second or more earlier, for the sound's counts over that second.
#[derive(Default)]
pub struct PlayerReports {
    now: Option<serde_json::Value>,
    before: Option<serde_json::Value>,
}

impl PlayerReports {
    pub fn read(&mut self) {
        let Some(report) = std::fs::read(PLAYER_HEALTH).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else {
            self.now = None;
            return;
        };
        let at = |v: &serde_json::Value| v["at_ns"].as_u64().unwrap_or(0);
        if self.before.as_ref().is_none_or(|b| at(&report) >= at(b) + 1_000_000_000 || at(&report) < at(b)) {
            self.before = self.now.take();
        }
        self.now = Some(report);
    }
}

pub const PLAYER_HEALTH: &str = "/run/mediabox-live/health.json";

fn ms(ns: i64) -> String {
    format!("{:+.0} ms", ns as f64 / 1e6)
}

/// A row's state: as the screen colours it.
pub const OK: i32 = 0;
pub const WAS: i32 = 1;
pub const NOW: i32 = 2;
pub const UNKNOWN: i32 = 3;

/// The monitor's rows for the screen: (label, value, state).
pub fn rows(reports: &PlayerReports) -> Vec<(String, String, i32)> {
    let now_ns = mediabox_core::timeline::now_ns();
    let mut rows = Vec::new();
    with(|m| {
        let s = &m.stream;
        let l = &m.last_second;
        let flowing = m.last.is_some_and(|(_, flip)| now_ns.saturating_sub(flip) < 500_000_000);
        let fps = m.frame().map_or("-".into(), |f| format!("{:.0} kare/s", 1e9 / f as f64));
        rows.push((
            "Görüntü".into(),
            if flowing { format!("{fps} · {} kare gösterildi", s.frames) } else { "KARE GELMİYOR".into() },
            if flowing { OK } else { NOW },
        ));
        let state = |now: u64, total: u64| if now > 0 { NOW } else if total > 0 { WAS } else { OK };
        rows.push(("Atlanan kare".into(), format!("{}  (son 1 s: {})", s.skipped, l.skipped), state(l.skipped, s.skipped)));
        let (p, pl) = (&m.path_stream, &m.last_path);
        rows.push((
            "Sunumun bıraktığı".into(),
            format!("{}  (geç gelen {} · işlenmeyen {} · kadans {})  son 1 s: {}", p.dropped(), p.came_late, p.not_committed, p.cadence, pl.dropped()),
            state(pl.dropped(), p.dropped()),
        ));
        rows.push(("Tekrar / kısa kare".into(), format!("{} / {}  (son 1 s: {} / {})", s.repeated, s.short, l.repeated, l.short), state(l.repeated + l.short, s.repeated + s.short)));
        rows.push((
            "Ekrana çıkış sapması".into(),
            format!("ort {}  ({} … {})", ms(l.late_mean()), ms(l.late_min), ms(l.late_max)),
            if l.frames == 0 { UNKNOWN } else if l.late_min.abs().max(l.late_max.abs()) <= m.period as i64 / 2 + 1_000_000 { OK } else { NOW },
        ));
        let Some(p) = reports.now.as_ref().filter(|p| now_ns.saturating_sub(p["at_ns"].as_u64().unwrap_or(0)) < 1_500_000_000) else {
            rows.push(("Oynatıcı".into(), "rapor yok".into(), UNKNOWN));
            return;
        };
        let get = |k: &str| p[k].as_i64();
        let u = |k: &str| p[k].as_u64().unwrap_or(0);
        let delta = |k: &str| u(k).saturating_sub(reports.before.as_ref().and_then(|b| b[k].as_u64()).unwrap_or(u(k)));
        let video_due = get("video_due_ns").unwrap_or(0);
        let audio_due = get("audio_due_ns").unwrap_or(0);
        let late = l.late_mean();
        match get("edge_ns").filter(|_| l.frames > 0) {
            Some(edge) => {
                let behind = video_due + late - edge;
                rows.push(("Canlıdan geride".into(), format!("{:.0} ms", behind as f64 / 1e6), if behind <= 300_000_000 { OK } else { NOW }));
            }
            None => rows.push(("Canlıdan geride".into(), "ölçülemiyor".into(), UNKNOWN)),
        }
        match get("audio_late_ns").filter(|_| l.frames > 0) {
            Some(audio_late) => {
                let av = (audio_due + audio_late) - (video_due + late);
                let words = if av.abs() <= 20_000_000 { "" } else if av > 0 { "  ses geride" } else { "  ses önde" };
                rows.push(("Ses − görüntü".into(), format!("{}{words}", ms(av)), if av.abs() <= 20_000_000 { OK } else if av.abs() <= 45_000_000 { WAS } else { NOW }));
            }
            None => rows.push(("Ses − görüntü".into(), "ses çalmıyor".into(), UNKNOWN)),
        }
        // Every way sound was heard broken: missing or doubled in the stream,
        // reaching the sink after its time, or repaired by the sink itself.
        let keys = ["audio_gaps", "audio_overlaps", "audio_late", "sink_late", "sink_discont"];
        let breaks: u64 = keys.iter().map(|k| u(k)).sum();
        let breaks_now: u64 = keys.iter().map(|k| delta(k)).sum();
        rows.push((
            "Ses kesintisi".into(),
            format!(
                "{breaks}  (akışta {} · geç gelen {} · aygıt onardı {})  son 1 s: {breaks_now}",
                u("audio_gaps") + u("audio_overlaps"),
                u("audio_late"),
                u("sink_late") + u("sink_discont")
            ),
            state(breaks_now, breaks),
        ));
        let lead = p["audio_lead_least_ms"].as_f64();
        rows.push((
            "Ses payı".into(),
            lead.map_or("-".into(), |l| format!("en az {l:.0} ms önce geldi")),
            match lead {
                None => UNKNOWN,
                Some(l) if l < 0.0 => NOW,
                Some(l) if l < 15.0 => WAS,
                Some(_) => OK,
            },
        ));
        let least = p["audio_queue_least_ms"].as_f64();
        rows.push((
            "Ses aygıtı".into(),
            format!(
                "kuyruk en az {}  · yeniden başlama {}",
                least.map_or("-".into(), |l| format!("{l:.0} ms")),
                u("audio_restarts")
            ),
            if delta("audio_restarts") > 0 || least.is_some_and(|l| l < 5.0) { NOW } else if u("audio_restarts") > 0 { WAS } else if least.is_none() { UNKNOWN } else { OK },
        ));
        rows.push(("Oynatıcının bilerek attığı".into(), format!("{} kare", u("video_dropped")), state(delta("video_dropped"), u("video_dropped"))));
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// 25 fps on a 50 Hz panel: a picture every 40 ms, two vblanks each.
    fn run(steps: &[(u64, u64)]) -> Monitor {
        let mut m = Monitor::default();
        for &(due, flip) in steps {
            m.shown(due, flip, 20 * MS);
        }
        m
    }

    /// The player's own drops are `video_dropped`; the sink's QoS count
    /// (`late_dropped`, the IPC's, never in health.json) is not shown as one.
    #[test]
    fn the_player_s_drops_are_its_video_dropped() {
        let at = mediabox_core::timeline::now_ns();
        let reports = PlayerReports {
            now: Some(serde_json::json!({"at_ns": at, "video_dropped": 4, "late_dropped": 99})),
            before: Some(serde_json::json!({"at_ns": at - 1_000_000_000, "video_dropped": 1, "late_dropped": 0})),
        };
        let rows = rows(&reports);
        let row = rows.iter().find(|(label, _, _)| label == "Oynatıcının bilerek attığı").expect("the row");
        assert_eq!((row.1.as_str(), row.2), ("4 kare", NOW));
    }

    #[test]
    fn a_steady_film_has_no_faults() {
        let steps: Vec<_> = (0..100).map(|i| (1000 * MS + i * 40 * MS, 1005 * MS + i * 40 * MS)).collect();
        let m = run(&steps);
        assert_eq!((m.stream.skipped, m.stream.repeated, m.stream.short), (0, 0, 0));
        assert_eq!(m.stream.frames, 100);
        assert_eq!(m.stream.late_mean(), 5 * MS as i64);
    }

    #[test]
    fn a_picture_left_out_is_skipped_and_one_held_is_repeated() {
        let mut steps: Vec<_> = (0..10).map(|i| (1000 * MS + i * 40 * MS, 1000 * MS + i * 40 * MS)).collect();
        // Picture 10 never shown: 11 follows 9.
        steps.push((1000 * MS + 11 * 40 * MS, 1000 * MS + 11 * 40 * MS));
        // Picture 12 a vblank late: 11 held for three vblanks, 12 for one.
        steps.push((1000 * MS + 12 * 40 * MS, 1000 * MS + 12 * 40 * MS + 20 * MS));
        steps.push((1000 * MS + 13 * 40 * MS, 1000 * MS + 13 * 40 * MS));
        let m = run(&steps);
        assert_eq!(m.stream.skipped, 1);
        assert_eq!(m.stream.repeated, 1);
        assert_eq!(m.stream.short, 1);
    }

    #[test]
    fn another_channel_starts_the_count_again() {
        let mut steps: Vec<_> = (0..10).map(|i| (1000 * MS + i * 40 * MS, 1000 * MS + i * 40 * MS)).collect();
        // Picture 10 never shown, then another channel.
        steps.push((1000 * MS + 11 * 40 * MS, 1000 * MS + 11 * 40 * MS));
        steps.push((100 * MS, 3000 * MS));
        let m = run(&steps);
        assert_eq!(m.stream.frames, 1);
        assert_eq!(m.stream.skipped, 0);
        // The session's count keeps what the channel before lost.
        assert_eq!(m.total.skipped, 1);
    }

    fn cadence(period: u64, step: u64, n: u64, leave_out: &[u64]) -> Monitor {
        let mut m = Monitor::default();
        for i in (0..n).filter(|i| !leave_out.contains(i)) {
            let due = 1000 * MS + i * step;
            // The panel's vblank at or after it, on a grid of `period`.
            let flip = due.div_ceil(period) * period + 3 * MS;
            m.shown(due, flip, period);
        }
        m
    }

    #[test]
    fn steady_50p_25p_and_50i_as_frames_count_nothing() {
        for (period, step) in [(20 * MS, 20 * MS), (20 * MS, 40 * MS), (40 * MS / 2, 40 * MS)] {
            let m = cadence(period, step, 3000, &[]);
            assert_eq!((m.stream.skipped, m.stream.repeated, m.stream.short), (0, 0, 0), "step {step}");
        }
    }

    #[test]
    fn one_picture_missing_from_50p_is_one_skipped() {
        let m = cadence(20 * MS, 20 * MS, 500, &[100, 300, 301]);
        assert_eq!(m.stream.skipped, 3);
        assert_eq!((m.stream.repeated, m.stream.short), (0, 0));
    }

    /// 25p whose decoder hands one picture with a field's PTS step (20 ms,
    /// a 50i edit point): the least step alone made every 40 ms step after
    /// it a skipped picture.
    #[test]
    fn a_stray_field_step_does_not_halve_the_frame() {
        let mut m = Monitor::default();
        let mut due = 1000 * MS;
        for i in 0..300u64 {
            due += if i == 50 { 20 * MS } else { 40 * MS };
            m.shown(due, due + 3 * MS, 20 * MS);
        }
        assert_eq!(m.stream.skipped, 0);
        assert_eq!(m.frame(), Some(40 * MS));
    }

    /// 25p turning into 50p (a programme change on the same service): the
    /// frame follows, and a picture missing after it still counts.
    #[test]
    fn a_rate_going_up_is_followed() {
        let mut m = Monitor::default();
        let mut due = 1000 * MS;
        for _ in 0..100 {
            due += 40 * MS;
            m.shown(due, due, 20 * MS);
        }
        for i in 0..100 {
            due += if i == 50 { 40 * MS } else { 20 * MS };
            m.shown(due, due, 20 * MS);
        }
        assert_eq!(m.frame(), Some(20 * MS));
        assert_eq!(m.stream.skipped, 1);
    }

    #[test]
    fn the_presenter_says_why_it_gave_a_frame_back() {
        let mut m = Monitor::default();
        m.presenter_dropped(Drop::CameLate);
        m.presenter_dropped(Drop::NotCommitted);
        m.presenter_dropped(Drop::NotCommitted);
        m.arrived(5 * MS as i64);
        m.arrived(-2 * MS as i64);
        assert_eq!(m.path.margin_min, Some(-2 * MS as i64));
        assert_eq!((m.path_total.came_late, m.path_total.not_committed, m.path_total.dropped()), (1, 2, 3));
        // A whole second later the second's account is kept for the screen.
        m.shown(1000 * MS, 1000 * MS, 20 * MS);
        m.shown(1020 * MS, 2100 * MS, 20 * MS);
        assert_eq!(m.last_path.dropped(), 3);
        assert_eq!(m.path.dropped(), 0);
    }
}
