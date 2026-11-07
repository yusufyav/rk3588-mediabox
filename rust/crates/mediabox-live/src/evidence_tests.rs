use super::*;

/// The dumps in a directory: not its lock, nor what is half made.
fn dumps_in(dir: &Path) -> Vec<String> {
    let mut made: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.') && n != "active.jsonl")
        .collect();
    made.sort();
    made
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mediabox-evidence-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn off_without_its_directory() {
    // The production path: nothing named, nothing made, nothing kept.
    assert!(std::env::var("MEDIABOX_LIVE_EVIDENCE").is_err());
    assert!(get().is_none());
}

#[test]
fn a_ring_keeps_its_last_seconds() {
    let mut ring = Ring::default();
    for k in 0..40u64 {
        ring.push(Pes { at: k * 1_000_000_000, generation: 1, raw: k, pts: k });
    }
    let kept = ring.window(0, u64::MAX);
    assert_eq!(kept.first().map(|p| p.at), Some(39_000_000_000 - KEEP_NS));
    assert_eq!(ring.window(30_000_000_000, 32_000_000_000).len(), 3);
}

fn ts_packet(pid: u16, af_pcr: bool) -> Vec<u8> {
    let mut p = vec![0x47, (pid >> 8) as u8, pid as u8, if af_pcr { 0x30 } else { 0x10 }];
    if af_pcr {
        p.extend_from_slice(&[7, 0x10, 0, 0, 0, 0, 0, 0]);
    }
    p.resize(188, 0xFF);
    p
}

#[test]
fn the_service_packets_kept_are_the_tables_the_sound_and_the_pcrs() {
    let mut data = Vec::new();
    for (pid, pcr) in [(0, false), (0x100, false), (0x201, false), (0x101, false), (0x101, true), (0x300, false)] {
        data.extend(ts_packet(pid, pcr));
    }
    let kept = keep(&data, &[0x100], Some(0x201), Some(0x101));
    let pids: Vec<u16> = kept.chunks(188).map(pid).collect();
    assert_eq!(pids, vec![0, 0x100, 0x201, 0x101]);
    assert!(carries_pcr(&kept[3 * 188..]));
}

#[test]
fn the_alsa_status_is_read_as_the_kernel_writes_it() {
    let text = "state: RUNNING\nowner_pid   : 1262037\ntrigger_time: 45566.123456789\ntstamp      : 45570.000500000\ndelay       : 4512\navail       : 288\navail_max   : 1056\n-----\nhw_ptr      : 196608\nappl_ptr    : 201120\n";
    let a = Alsa::parse(text);
    assert!(a.running);
    assert_eq!(a.trigger_ns, Some(45_566_123_456_789));
    assert_eq!(a.tstamp_ns, Some(45_570_000_500_000));
    assert_eq!((a.hw_ptr, a.appl_ptr, a.delay, a.avail), (Some(196_608), Some(201_120), Some(4512), Some(288)));
    assert!(!Alsa::parse("closed\n").running);
}

#[test]
fn a_clock_reading_off_its_seconds_median_is_an_event_once_in_its_spacing() {
    let mut w = ClockWatch::default();
    let mut found = Vec::new();
    for k in 0..300u64 {
        let late = if k == 150 || k == 200 { 10_000_000 } else { 0 };
        if let Some(off) = w.see(k * 10_000_000, late) {
            found.push((k, off));
        }
    }
    // The second, 0.5 s later, is inside the spacing.
    assert_eq!(found, vec![(150, 10_000_000)]);
}

#[test]
fn overlapping_requests_of_a_kind_are_one_window() {
    let e = Evidence::new(scratch("merge"), 1 << 20);
    e.request("decoded_pcm_below_threshold", 20_000_000_000, 20_300_000_000, serde_json::Value::Null);
    e.request("decoded_pcm_below_threshold", 21_000_000_000, 21_500_000_000, serde_json::Value::Null);
    e.request("clock", 21_000_000_000, 21_000_000_000, serde_json::Value::Null);
    let r = e.requests.lock().unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!((r[0].from, r[0].to), (17_000_000_000, 24_500_000_000));
}

fn filled(dir: PathBuf, quota: u64) -> Evidence {
    let e = Evidence::new(dir, quota);
    let at = now();
    e.pcm(1, "PcmFormat { sample: S16, rate: 48000, channels: 2, interleaved: true }", Some(5), false, &[1u8; 4608]);
    e.pes(1, 900_000, 5);
    e.ts(1, &ts_packet(0x201, false), &[], Some(0x201), None);
    e.request("decoded_pcm_below_threshold", at, at, serde_json::json!({ "pts": 5 }));
    e
}

#[test]
fn a_dump_is_written_whole_when_its_window_has_passed() {
    let dir = scratch("dump");
    let e = filled(dir.clone(), 1 << 30);
    e.write_due(now());
    assert_eq!(e.dumps.load(Ordering::Relaxed), 0, "not before its after-window");
    e.write_due(now() + AFTER_NS + 1);
    assert_eq!(e.dumps.load(Ordering::Relaxed), 1);
    let made: Vec<String> = dumps_in(&dir);
    assert_eq!(made.len(), 1);
    assert!(!made[0].starts_with('.'), "no temporary left: {made:?}");
    let dump = dir.join(&made[0]);
    assert_eq!(std::fs::read(dump.join("pcm.raw")).unwrap().len(), 4608);
    assert_eq!(std::fs::read(dump.join("sound.ts")).unwrap().len(), 188);
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dump.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["items"]["pes"], 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_oldest_dumps_go_to_hold_the_quota() {
    let dir = scratch("quota");
    // Room for one dump (4608 + 188 bytes and the indexes), not two.
    let e = filled(dir.clone(), 7_000);
    e.write_due(now() + AFTER_NS + 1);
    let at = now();
    e.request("clock", at, at, serde_json::Value::Null);
    e.write_due(now() + AFTER_NS + 1);
    assert_eq!(e.dumps.load(Ordering::Relaxed), 2);
    let made: Vec<String> = dumps_in(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].starts_with("0001-clock"), "the newer kept: {made:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dump_that_cannot_be_written_is_counted_and_said() {
    let dir = scratch("unwritable");
    // The directory's place taken by a file.
    std::fs::write(&dir, b"not a directory").unwrap();
    let e = filled(dir.clone(), 1 << 30);
    e.write_due(now() + AFTER_NS + 1);
    assert_eq!(e.dumps.load(Ordering::Relaxed), 0);
    assert!(e.write_errors.load(Ordering::Relaxed) >= 1);
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn a_ring_held_by_the_writer_is_passed_by_not_waited_on() {
    let e = Evidence::new(scratch("held"), 1 << 20);
    let held = e.pes.lock().unwrap();
    e.pes(1, 1, 1);
    assert_eq!(e.dropped.load(Ordering::Relaxed), 1);
    drop(held);
    e.pes(1, 2, 2);
    assert_eq!(e.pes.lock().unwrap().window(0, u64::MAX).len(), 1);
}

#[test]
fn the_quota_is_256_mib_unless_given() {
    assert_eq!(DEFAULT_QUOTA, 256 << 20);
}

#[test]
fn a_quota_lock_held_by_another_player_defers_the_clearing_without_waiting() {
    use std::os::fd::AsRawFd;
    let dir = scratch("quota-lock");
    let e = filled(dir.clone(), 7_000);
    e.write_due(now() + AFTER_NS + 1);
    // Another player's writer holds the directory's lock.
    let other = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".quota.lock")).unwrap();
    // SAFETY: a valid descriptor.
    assert_eq!(unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    let at = now();
    e.request("clock", at, at, serde_json::Value::Null);
    let began = std::time::Instant::now();
    e.write_due(now() + AFTER_NS + 1);
    assert!(began.elapsed() < std::time::Duration::from_secs(1), "the lock was waited on");
    assert_eq!(e.quota_deferred.load(Ordering::Relaxed), 1);
    assert_eq!(dumps_in(&dir).len(), 2, "nothing cleared while the lock is held");
    drop(other);
    e.hold_quota().unwrap();
    assert_eq!(dumps_in(&dir).len(), 1, "cleared once the lock is free");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn each_channel_the_evidence_is_kept_for_is_said_once_with_the_players_pid() {
    let dir = scratch("active");
    let e = Evidence::new(dir.clone(), 1 << 20);
    e.service(1, &[0x100], Some(0x201), Some(0x101), Some("mpeg-audio"));
    e.service(1, &[0x100], Some(0x201), Some(0x101), Some("mpeg-audio"));
    e.service(2, &[0x100], Some(0x202), Some(0x101), Some("mpeg-audio"));
    e.write_active();
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(dir.join("active.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["event"], "evidence-active");
    assert_eq!(lines[0]["pid"], std::process::id());
    assert_eq!((lines[0]["generation"].as_u64(), lines[1]["generation"].as_u64()), (Some(1), Some(2)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_request_pending_at_the_players_end_is_written_cut_not_lost() {
    // A silence asked for at `at`; the player ends 2 s later, a second
    // before its after-window (`AFTER_NS`) is due.
    let dir = scratch("shutdown");
    let e = filled(dir.clone(), 1 << 30);
    let end = now() + AFTER_NS - 1_000_000_000;
    e.write_due(end);
    assert_eq!(e.dumps.load(Ordering::Relaxed), 0, "not due yet");
    assert_eq!(e.write_pending_at_shutdown(end), 1);
    let made = dumps_in(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    let dump = dir.join(&made[0]);
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dump.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["truncated_at_shutdown"], true);
    assert_eq!(meta["to"].as_u64(), Some(end), "cut where the player ended, no after-window made up");
    assert!(meta["asked_to"].as_u64().unwrap() > end);
    assert_eq!(std::fs::read(dump.join("pcm.raw")).unwrap().len(), 4608, "what was gathered is kept");
    assert_eq!(e.write_pending_at_shutdown(end), 0, "nothing written twice");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dump_written_in_its_time_is_not_marked_cut() {
    let dir = scratch("whole");
    let e = filled(dir.clone(), 1 << 30);
    e.write_due(now() + AFTER_NS + 1);
    let made = dumps_in(&dir);
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(&made[0]).join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["truncated_at_shutdown"], false);
    assert!(meta["asked_to"].is_null());
    let _ = std::fs::remove_dir_all(&dir);
}
