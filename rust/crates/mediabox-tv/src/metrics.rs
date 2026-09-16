//! What the television is actually doing, in numbers, without a benchmark run.
//!
//! Everything here is read from files this process already owns — /proc/self —
//! and from timestamps the render loop hands over. There is no sampling thread
//! and no external tool, so leaving it on costs a line in the journal every few
//! seconds and nothing else.

use std::time::{Duration, Instant};

/// A frame slower than this is worth counting separately: at 60 Hz the budget
/// is 16.7 ms, and 20 ms is the point where a person sees the step rather than
/// feels it.
const LONG_FRAME: Duration = Duration::from_millis(20);

const REPORT_EVERY: Duration = Duration::from_secs(5);

pub struct Metrics {
    started: Instant,
    first_frame: Option<Duration>,
    first_data: Option<Duration>,

    window_start: Instant,
    last_frame: Option<Instant>,
    frames: u32,
    long_frames: u32,
    frame_times: Vec<Duration>,

    /// When a key was accepted and a repaint is owed because of it. Cleared by
    /// the first frame that follows, which is what gets measured.
    pending_key: Option<Instant>,
    key_to_render: Vec<Duration>,

    cpu: CpuSample,
}

impl Metrics {
    pub fn new(started: Instant) -> Self {
        Self {
            started,
            first_frame: None,
            first_data: None,
            window_start: Instant::now(),
            last_frame: None,
            frames: 0,
            long_frames: 0,
            frame_times: Vec::with_capacity(512),
            pending_key: None,
            key_to_render: Vec::with_capacity(128),
            cpu: CpuSample::now(),
        }
    }

    /// A key was accepted. The next painted frame is the one the viewer waited
    /// for, so that is where the clock stops.
    pub fn key_accepted(&mut self) {
        if self.pending_key.is_none() {
            self.pending_key = Some(Instant::now());
        }
    }

    pub fn data_arrived(&mut self) {
        if self.first_data.is_none() {
            self.first_data = Some(self.started.elapsed());
            eprintln!(
                "mediabox-tv.startup first_data_ms={}",
                self.first_data.unwrap().as_millis()
            );
        }
    }

    pub fn frame(&mut self) {
        let now = Instant::now();

        if self.first_frame.is_none() {
            self.first_frame = Some(self.started.elapsed());
            eprintln!(
                "mediabox-tv.startup first_frame_ms={}",
                self.first_frame.unwrap().as_millis()
            );
        }

        if let Some(at) = self.pending_key.take() {
            self.key_to_render.push(now.duration_since(at));
        }

        if let Some(previous) = self.last_frame {
            let delta = now.duration_since(previous);
            if delta < Duration::from_secs(1) {
                self.frames += 1;
                if delta > LONG_FRAME {
                    self.long_frames += 1;
                }
                self.frame_times.push(delta);
            }
        }
        self.last_frame = Some(now);
    }

    /// Emits a line if the reporting window has elapsed, and starts a new one.
    ///
    /// The line is the appliance's own evidence, so it carries the budget as
    /// well as the reading: the target is the panel's refresh rate as the
    /// display controller reports it, not a number written here, because a
    /// 50 Hz television and a 60 Hz monitor are two different passes.
    pub fn report_due(&mut self) -> Option<String> {
        let elapsed = self.window_start.elapsed();
        if elapsed < REPORT_EVERY {
            return None;
        }

        let seconds = elapsed.as_secs_f64();
        let fps = if seconds > 0.0 {
            self.frames as f64 / seconds
        } else {
            0.0
        };

        let (_, _, refresh) = crate::platform::active_mode();
        let phases = crate::platform::drain_phases();
        let per_frame = |total: u64| -> f64 {
            if phases.frames == 0 {
                0.0
            } else {
                total as f64 / phases.frames as f64 / 1000.0
            }
        };
        let draw_ms = per_frame(phases.draw_us());
        let flip_ms = per_frame(phases.flip_wait_us);

        let p95 = percentile_ms(&mut self.frame_times, 0.95);
        let p99 = percentile_ms(&mut self.frame_times, 0.99);

        crate::platform::set_last_report(crate::platform::Report {
            frames: self.frames,
            fps,
            draw_ms,
            flip_ms,
        });

        let line = format!(
            "mediabox-tv.metrics {{\"fps\":{fps:.1},\"target_fps\":{refresh},\
             \"p95_frame_ms\":{p95:.1},\"p99_frame_ms\":{p99:.1},\"long_frames\":{long},\
             \"draw_ms\":{draw_ms:.2},\"flip_wait_ms\":{flip_ms:.2},\
             \"swap_ms\":{swap:.2},\"present_ms\":{present:.2},\
             \"key_to_render_p95_ms\":{key},\"rss_kb\":{rss},\"cpu_pct\":{cpu:.1}}}",
            long = self.long_frames,
            swap = per_frame(phases.swap_us),
            present = per_frame(phases.present_us),
            key = percentile_ms(&mut self.key_to_render, 0.95) as u64,
            rss = rss_kb().unwrap_or(0),
            cpu = self.cpu.advance(),
        );

        self.window_start = Instant::now();
        self.frames = 0;
        self.long_frames = 0;
        self.frame_times.clear();
        self.key_to_render.clear();

        Some(line)
    }
}

fn percentile_ms(samples: &mut Vec<Duration>, q: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_unstable();
    let index = ((samples.len() as f64 - 1.0) * q).round() as usize;
    samples[index].as_secs_f64() * 1000.0
}

fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// This process's own CPU time against wall time, as a percentage of one core.
struct CpuSample {
    ticks: u64,
    at: Instant,
    hz: u64,
}

impl CpuSample {
    fn now() -> Self {
        Self {
            ticks: process_ticks().unwrap_or(0),
            at: Instant::now(),
            // SAFETY: sysconf is a pure lookup with no pointer arguments.
            hz: unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64,
        }
    }

    fn advance(&mut self) -> f64 {
        let ticks = process_ticks().unwrap_or(self.ticks);
        let at = Instant::now();
        let wall = at.duration_since(self.at).as_secs_f64();

        let used = ticks.saturating_sub(self.ticks) as f64 / self.hz as f64;
        self.ticks = ticks;
        self.at = at;

        if wall > 0.0 { used / wall * 100.0 } else { 0.0 }
    }
}

fn process_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The command name may contain spaces and is parenthesised; everything
    // after the closing parenthesis is positional.
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the whole line, which is 11 and
    // 12 of what follows the name.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}
