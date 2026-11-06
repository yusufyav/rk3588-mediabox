use super::*;
use std::sync::Condvar;
use std::sync::atomic::AtomicUsize;

static DIRS: AtomicUsize = AtomicUsize::new(0);

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("bintrace-{}-{}", std::process::id(), DIRS.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(cap: u64) -> Config {
    Config { min_free_bytes: 0, ..Config::files(cap) }
}

fn files(path: &Path) -> Vec<Vec<Record>> {
    (0..)
        .map(|i| file_name(path, i))
        .take_while(|f| f.exists())
        .map(|f| read(&std::fs::read(f).unwrap()).unwrap())
        .collect()
}

fn put(trace: &Trace, n: u64) {
    for i in 0..n {
        trace.now(1, 0, 0, i, 0, 0);
    }
}

/// An opener held until released: the writer stops in it, the channel fills.
fn gated() -> (Opener, Arc<(Mutex<bool>, Condvar)>) {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = Arc::clone(&gate);
    let open: Opener = Box::new(move |path: &Path| {
        let (lock, cv) = &*g;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        Ok(Box::new(std::fs::File::create(path)?) as Box<dyn Sink>)
    });
    (open, gate)
}

fn release(gate: &(Mutex<bool>, Condvar)) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

#[test]
fn a_record_reads_back_as_written() {
    let r = Record { kind: 3, id: 0x1FFF, n: 7, seq: 9, mono: 1, raw: 2, a: u64::MAX, b: 5, c: 6 };
    assert_eq!(Record::parse(&r.bytes()), r);
}

#[test]
fn a_file_without_the_version_2_header_is_not_read() {
    let data = Record { kind: 1, ..Record::default() }.bytes();
    assert_eq!(read(&data), Err(FormatError::NotVersion2));
    let old = Record { kind: KIND_HEADER, n: 1, a: MAGIC, ..Record::default() }.bytes();
    assert_eq!(read(&old), Err(FormatError::Version(1)));
    assert_eq!(read(&data[..10]), Err(FormatError::Short));
    let mut two = old.to_vec();
    two.extend_from_slice(&data[..20]);
    assert_eq!(read(&two), Err(FormatError::Truncated(RECORD + 20)));
}

#[test]
fn a_finished_trace_has_every_record_and_its_last_on_the_disk() {
    let d = dir();
    let path = d.join("t.bin");
    let trace = Trace::with(&path, config(1 << 20)).unwrap();
    put(&trace, 5000);
    let s = trace.finish(Duration::from_secs(5));
    assert!(s.finished);
    assert_eq!((s.given, s.written, s.dropped()), (5000, 5000, 0));
    assert_eq!(s.last_written, Some(4999));
    let f = files(&path);
    assert_eq!(f.len(), 1);
    let c = check(&f);
    assert_eq!((c.records, c.first, c.last, c.missing, c.out_of_order), (5000, Some(0), Some(4999), 0, 0));
    assert!(c.closed, "the footer of a finish names the last sequence");
    assert_eq!(c.loss, Some((0, 0, 0)));
    // The records are the ones put, in order.
    let values: Vec<u64> = f[0].iter().filter(|r| r.kind == 1).map(|r| r.a).collect();
    assert_eq!(values, (0..5000).collect::<Vec<_>>());
}

#[test]
fn a_full_channel_is_a_counted_hole_in_the_sequence() {
    let d = dir();
    let path = d.join("t.bin");
    let (open, gate) = gated();
    let trace = Trace::with(&path, Config { channel: 4, open, ..config(1 << 20) }).unwrap();
    put(&trace, 100);
    release(&gate);
    let s = trace.finish(Duration::from_secs(5));
    assert!(s.finished);
    assert_eq!(s.channel_full, 96);
    assert_eq!(s.given, s.written + s.dropped());
    let c = check(&files(&path));
    assert_eq!(c.records, 4);
    assert_eq!(c.missing, 96, "the lost records are a gap the file shows");
    assert_eq!(c.loss, Some((96, 0, 0)));
}

#[test]
fn a_record_after_the_finish_is_counted() {
    let d = dir();
    let trace = Trace::with(&d.join("t.bin"), config(1 << 20)).unwrap();
    put(&trace, 3);
    trace.finish(Duration::from_secs(5));
    put(&trace, 2);
    let s = trace.summary();
    assert_eq!((s.written, s.after_close), (3, 0), "the finish's answer stays");
    assert_eq!(trace.counters.after_close.load(Ordering::SeqCst), 2);
}

#[test]
fn files_rotate_without_a_hole_or_a_repeat() {
    let d = dir();
    let path = d.join("t.bin");
    // 10 records a file: a header, six records, the three closing ones.
    let trace = Trace::with(&path, config(10 * RECORD as u64)).unwrap();
    put(&trace, 25);
    let s = trace.finish(Duration::from_secs(5));
    assert!(s.finished);
    let f = files(&path);
    assert_eq!(f.len(), 5);
    assert!(f.iter().all(|r| r.len() <= 10));
    let c = check(&f);
    assert_eq!((c.records, c.missing, c.out_of_order, c.last), (25, 0, 0, Some(24)));
    assert!(c.closed);
    // Each file's header expects the sequence its first record has.
    for records in &f {
        let first = records.iter().find(|r| r.kind == 1).unwrap();
        assert_eq!(records[0].seq, first.seq);
    }
    let mut manifest = path.as_os_str().to_owned();
    manifest.push(".manifest");
    let lines = std::fs::read_to_string(PathBuf::from(manifest)).unwrap();
    assert_eq!(lines.lines().count(), 5);
    assert!(lines.lines().last().unwrap().contains("first=24 last=24 records=1"));
    assert!(lines.lines().last().unwrap().ends_with("end=finish"));
}

#[test]
fn past_the_quota_recording_stops_and_counts() {
    let d = dir();
    let path = d.join("t.bin");
    let trace = Trace::with(&path, Config { max_files: 2, ..config(10 * RECORD as u64) }).unwrap();
    put(&trace, 20);
    let s = trace.finish(Duration::from_secs(5));
    assert!(s.finished);
    assert_eq!((s.written, s.discarded, s.quota_stops), (12, 8, 1));
    assert_eq!(s.given, s.written + s.dropped());
    assert_eq!(files(&path).len(), 2);
}

#[test]
fn a_file_that_cannot_be_opened_loses_counted_records() {
    let d = dir();
    let trace = Trace::with(&d.join("no/such/dir/t.bin"), config(1 << 20)).unwrap();
    put(&trace, 7);
    let s = trace.finish(Duration::from_secs(5));
    assert!(s.finished);
    assert_eq!((s.open_errors, s.written, s.discarded), (1, 0, 7));
}

struct Failing(usize);
impl Write for Failing {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.0 < b.len() {
            return Err(std::io::Error::other("full"));
        }
        self.0 -= b.len();
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Sink for Failing {
    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_write_error_stops_recording_and_counts() {
    let d = dir();
    let open: Opener = Box::new(|_: &Path| Ok(Box::new(Failing(1000)) as Box<dyn Sink>));
    let trace = Trace::with(&d.join("t.bin"), Config { open, ..config(1 << 30) }).unwrap();
    // More than the 1 MiB buffer: it goes to the sink, which fails.
    put(&trace, 30_000);
    let s = trace.finish(Duration::from_secs(5));
    assert!(s.finished);
    assert_eq!(s.write_errors, 1);
    assert!(s.discarded > 0);
    assert_eq!(s.given, s.written + s.dropped());
}

#[test]
fn a_stuck_writer_does_not_hold_the_finish() {
    let d = dir();
    let (open, gate) = gated();
    let trace = Trace::with(&d.join("t.bin"), Config { open, ..config(1 << 20) }).unwrap();
    put(&trace, 3);
    let started = std::time::Instant::now();
    let s = trace.finish(Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!s.finished, "not done in time is said, not waited for");
    release(&gate);
}

#[test]
fn a_missing_record_or_footer_is_found() {
    let d = dir();
    let path = d.join("t.bin");
    let trace = Trace::with(&path, config(1 << 20)).unwrap();
    put(&trace, 10);
    trace.finish(Duration::from_secs(5));
    let mut f = files(&path);
    // The tail cut: no footer.
    let cut: Vec<Record> = f[0].iter().copied().filter(|r| r.kind != KIND_FOOTER).collect();
    assert!(!check(&[cut]).closed);
    // A record out of the middle.
    f[0].remove(5);
    let c = check(&f);
    assert_eq!(c.missing, 1);
    assert!(!c.closed || c.last == Some(9));
}
