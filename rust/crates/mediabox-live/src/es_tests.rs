//! The TS/PES reader against ISO/IEC 13818-1 (ITU-T H.222.0 08/2018): every
//! stream here is built from the standard's syntax, not copied from a
//! broadcast. Clause numbers are the standard's.
//!
//! The streams are run through `run`, which is the only place that knows the
//! reader's interface.

use std::collections::HashMap;

use super::*;
use crate::stamp::PACKET;

pub(crate) const PMT_PID: u16 = 0x1000;
pub(crate) const PCR_PID: u16 = 0x100;
pub(crate) const AUDIO: u16 = 0x101;
pub(crate) const PROGRAM: u16 = 1;

// ---- H.222.0 syntax ---------------------------------------------------------

/// CRC_32 of 2.4.4 (Annex A): zero remainder over a section with its CRC.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

/// A 33-bit PTS/DTS in its five bytes (2.4.3.6): the 4-bit prefix, then
/// 3, 15 and 15 bits each followed by a marker_bit.
pub(crate) fn timestamp(prefix: u8, t: u64) -> [u8; 5] {
    [
        (prefix << 4) | ((((t >> 30) & 7) as u8) << 1) | 1,
        (t >> 22) as u8,
        ((((t >> 15) & 0x7F) as u8) << 1) | 1,
        (t >> 7) as u8,
        (((t & 0x7F) as u8) << 1) | 1,
    ]
}

/// A PES packet with the optional header (2.4.3.6): `stuffing` 0xFF bytes in
/// the header, PES_packet_length the exact one unless `length` says another.
pub(crate) fn pes(stream_id: u8, pts: Option<u64>, dts: Option<u64>, stuffing: usize, payload: &[u8], length: Option<u16>) -> Vec<u8> {
    let mut optional = Vec::new();
    let flags = match (pts, dts) {
        (Some(_), Some(_)) => 0xC0,
        (Some(_), None) => 0x80,
        _ => 0x00,
    };
    if let Some(pts) = pts {
        optional.extend(timestamp(if dts.is_some() { 0b0011 } else { 0b0010 }, pts));
    }
    if let Some(dts) = dts {
        optional.extend(timestamp(0b0001, dts));
    }
    optional.extend(std::iter::repeat_n(0xFF, stuffing));
    let mut v = vec![0, 0, 1, stream_id, 0, 0, 0x80, flags, optional.len() as u8];
    v.extend(optional);
    v.extend_from_slice(payload);
    let length = length.unwrap_or((v.len() - 6) as u16);
    v[4..6].copy_from_slice(&length.to_be_bytes());
    v
}

/// A sound PES as broadcasters send it: MPEG audio stream_id, a PTS.
pub(crate) fn sound(pts: u64, payload: &[u8]) -> Vec<u8> {
    pes(0xC0, Some(pts), None, 0, payload, None)
}

/// One transport packet (2.4.3.2): the adaptation field when `af` is given
/// (its flags byte and the fields after it) or when the payload does not
/// fill the packet (stuffing, 2.4.3.5); adaptation-only for no payload.
pub(crate) fn packet(pid: u16, pusi: bool, cc: u8, af: Option<(u8, &[u8])>, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= 184);
    let mut p = vec![0x47, (u8::from(pusi) << 6) | (pid >> 8) as u8, pid as u8, 0];
    let afc = match (af.is_some() || payload.len() < 184, payload.is_empty()) {
        (_, true) => 0b10,
        (true, false) => 0b11,
        (false, false) => 0b01,
    };
    p[3] = (afc << 4) | (cc & 0x0F);
    if afc & 0b10 != 0 {
        let length = 183 - payload.len();
        p.push(length as u8);
        if length > 0 {
            let (flags, fields) = af.unwrap_or((0, &[]));
            assert!(fields.len() < length, "the flags byte and the fields fit");
            p.push(flags);
            p.extend_from_slice(fields);
            p.resize(5 + length, 0xFF);
        }
    }
    p.extend_from_slice(payload);
    assert_eq!(p.len(), PACKET);
    p
}

/// The PCR fields of an adaptation field (2.4.3.4): base, 6 reserved, ext.
pub(crate) fn pcr_field(pcr: u64) -> [u8; 6] {
    let (base, ext) = (pcr / 300, pcr % 300);
    [(base >> 25) as u8, (base >> 17) as u8, (base >> 9) as u8, (base >> 1) as u8, (((base & 1) << 7) as u8) | 0x7E | ((ext >> 8) as u8), ext as u8]
}

/// Continuity counters per PID, and units cut into packets.
#[derive(Default)]
pub(crate) struct Mux {
    cc: HashMap<u16, u8>,
}

impl Mux {
    pub(crate) fn next(&mut self, pid: u16) -> u8 {
        let cc = self.cc.entry(pid).or_insert(0);
        let now = *cc;
        *cc = (*cc + 1) & 0x0F;
        now
    }

    pub(crate) fn start_at(&mut self, pid: u16, cc: u8) {
        self.cc.insert(pid, cc & 0x0F);
    }

    /// A PES into packets, each with at most `sizes[i]` bytes of it (the
    /// last size repeats), the rest adaptation stuffing.
    pub(crate) fn pes(&mut self, pid: u16, unit: &[u8], sizes: &[usize]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut at = 0;
        let mut i = 0;
        while at < unit.len() {
            let size = sizes.get(i).or(sizes.last()).copied().unwrap_or(184).clamp(1, 184).min(unit.len() - at);
            let cc = self.next(pid);
            out.push(packet(pid, at == 0, cc, None, &unit[at..at + size]));
            at += size;
            i += 1;
        }
        out
    }

    /// A section (2.4.4) into packets: pointer_field 0, 0xFF after it.
    pub(crate) fn section(&mut self, pid: u16, section: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8];
        bytes.extend_from_slice(section);
        let mut out = Vec::new();
        for (n, piece) in bytes.chunks(184).enumerate() {
            let mut payload = piece.to_vec();
            payload.resize(184, 0xFF);
            let cc = self.next(pid);
            out.extend(packet(pid, n == 0, cc, None, &payload));
        }
        out
    }

    /// PAT and PMT of the one programme (2.4.4.3, 2.4.4.8).
    pub(crate) fn tables(&mut self, version: u8, pcr_pid: u16, streams: &[(u8, u16, Vec<u8>)]) -> Vec<u8> {
        let mut out = self.section(0, &pat(&[(PROGRAM, PMT_PID)]));
        out.extend(self.section(PMT_PID, &pmt(PROGRAM, version, pcr_pid, streams)));
        out
    }
}

pub(crate) fn finish_section(mut s: Vec<u8>) -> Vec<u8> {
    let length = s.len() - 3 + 4;
    s[1] = 0xB0 | ((length >> 8) as u8 & 0x0F);
    s[2] = length as u8;
    let crc = crc32(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

pub(crate) fn pat(programs: &[(u16, u16)]) -> Vec<u8> {
    let mut s = vec![0x00, 0, 0, 0x00, 0x01, 0xC1, 0, 0];
    for &(program, pid) in programs {
        s.extend_from_slice(&program.to_be_bytes());
        s.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    finish_section(s)
}

pub(crate) fn pmt(program: u16, version: u8, pcr_pid: u16, streams: &[(u8, u16, Vec<u8>)]) -> Vec<u8> {
    let mut s = vec![0x02, 0, 0];
    s.extend_from_slice(&program.to_be_bytes());
    s.push(0xC1 | (version & 0x1F) << 1);
    s.extend_from_slice(&[0, 0]);
    s.extend_from_slice(&(0xE000 | pcr_pid).to_be_bytes());
    s.extend_from_slice(&[0xF0, 0x00]);
    for (stream_type, pid, descriptors) in streams {
        s.push(*stream_type);
        s.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        s.extend_from_slice(&(0xF000 | descriptors.len() as u16).to_be_bytes());
        s.extend_from_slice(descriptors);
    }
    finish_section(s)
}

/// MPEG-1 Layer II (stream_type 0x04, 2.4.4.9 Table 2-34).
pub(crate) fn mp2() -> Vec<(u8, u16, Vec<u8>)> {
    vec![(0x04, AUDIO, vec![])]
}

pub(crate) fn flat(packets: Vec<Vec<u8>>) -> Vec<u8> {
    packets.concat()
}

pub(crate) fn payload(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

// ---- the reader, as the tests see it --------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct C {
    tei: u64,
    scrambled: u64,
    cc_loss: u64,
    duplicate: u64,
    duplicate_excess: u64,
    adaptation_bad: u64,
    pes_bad: u64,
    pes_foreign: u64,
    timestamp_bad: u64,
    discontinuity_announced: u64,
    timebase_discontinuity: u64,
    recovered: u64,
    pid_reset: u64,
    pmt_changes: u64,
    pcr_pid_changes: u64,
    psi_bad: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    pts: Option<u64>,
    dts: Option<u64>,
    data: Vec<u8>,
    /// Why it does not run on from the one before, if it does not.
    brk: Option<String>,
    timebase: bool,
    /// The PMT moved the sound here: "PID codec rebuild".
    switch: Option<String>,
}

#[derive(Debug, Default)]
struct Run {
    units: Vec<Unit>,
    counts: C,
    codec: Option<String>,
}

enum Step {
    Feed(Vec<u8>),
    Select(u16),
    /// The viewer's choice of audio stream, by its order (`Reader::choose`).
    Choose(usize),
    NewChannel,
}

fn run(steps: Vec<Step>) -> Run {
    let mut out = Run::default();
    let mut reader = Some(Reader::new());
    let mut counts = Counts::default();
    let take = |o: Output, out: &mut Run| {
        for chunk in o.chunks {
            if chunk.start || out.units.is_empty() {
                let brk = chunk.discont.map(|b| format!("{b:?}"));
                let switch = chunk.switch.map(|w| format!("{:#x} {} {}", w.pid, w.codec.name(), w.rebuild));
                out.units.push(Unit { pts: chunk.pts, dts: chunk.dts, data: chunk.data, brk, timebase: chunk.new_time_base, switch });
            } else {
                out.units.last_mut().unwrap().data.extend(chunk.data);
            }
        }
    };
    for step in steps {
        match step {
            Step::Select(pid) => {
                let r = reader.get_or_insert_with(Reader::new);
                let mut o = Output::default();
                r.select(pid, None, &mut o);
                take(o, &mut out);
            }
            Step::Choose(order) => {
                let r = reader.get_or_insert_with(Reader::new);
                let mut o = Output::default();
                r.choose(order, &mut o);
                take(o, &mut out);
            }
            Step::NewChannel => {
                if let Some(old) = reader.replace(Reader::new()) {
                    counts = counts.plus(&old.counts());
                }
            }
            Step::Feed(bytes) => {
                let r = reader.get_or_insert_with(Reader::new);
                let o = r.feed(&bytes);
                take(o, &mut out);
            }
        }
    }
    if let Some(r) = &reader {
        counts = counts.plus(&r.counts());
        out.codec = r.codec().map(|c| c.name().to_string());
    }
    let c = counts;
    out.counts = C {
        tei: c.tei,
        scrambled: c.scrambled,
        cc_loss: c.cc_loss,
        duplicate: c.duplicate,
        duplicate_excess: c.duplicate_excess,
        adaptation_bad: c.adaptation_bad,
        pes_bad: c.pes_bad,
        pes_foreign: c.pes_foreign,
        timestamp_bad: c.timestamp_bad,
        discontinuity_announced: c.discontinuity_announced,
        timebase_discontinuity: c.timebase_discontinuity,
        recovered: c.recovered,
        pid_reset: c.pid_reset,
        pmt_changes: c.pmt_changes,
        pcr_pid_changes: c.pcr_pid_changes,
        psi_bad: c.psi_bad,
    };
    out
}

/// Selected, tables, then `body`.
fn simple(mux: &mut Mux, body: Vec<u8>) -> Run {
    let mut all = mux.tables(0, PCR_PID, &mp2());
    all.extend(body);
    run(vec![Step::Select(AUDIO), Step::Feed(all)])
}

fn zero() -> C {
    C::default()
}

// ---- E 1-3: headers across packets ---------------------------------------

#[test]
fn e01_pes_header_across_two_packets() {
    let mut mux = Mux::default();
    let a = sound(90_000, &payload(300, 1));
    // 8 bytes in the first packet: the header (14) goes on into the next.
    let body = flat(mux.pes(AUDIO, &a, &[8, 184]));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 1, "{r:?}");
    assert_eq!(r.units[0].pts, Some(90_000));
    assert_eq!(r.units[0].data, payload(300, 1));
    assert_eq!(r.counts, zero());
}

#[test]
fn e02_pes_header_across_three_packets() {
    let mut mux = Mux::default();
    // 9 + 5 + 120 bytes of header in 10, 60 and then 184 bytes per packet.
    let a = pes(0xC0, Some(123_456), None, 120, &payload(400, 2), None);
    let body = flat(mux.pes(AUDIO, &a, &[10, 60, 184]));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 1, "{r:?}");
    assert_eq!(r.units[0].pts, Some(123_456));
    assert_eq!(r.units[0].data, payload(400, 2));
    assert_eq!(r.counts, zero());
}

#[test]
fn e03_pts_split_at_a_packet_boundary() {
    for split in 10..=13 {
        let mut mux = Mux::default();
        let a = pes(0xC0, Some(0x1_2345_6789), Some(0x1_2345_0000), 0, &payload(200, 3), None);
        let b = sound(0x1_2346_0000, &payload(50, 4));
        let mut body = flat(mux.pes(AUDIO, &a, &[split, 184]));
        body.extend(flat(mux.pes(AUDIO, &b, &[184])));
        let r = simple(&mut mux, body);
        assert_eq!(r.units.len(), 2, "split {split}: {r:?}");
        assert_eq!((r.units[0].pts, r.units[0].data.clone()), (Some(0x1_2345_6789), payload(200, 3)), "split {split}");
        assert_eq!(r.units[1].pts, Some(0x1_2346_0000));
        assert_eq!(r.counts, zero(), "split {split}");
    }
}

// ---- E 4: 33-bit wrap -----------------------------------------------------

#[test]
fn e04_pts_wraps_at_33_bits() {
    let mut mux = Mux::default();
    let last = (1u64 << 33) - 1;
    let mut body = flat(mux.pes(AUDIO, &sound(last, &payload(40, 5)), &[184]));
    body.extend(flat(mux.pes(AUDIO, &sound(2_160, &payload(40, 6)), &[184])));
    let r = simple(&mut mux, body);
    let pts: Vec<_> = r.units.iter().map(|u| u.pts).collect();
    assert_eq!(pts, vec![Some(last), Some(2_160)]);
    assert_eq!(r.counts, zero());
    // On the clock: a PCR 1 tick before the wrap, stamped at 10 s; a PTS
    // 2160 ticks after the wrap is 2161 ticks (24.011 ms) later.
    let pcr = ((1u64 << 33) - 1) * 300;
    let at = crate::engine::pts_on_clock(2_160, (10_000_000_000, pcr));
    assert_eq!(at, 10_000_000_000 + 2_161 * 1_000_000_000 / 90_000);
    let before = crate::engine::pts_on_clock(last - 90_000, (10_000_000_000, pcr));
    assert_eq!(before, 9_000_000_000);
}

// ---- E 5-8: continuity -----------------------------------------------------

#[test]
fn e05_adaptation_only_packet_announcing_a_discontinuity() {
    let mut mux = Mux::default();
    let mut body = flat(mux.pes(AUDIO, &sound(90_000, &payload(150, 7)), &[184]));
    // Adaptation only, discontinuity_indicator, a new counter (2.4.3.5).
    body.extend(packet(AUDIO, false, 9, Some((0x80, &[])), &[]));
    mux.start_at(AUDIO, 10);
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(150, 8)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 2, "{r:?}");
    assert_eq!(r.units[0].data, payload(150, 7));
    assert_eq!(r.units[1].data, payload(150, 8));
    assert_eq!(r.units[1].brk.as_deref(), Some("Announced"));
    assert_eq!(r.counts, C { discontinuity_announced: 1, ..zero() });
}

#[test]
fn e06_announced_continuity_jump() {
    let mut mux = Mux::default();
    let mut body = flat(mux.pes(AUDIO, &sound(90_000, &payload(150, 7)), &[184]));
    let b = sound(93_600, &payload(100, 9));
    // The next PES on counter 12, its packet flagged.
    body.extend(packet(AUDIO, true, 12, Some((0x80, &[])), &b));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 2, "{r:?}");
    assert_eq!(r.units[1].data, payload(100, 9));
    assert_eq!(r.units[1].brk.as_deref(), Some("Announced"));
    assert_eq!(r.counts, C { discontinuity_announced: 1, ..zero() });
}

#[test]
fn e07_lost_packet_without_indicator() {
    let mut mux = Mux::default();
    let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(500, 10)), &[184]);
    packets.remove(1);
    let mut body = flat(packets);
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(100, 11)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 2, "{r:?}");
    // What came before the loss, nothing after it.
    assert_eq!(r.units[0].data, payload(500, 10)[..184 - 14].to_vec());
    assert_eq!(r.units[1].brk.as_deref(), Some("Loss"));
    assert_eq!(r.units[1].data, payload(100, 11));
    assert_eq!(r.counts, C { cc_loss: 1, recovered: 1, ..zero() });
}

#[test]
fn e08_counter_wraps_15_to_0() {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &mp2());
    mux.start_at(AUDIO, 10);
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(3_000, 12)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(r.units.len(), 1);
    assert_eq!(r.units[0].data, payload(3_000, 12));
    assert_eq!(r.counts, zero());
}

// ---- E 9-11: duplicates (2.4.3.3) -----------------------------------------

#[test]
fn e09_one_duplicate_packet() {
    let mut mux = Mux::default();
    let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 13)), &[184]);
    packets.insert(2, packets[1].clone());
    let r = simple(&mut mux, flat(packets));
    assert_eq!(r.units.len(), 1);
    assert_eq!(r.units[0].data, payload(600, 13));
    assert_eq!(r.counts, C { duplicate: 1, ..zero() });
}

#[test]
fn e10_a_third_copy_is_an_error() {
    let mut mux = Mux::default();
    let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 13)), &[184]);
    packets.insert(2, packets[1].clone());
    packets.insert(2, packets[1].clone());
    let r = simple(&mut mux, flat(packets));
    assert_eq!(r.units[0].data, payload(600, 13));
    assert_eq!(r.counts, C { duplicate: 1, duplicate_excess: 1, ..zero() });
}

#[test]
fn e11_same_counter_other_content() {
    let mut mux = Mux::default();
    let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 13)), &[184]);
    let mut other = packets[1].clone();
    other[100] ^= 0x55;
    packets.insert(2, other);
    let mut body = flat(packets);
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 14)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 2, "{r:?}");
    // Up to the packet the copy disagrees with, then nothing of that PES.
    assert_eq!(r.units[0].data, payload(600, 13)[..170 + 184].to_vec());
    assert_eq!(r.units[1].brk.as_deref(), Some("Loss"));
    assert_eq!(r.counts, C { cc_loss: 1, recovered: 1, ..zero() });
}

// ---- E 12-15: the packet header and adaptation field -----------------------

#[test]
fn e12_transport_error() {
    let mut mux = Mux::default();
    let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 15)), &[184]);
    packets[2][1] |= 0x80;
    let mut body = flat(packets);
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 16)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 2, "{r:?}");
    assert_eq!(r.units[0].data, payload(600, 15)[..170 + 184].to_vec());
    assert_eq!(r.units[1].brk.as_deref(), Some("TransportError"));
    assert_eq!(r.counts, C { tei: 1, recovered: 1, ..zero() });
}

#[test]
fn e13_adaptation_field_control_00_is_discarded() {
    let mut mux = Mux::default();
    let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 17)), &[184]);
    // Reserved '00' (2.4.3.3): discarded, its counter not counted.
    let mut reserved = packets[1].clone();
    reserved[3] &= 0xCF;
    packets.insert(1, reserved);
    let r = simple(&mut mux, flat(packets));
    assert_eq!(r.units.len(), 1);
    assert_eq!(r.units[0].data, payload(600, 17));
    assert_eq!(r.counts, C { adaptation_bad: 1, ..zero() });
}

#[test]
fn e14_adaptation_field_length_out_of_range() {
    for (afc, length) in [(0b11u8, 183u8), (0b11, 200), (0b10, 100)] {
        let mut mux = Mux::default();
        let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 18)), &[184]);
        let p = &mut packets[1];
        p[3] = (p[3] & 0xCF) | (afc << 4);
        p[4] = length;
        let mut body = flat(packets);
        body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 19)), &[184])));
        let r = simple(&mut mux, body);
        assert_eq!(r.units.len(), 2, "afc {afc:02b} length {length}: {r:?}");
        assert_eq!(r.units[0].data, payload(600, 18)[..170].to_vec(), "afc {afc:02b} length {length}");
        assert_eq!(r.units[1].brk.as_deref(), Some("MalformedAdaptation"));
        // Read as adaptation only, the packet does not count on the
        // counter (2.4.3.3): the next one shows the payload missing.
        let cc_loss = u64::from(afc == 0b10);
        assert_eq!(r.counts, C { adaptation_bad: 1, cc_loss, recovered: 1, ..zero() }, "afc {afc:02b} length {length}");
    }
}

#[test]
fn e15_scrambled_payload_never_reaches_the_parser() {
    // In the transport packet (2.4.3.3) and in the PES header (2.4.3.7).
    for pes_level in [false, true] {
        let mut mux = Mux::default();
        let mut packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 20)), &[184]);
        if pes_level {
            // PES_scrambling_control '10' in the first header.
            packets[0][4 + 6] = 0x80 | 0x20;
        } else {
            packets[2][3] |= 0x80;
        }
        let mut body = flat(packets);
        body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 21)), &[184])));
        let r = simple(&mut mux, body);
        let expected = if pes_level { vec![] } else { payload(600, 20)[..170 + 184].to_vec() };
        let first = r.units.iter().find(|u| u.pts == Some(90_000)).map(|u| u.data.clone()).unwrap_or_default();
        assert_eq!(first, expected, "pes level {pes_level}");
        let last = r.units.last().unwrap();
        assert_eq!((last.pts, last.brk.as_deref()), (Some(93_600), Some("Scrambled")), "pes level {pes_level}");
        assert_eq!(r.counts, C { scrambled: 1, recovered: 1, ..zero() }, "pes level {pes_level}");
    }
}

// ---- E 16-18: the PES header ----------------------------------------------

#[test]
fn e16_pts_dts_flags_01_is_forbidden() {
    let mut mux = Mux::default();
    let mut a = sound(90_000, &payload(100, 22));
    a[7] = 0x40;
    let mut body = flat(mux.pes(AUDIO, &a, &[184]));
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 23)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 1, "{r:?}");
    assert_eq!(r.units[0].pts, Some(93_600));
    assert_eq!(r.units[0].brk.as_deref(), Some("MalformedPes"));
    assert_eq!(r.counts, C { timestamp_bad: 1, recovered: 1, ..zero() });
}

#[test]
fn e17_pts_marker_bits() {
    for byte in [9usize, 11, 13] {
        let mut mux = Mux::default();
        let mut a = sound(90_000, &payload(100, 24));
        a[byte] &= 0xFE;
        let mut body = flat(mux.pes(AUDIO, &a, &[184]));
        body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 25)), &[184])));
        let r = simple(&mut mux, body);
        assert_eq!(r.units.len(), 1, "marker in byte {byte}: {r:?}");
        assert_eq!(r.units[0].pts, Some(93_600));
        assert_eq!(r.counts, C { timestamp_bad: 1, recovered: 1, ..zero() }, "marker in byte {byte}");
    }
}

#[test]
fn e18_pes_packet_length_short_long_and_impossible() {
    // Short: says 300 bytes, the next PES starts after 100.
    let mut mux = Mux::default();
    let mut body = flat(mux.pes(AUDIO, &pes(0xC0, Some(90_000), None, 0, &payload(100, 26), Some(308)), &[184]));
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 27)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 2, "short: {r:?}");
    assert_eq!(r.units[1].brk.as_deref(), Some("MalformedPes"), "short");
    assert_eq!(r.counts, C { pes_bad: 1, recovered: 1, ..zero() }, "short");

    // Long: says 20 bytes of payload, 100 follow; only 20 are the PES's.
    let mut mux = Mux::default();
    let mut body = flat(mux.pes(AUDIO, &pes(0xC0, Some(90_000), None, 0, &payload(100, 28), Some(8 + 20)), &[184]));
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 29)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units[0].data, payload(100, 28)[..20].to_vec(), "long");
    assert_eq!(r.units[1].brk.as_deref(), Some("MalformedPes"), "long");
    assert_eq!(r.counts, C { pes_bad: 1, recovered: 1, ..zero() }, "long");

    // Impossible: shorter than its own header.
    let mut mux = Mux::default();
    let mut body = flat(mux.pes(AUDIO, &pes(0xC0, Some(90_000), None, 0, &payload(100, 30), Some(5)), &[184]));
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 31)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 1, "impossible: {r:?}");
    assert_eq!(r.counts, C { pes_bad: 1, recovered: 1, ..zero() }, "impossible");

    // Zero: allowed for video only (2.4.3.7); counted, and read to the next.
    let mut mux = Mux::default();
    let mut body = flat(mux.pes(AUDIO, &pes(0xC0, Some(90_000), None, 0, &payload(100, 32), Some(0)), &[184]));
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 33)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units[0].data, payload(100, 32), "zero");
    assert_eq!(r.counts, C { pes_bad: 1, ..zero() }, "zero");
}

#[test]
fn stream_ids_without_the_optional_header_and_foreign_ones() {
    let mut mux = Mux::default();
    // padding_stream (0xBE): bytes from byte 6, dropped without a word.
    let mut padding = vec![0, 0, 1, 0xBE, 0, 50];
    padding.extend(std::iter::repeat_n(0xFF, 50));
    let mut body = flat(mux.pes(AUDIO, &padding, &[184]));
    body.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(10, 34)), &[184])));
    // private_stream_2 (0xBF) and a video stream_id on the sound's PID.
    let mut private = vec![0, 0, 1, 0xBF, 0, 20];
    private.extend(payload(20, 35));
    body.extend(flat(mux.pes(AUDIO, &private, &[184])));
    body.extend(flat(mux.pes(AUDIO, &pes(0xE0, Some(1), None, 0, &payload(20, 36), None), &[184])));
    // Not a PES stream_id at all (below 0xBC).
    body.extend(flat(mux.pes(AUDIO, &pes(0xB9, Some(2), None, 0, &payload(20, 37), None), &[184])));
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 38)), &[184])));
    let r = simple(&mut mux, body);
    let pts: Vec<_> = r.units.iter().map(|u| u.pts).collect();
    assert_eq!(pts, vec![Some(90_000), Some(93_600)], "{r:?}");
    assert_eq!(r.counts, C { pes_foreign: 2, pes_bad: 1, recovered: 1, ..zero() });
}

// ---- E 19-23: the sound codecs (2.4.4.9 Table 2-34, EN 300 468 Annex D) ----

/// One frame-like payload per codec, cut into packets at odd sizes: what
/// comes out is the elementary stream byte for byte, and the codec is the
/// one the PMT names.
fn codec_case(stream_type: u8, descriptors: Vec<u8>, stream_id: u8, frame: &[u8], codec: &str) {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &[(stream_type, AUDIO, descriptors)]);
    let mut es = Vec::new();
    for k in 0..6u64 {
        let mut payload = Vec::new();
        for _ in 0..3 {
            payload.extend_from_slice(frame);
        }
        es.push(payload.clone());
        let unit = pes(stream_id, Some(90_000 + k * 6_480), None, (k * 7) as usize % 40, &payload, None);
        all.extend(flat(mux.pes(AUDIO, &unit, &[7 + k as usize * 13, 101, 184, 33])));
    }
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    let data: Vec<_> = r.units.iter().map(|u| u.data.clone()).collect();
    assert_eq!(data, es, "{codec}");
    assert_eq!(r.codec.as_deref(), Some(codec), "{codec}");
    assert_eq!(r.counts, zero(), "{codec}");
}

fn frame(sync: &[u8], n: usize) -> Vec<u8> {
    let mut f = sync.to_vec();
    f.extend(payload(n - sync.len(), 0x5A));
    f
}

#[test]
fn e19_mpeg1_layer2() {
    codec_case(0x03, vec![], 0xC0, &frame(&[0xFF, 0xFD, 0x94, 0x04], 576), "mpeg-audio");
}

#[test]
fn e20_aac_adts() {
    codec_case(0x0F, vec![], 0xC0, &frame(&[0xFF, 0xF1, 0x50, 0x80, 0x2E, 0x7F, 0xFC], 371), "aac-adts");
}

#[test]
fn e21_aac_latm() {
    codec_case(0x11, vec![], 0xC0, &frame(&[0x56, 0xE1, 0x70], 380), "aac-latm");
}

#[test]
fn e22_ac3() {
    codec_case(0x06, vec![0x6A, 0x01, 0x00], 0xBD, &frame(&[0x0B, 0x77, 0x1A, 0x2B, 0x14, 0x40], 768), "ac3");
}

#[test]
fn e23_eac3() {
    codec_case(0x06, vec![0x7A, 0x01, 0x00], 0xBD, &frame(&[0x0B, 0x77, 0x02, 0xFF, 0x3F, 0x86], 512), "eac3");
}

// ---- E 24-27: tables, channels, recovery ----------------------------------

/// (pts, break, switch) of each PES given.
fn shape(r: &Run) -> Vec<(Option<u64>, Option<String>, Option<String>)> {
    r.units.iter().map(|u| (u.pts, u.brk.clone(), u.switch.clone())).collect()
}

fn some(text: &str) -> Option<String> {
    Some(text.to_string())
}

#[test]
fn e24_pmt_version_moves_the_sound_to_another_pid() {
    // Chosen once, at the start, as the demultiplexer's pad names it; the
    // PMT moves it and the reader follows by itself.
    let mut mux = Mux::default();
    let mut first = mux.tables(0, PCR_PID, &mp2());
    let a = sound(90_000, &payload(600, 40));
    let packets = mux.pes(AUDIO, &a, &[184]);
    first.extend(flat(packets[..2].to_vec()));
    let mut second = mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x04, 0x102, vec![])]));
    // The rest of 0x101's PES, and a whole new one on it: not the sound now.
    second.extend(flat(packets[2..].to_vec()));
    second.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 41)), &[184])));
    second.extend(flat(mux.pes(0x102, &sound(97_200, &payload(50, 42)), &[184])));
    second.extend(flat(mux.pes(0x102, &sound(100_800, &payload(50, 43)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(first), Step::Feed(second)]);
    assert_eq!(
        shape(&r),
        vec![(Some(90_000), some("Start"), None), (Some(97_200), some("Reset"), some("0x102 mpeg-audio false")), (Some(100_800), None, None)],
        "{r:?}"
    );
    assert_eq!(r.units[0].data, payload(600, 40)[..170 + 184].to_vec(), "not a byte of 0x101 after the move");
    assert_eq!(r.units[1].data, payload(50, 42));
    assert_eq!(r.counts, C { pid_reset: 1, pmt_changes: 1, ..zero() });
}

/// The reader ahead of the demultiplexer: the PMT changed before the
/// branch was built, from the PMT before it. The first PES says so: the
/// branch is built again for the codec in force.
#[test]
fn a_branch_built_from_an_older_pmt_is_corrected_at_once() {
    for (after, expected) in [
        (vec![(0x06, AUDIO, vec![0x6A, 1, 0])], "0x101 ac3 true"),
        (vec![(0x04, 0x102, vec![])], "0x102 mpeg-audio false"),
    ] {
        let mut mux = Mux::default();
        let mut all = mux.tables(0, PCR_PID, &mp2());
        all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &after)));
        let (pid, id) = if expected.starts_with("0x101") { (AUDIO, 0xBD) } else { (0x102, 0xC0) };
        all.extend(flat(mux.pes(pid, &pes(id, Some(90_000), None, 0, &payload(50, 72), None), &[184])));
        let mut reader = Reader::new();
        let mut out = Output::default();
        let fed = reader.feed(&all);
        assert!(fed.chunks.is_empty());
        reader.select(AUDIO, Some(Codec::MpegAudio), &mut out);
        let fed = reader.feed(&flat(mux.pes(pid, &pes(id, Some(93_600), None, 0, &payload(50, 73), None), &[184])));
        let first = &fed.chunks[0];
        let switch = first.switch.map(|w| format!("{:#x} {} {}", w.pid, w.codec.name(), w.rebuild));
        assert_eq!((first.pts, switch.as_deref(), first.discont), (Some(93_600), Some(expected), Some(Break::Start)), "{expected}");
    }
}

#[test]
fn pmt_gives_the_same_pid_another_codec() {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &mp2());
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 44)), &[184])));
    let half = mux.pes(AUDIO, &sound(91_000, &payload(400, 45)), &[184]);
    all.extend(flat(half[..1].to_vec()));
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x06, AUDIO, vec![0x6A, 1, 0])])));
    // The old codec's PES cut by the PMT: nothing more of it.
    all.extend(flat(half[1..].to_vec()));
    all.extend(flat(mux.pes(AUDIO, &pes(0xBD, Some(93_600), None, 0, &payload(50, 46), None), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(
        shape(&r),
        vec![(Some(90_000), some("Start"), None), (Some(91_000), None, None), (Some(93_600), some("Reset"), some("0x101 ac3 true"))],
        "{r:?}"
    );
    assert_eq!(r.units[1].data, payload(400, 45)[..170].to_vec());
    assert_eq!(r.codec.as_deref(), Some("ac3"));
    assert_eq!(r.counts, C { pid_reset: 1, pmt_changes: 1, ..zero() });
}

#[test]
fn pmt_moves_the_sound_to_another_pid_and_codec() {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &mp2());
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 47)), &[184])));
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x06, 0x103, vec![0x7A, 1, 0])])));
    all.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 48)), &[184])));
    all.extend(flat(mux.pes(0x103, &pes(0xBD, Some(97_200), None, 0, &payload(50, 49), None), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(shape(&r), vec![(Some(90_000), some("Start"), None), (Some(97_200), some("Reset"), some("0x103 eac3 true"))], "{r:?}");
    assert_eq!(r.counts, C { pid_reset: 1, pmt_changes: 1, ..zero() });
}

#[test]
fn the_sound_is_followed_by_its_language() {
    let tur = vec![0x0A, 4, b't', b'u', b'r', 0];
    let eng = vec![0x0A, 4, b'e', b'n', b'g', 0];
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &[(0x04, AUDIO, tur.clone()), (0x04, 0x102, eng.clone())]);
    all.extend(flat(mux.pes(0x102, &sound(90_000, &payload(50, 50)), &[184])));
    // Both PIDs renumbered, English now first: English is followed.
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x04, 0x202, eng), (0x04, 0x201, tur)])));
    all.extend(flat(mux.pes(0x201, &sound(93_600, &payload(50, 51)), &[184])));
    all.extend(flat(mux.pes(0x202, &sound(93_600, &payload(50, 52)), &[184])));
    let r = run(vec![Step::Select(0x102), Step::Feed(all)]);
    assert_eq!(shape(&r), vec![(Some(90_000), some("Start"), None), (Some(93_600), some("Reset"), some("0x202 mpeg-audio false"))], "{r:?}");
    assert_eq!(r.units[1].data, payload(50, 52));
}

#[test]
fn a_pmt_for_a_moment_empty_holds_the_sound_and_lets_it_go_on() {
    // 2.4.3.5: a PMT with no streams, then the whole one again.
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &mp2());
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 53)), &[184])));
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[])));
    all.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 54)), &[184])));
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 2, PCR_PID, &mp2())));
    all.extend(flat(mux.pes(AUDIO, &sound(97_200, &payload(50, 55)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(shape(&r), vec![(Some(90_000), some("Start"), None), (Some(97_200), some("Reset"), None)], "{r:?}");
    assert_eq!(r.counts, C { pid_reset: 2, pmt_changes: 2, ..zero() });
}

#[test]
fn pmt_in_two_packets_and_a_bad_crc() {
    let mut mux = Mux::default();
    let mut all = mux.section(0, &pat(&[(PROGRAM, PMT_PID)]));
    // A PMT whose descriptors take it past one packet.
    let long = vec![(0x04, AUDIO, [0x0A, 4, b't', b'u', b'r', 0].repeat(40))];
    let section = pmt(PROGRAM, 0, PCR_PID, &long);
    assert!(section.len() > 184);
    // First a copy with its CRC broken: not taken.
    let mut broken = pmt(PROGRAM, 0, PCR_PID, &[(0x06, AUDIO, vec![0x6A, 1, 0])]);
    let end = broken.len() - 1;
    broken[end] ^= 1;
    all.extend(mux.section(PMT_PID, &broken));
    all.extend(mux.section(PMT_PID, &section));
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 49)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(r.units.len(), 1, "{r:?}");
    assert_eq!(r.codec.as_deref(), Some("mpeg-audio"));
    assert_eq!(r.counts, C { psi_bad: 1, ..zero() });
}

/// A section with its current_next_indicator set to `current`, CRC again.
fn with_current_next(mut section: Vec<u8>, current: bool) -> Vec<u8> {
    section.truncate(section.len() - 4);
    section[5] = (section[5] & 0xFE) | u8::from(current);
    let crc = crc32(&section);
    section.extend_from_slice(&crc.to_be_bytes());
    section
}

/// One PAT section: `number` of `last`, version `version`.
fn pat_section(version: u8, number: u8, last: u8, programs: &[(u16, u16)]) -> Vec<u8> {
    let mut s = pat(programs);
    s.truncate(s.len() - 4);
    s[5] = 0xC1 | (version & 0x1F) << 1;
    s[6] = number;
    s[7] = last;
    let crc = crc32(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

#[test]
fn psi_duplicate_and_a_copy_that_is_not_one() {
    let mut mux = Mux::default();
    let mut all = mux.section(0, &pat(&[(PROGRAM, PMT_PID)]));
    // A PMT packet sent twice, byte for byte: one PMT (2.4.3.3).
    let first = mux.section(PMT_PID, &pmt(PROGRAM, 0, PCR_PID, &mp2()));
    all.extend(&first);
    all.extend(&first);
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 60)), &[184])));
    // Version 1 over two packets; its first packet's counter repeated with
    // other bytes: the section is thrown away, version 1 not taken.
    let long = vec![(0x06, AUDIO, [vec![0x6A, 1, 0], [0x0A, 4, b't', b'u', b'r', 0].repeat(40)].concat())];
    let v1 = mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &long));
    assert_eq!(v1.len(), 2 * PACKET);
    let mut other = v1[..PACKET].to_vec();
    other[40] ^= 0xFF;
    all.extend(&v1[..PACKET]);
    all.extend(&other);
    all.extend(&v1[PACKET..]);
    all.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 61)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(r.codec.as_deref(), Some("mpeg-audio"), "version 1 was not taken: {r:?}");
    assert_eq!(r.units.len(), 2);
    assert_eq!(r.counts, C { duplicate: 1, cc_loss: 1, ..zero() });
}

#[test]
fn a_new_pat_is_put_in_force_only_whole() {
    let mut mux = Mux::default();
    // Version 31 in two sections: programme 7 in section 0, ours in 1.
    let mut all = mux.section(0, &pat_section(31, 0, 1, &[(7, 0x1100)]));
    all.extend(mux.section(0, &pat_section(31, 1, 1, &[(PROGRAM, PMT_PID)])));
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 0, PCR_PID, &mp2())));
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 62)), &[184])));
    // Version 0 (after 31), section 1 only, twice: ours to PMT PID 0x1001.
    // Not whole: the PAT in force stays; a PMT on 0x1001 is nobody's.
    all.extend(mux.section(0, &pat_section(0, 1, 1, &[(PROGRAM, 0x1001)])));
    all.extend(mux.section(0, &pat_section(0, 1, 1, &[(PROGRAM, 0x1001)])));
    let ac3 = [(0x06, AUDIO, vec![0x6A, 1, 0])];
    all.extend(mux.section(0x1001, &pmt(PROGRAM, 3, PCR_PID, &ac3)));
    all.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 63)), &[184])));
    // The old mapping still works: a new version on PMT_PID is taken.
    let r0 = {
        let mut probe = all.clone();
        probe.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &mp2())));
        run(vec![Step::Select(AUDIO), Step::Feed(probe)])
    };
    assert_eq!(r0.counts.pmt_changes, 1, "the PAT in force stayed: {r0:?}");
    // Section 0 comes last: version 0 whole, and 0x1001 is our PMT now.
    all.extend(mux.section(0, &pat_section(0, 0, 1, &[(7, 0x1100)])));
    all.extend(mux.section(0x1001, &pmt(PROGRAM, 3, PCR_PID, &ac3)));
    all.extend(flat(mux.pes(AUDIO, &pes(0xBD, Some(97_200), None, 0, &payload(50, 64), None), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(shape(&r), vec![(Some(90_000), some("Start"), None), (Some(93_600), None, None), (Some(97_200), some("Reset"), some("0x101 ac3 true"))], "{r:?}");
    assert_eq!(r.counts, C { pid_reset: 1, ..zero() });
}

#[test]
fn a_pmt_on_a_pid_the_pat_gives_another_programme_is_refused() {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &mp2());
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 65)), &[184])));
    // program_number 2 on the PID the PAT gives programme 1 (2.4.4.6).
    all.extend(mux.section(PMT_PID, &pmt(2, 1, PCR_PID, &[(0x06, AUDIO, vec![0x6A, 1, 0])])));
    all.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 66)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(r.codec.as_deref(), Some("mpeg-audio"));
    assert_eq!(r.units.len(), 2);
    assert_eq!(r.counts, C { psi_bad: 1, ..zero() });
}

#[test]
fn a_descriptor_loop_that_does_not_add_up_names_no_codec() {
    // ES_info_length 3, the descriptor in it claims 5; then one cut short.
    for broken in [vec![0x6A, 5, 0], vec![0x6A]] {
        let mut mux = Mux::default();
        let mut all = mux.tables(0, PCR_PID, &mp2());
        all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 67)), &[184])));
        all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x06, AUDIO, broken.clone())])));
        all.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 68)), &[184])));
        let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
        assert_eq!(r.codec.as_deref(), Some("mpeg-audio"), "{broken:?}");
        assert_eq!(r.units.len(), 2, "{broken:?}");
        assert_eq!(r.counts, C { psi_bad: 1, ..zero() }, "{broken:?}");
    }
}

#[test]
fn tables_not_yet_applicable_are_not_applied() {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &mp2());
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 69)), &[184])));
    // current_next_indicator 0 (2.4.4.9): announced, not in force.
    let next = pmt(PROGRAM, 1, PCR_PID, &[(0x04, 0x102, vec![])]);
    all.extend(mux.section(PMT_PID, &with_current_next(next.clone(), false)));
    all.extend(mux.section(0, &with_current_next(pat(&[(PROGRAM, 0x1001)]), false)));
    all.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 70)), &[184])));
    // Then in force.
    all.extend(mux.section(PMT_PID, &next));
    all.extend(flat(mux.pes(0x102, &sound(97_200, &payload(50, 71)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(
        shape(&r),
        vec![(Some(90_000), some("Start"), None), (Some(93_600), None, None), (Some(97_200), some("Reset"), some("0x102 mpeg-audio false"))],
        "{r:?}"
    );
    assert_eq!(r.counts, C { pid_reset: 1, pmt_changes: 1, ..zero() });
}

#[test]
fn pat_in_two_sections() {
    // 2.4.4.5: section 0 names another programme, section 1 ours.
    let mut mux = Mux::default();
    let section = |number: u8, programs: &[(u16, u16)]| {
        let mut s = pat(programs);
        s[6] = number;
        s[7] = 1;
        s.truncate(s.len() - 4);
        let crc = crc32(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    };
    let mut all = mux.section(0, &section(0, &[(7, 0x1100)]));
    all.extend(mux.section(0, &section(1, &[(PROGRAM, PMT_PID)])));
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 0, PCR_PID, &[(0x0F, AUDIO, vec![])])));
    // Section 0 again: section 1's programme stays.
    all.extend(mux.section(0, &section(0, &[(7, 0x1100)])));
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x0F, AUDIO, vec![])])));
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 60)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(r.units.len(), 1, "{r:?}");
    assert_eq!(r.codec.as_deref(), Some("aac-adts"));
    assert_eq!(r.counts, C { pmt_changes: 1, ..zero() });
}

#[test]
fn e25_pcr_pid_change_and_time_base_discontinuity() {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &mp2());
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 50)), &[184])));
    // The PCR moves to 0x103.
    all.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, 0x103, &mp2())));
    // An indicator on the old PCR PID is not this programme's clock now.
    let cc = mux.next(PCR_PID);
    all.extend(packet(PCR_PID, false, cc, Some((0x90, &pcr_field(1_000))), &[]));
    // A new time base on 0x103 (2.4.3.5): the PES after it is on it.
    let cc = mux.next(0x103);
    all.extend(packet(0x103, false, cc, Some((0x90, &pcr_field(5_000_000_000))), &[]));
    all.extend(flat(mux.pes(AUDIO, &sound(20_000_000, &payload(50, 51)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all)]);
    assert_eq!(r.units.len(), 2, "{r:?}");
    assert!(!r.units[0].timebase && r.units[1].timebase, "{r:?}");
    // The sound's own bytes run on: the parser is not started again.
    assert_eq!(r.units[1].brk, None);
    assert_eq!(r.counts, C { pmt_changes: 1, pcr_pid_changes: 1, timebase_discontinuity: 1, ..zero() });
}

#[test]
fn e26_half_a_pes_does_not_cross_a_channel_change() {
    let mut mux = Mux::default();
    let mut first = mux.tables(0, PCR_PID, &mp2());
    let packets = mux.pes(AUDIO, &sound(90_000, &payload(600, 52)), &[184]);
    first.extend(flat(packets[..2].to_vec()));
    // The next channel uses the same PID; the rest of the old PES arrives
    // first, then its own.
    let mut mux2 = Mux::default();
    let mut second = flat(packets[2..].to_vec());
    second.extend(mux2.tables(0, PCR_PID, &mp2()));
    second.extend(flat(mux2.pes(AUDIO, &sound(5_000, &payload(80, 53)), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(first), Step::NewChannel, Step::Select(AUDIO), Step::Feed(second)]);
    assert_eq!(r.units.len(), 2, "{r:?}");
    assert_eq!(r.units[0].data, payload(600, 52)[..170 + 184].to_vec());
    assert_eq!((r.units[1].pts, r.units[1].data.clone()), (Some(5_000), payload(80, 53)));
    // The old channel's packets and the new one's are one PID with two
    // counters: the parser starts afresh either way.
    assert!(r.units[1].brk.is_some(), "{r:?}");

}

#[test]
fn e27_recovery_at_the_next_access_point() {
    let mut mux = Mux::default();
    let mut bad = sound(90_000, &payload(100, 57));
    bad[2] = 2; // 00 00 02: not a start code
    let mut body = flat(mux.pes(AUDIO, &bad, &[184]));
    // Continuation packets of the broken PES are not taken either.
    let more = mux.pes(AUDIO, &sound(91_000, &payload(400, 58)), &[184]);
    body.extend(flat(more[1..].to_vec()));
    body.extend(flat(mux.pes(AUDIO, &sound(93_600, &payload(10, 59)), &[184])));
    let r = simple(&mut mux, body);
    assert_eq!(r.units.len(), 1, "{r:?}");
    assert_eq!((r.units[0].pts, r.units[0].brk.as_deref()), (Some(93_600), Some("MalformedPes")));
    assert_eq!(r.counts, C { pes_bad: 1, cc_loss: 1, recovered: 1, ..zero() });
}

// ---- deterministic property tests -----------------------------------------

/// xorshift64*: the same numbers on every run.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A run of PES, header stuffing and packet sizes at random; the PES by PTS.
fn random_stream(rng: &mut Rng, mux: &mut Mux) -> (Vec<Vec<u8>>, HashMap<u64, Vec<u8>>) {
    let mut packets = Vec::new();
    let mut by_pts = HashMap::new();
    for k in 0..40u64 {
        let size = 1 + rng.below(700) as usize;
        let data = payload(size, k as u8);
        let pts = 90_000 + k * 2_160;
        let stuffing = rng.below(32) as usize;
        let sizes: Vec<usize> = (0..8).map(|_| 1 + rng.below(184) as usize).collect();
        let unit = pes(0xC0, Some(pts), None, stuffing, &data, None);
        packets.extend(mux.pes(AUDIO, &unit, &sizes));
        by_pts.insert(pts, data);
    }
    (packets, by_pts)
}

fn feed_in_batches(rng: &mut Rng, mux: &mut Mux, packets: Vec<Vec<u8>>) -> Run {
    let mut steps = vec![Step::Select(AUDIO), Step::Feed(mux.tables(0, PCR_PID, &mp2()))];
    let mut at = 0;
    while at < packets.len() {
        let n = (1 + rng.below(9) as usize).min(packets.len() - at);
        steps.push(Step::Feed(packets[at..at + n].concat()));
        at += n;
    }
    run(steps)
}

#[test]
fn property_clean_streams_come_out_whole() {
    let mut rng = Rng(0x5EED_0001);
    for _ in 0..50 {
        let mut mux = Mux::default();
        let (packets, by_pts) = random_stream(&mut rng, &mut mux);
        let r = feed_in_batches(&mut rng, &mut mux, packets);
        assert_eq!(r.units.len(), by_pts.len());
        for (i, unit) in r.units.iter().enumerate() {
            assert_eq!(&unit.data, &by_pts[&unit.pts.unwrap()]);
            assert_eq!(unit.brk.as_deref(), if i == 0 { Some("Start") } else { None });
        }
        assert_eq!(r.counts, zero());
    }
}

#[test]
fn property_damage_never_gives_wrong_bytes() {
    let mut rng = Rng(0x5EED_0002);
    for round in 0..400 {
        let mut mux = Mux::default();
        let (mut packets, by_pts) = random_stream(&mut rng, &mut mux);
        let kind = round % 5;
        for _ in 0..1 + rng.below(4) {
            let i = 1 + rng.below(packets.len() as u64 - 2) as usize;
            match kind {
                0 => {
                    packets.remove(i);
                }
                1 => {
                    let copy = packets[i].clone();
                    packets.insert(i + 1, copy);
                }
                2 => {
                    let cc = packets[i][3] & 0x0F;
                    let other = (cc + 1 + rng.below(15) as u8) & 0x0F;
                    packets[i][3] = (packets[i][3] & 0xF0) | other;
                }
                3 => {
                    // An adaptation_field_length out of its range.
                    let p = &mut packets[i];
                    p[3] |= 0x20;
                    p[4] = 184 + rng.below(72) as u8;
                }
                _ => {
                    // Any bit of the packet.
                    let byte = rng.below(PACKET as u64) as usize;
                    packets[i][byte] ^= 1 << rng.below(8);
                }
            }
        }
        let r = feed_in_batches(&mut rng, &mut mux, packets);
        let mut errors = 0;
        for unit in &r.units {
            let Some(original) = unit.pts.and_then(|pts| by_pts.get(&pts)) else {
                // A flipped bit can make another valid PTS: then it is
                // still a PES of this stream, whole or cut short.
                assert_eq!(kind, 4, "round {round}: a PTS not in the stream: {unit:?}");
                continue;
            };
            if kind != 4 {
                assert!(original.starts_with(&unit.data), "round {round} kind {kind}: bytes not the PES's");
            }
            errors += usize::from(unit.brk.as_deref().is_some_and(|b| b != "Start"));
        }
        match kind {
            // Always seen on the counter; a PES given after it says so
            // (none may follow when the damage is in the last one).
            0 | 2 => assert!(r.counts.cc_loss >= 1 && (errors >= 1 || r.counts.recovered == 0), "round {round}: {:?}", r.counts),
            1 => {
                assert_eq!(r.counts.cc_loss, 0, "round {round}");
                assert!(r.counts.duplicate >= 1, "round {round}");
            }
            3 => assert!(r.counts.adaptation_bad >= 1, "round {round}"),
            _ => {}
        }
    }
}

/// The viewer chose the second audio stream while the first played: the
/// sound goes to it from its next PES, as a new branch whatever its codec
/// (`rebuild`), and nothing more of the first stream is given.
#[test]
fn the_viewers_choice_moves_the_sound_from_the_next_pes_as_a_new_branch() {
    let mut mux = Mux::default();
    let streams = [(0x04, AUDIO, vec![]), (0x06, 0x103, vec![0x6A, 1, 0])];
    let mut all = mux.tables(0, PCR_PID, &streams);
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 60)), &[184])));
    let mut later = flat(mux.pes(AUDIO, &sound(93_600, &payload(50, 61)), &[184]));
    later.extend(flat(mux.pes(0x103, &pes(0xBD, Some(93_600), None, 0, &payload(50, 62), None), &[184])));
    later.extend(flat(mux.pes(AUDIO, &sound(97_200, &payload(50, 63)), &[184])));
    later.extend(flat(mux.pes(0x103, &pes(0xBD, Some(97_200), None, 0, &payload(50, 64), None), &[184])));
    let r = run(vec![Step::Select(AUDIO), Step::Feed(all), Step::Choose(1), Step::Feed(later)]);
    assert_eq!(
        shape(&r),
        vec![(Some(90_000), some("Start"), None), (Some(93_600), some("Reset"), some("0x103 ac3 true")), (Some(97_200), None, None)],
        "{r:?}"
    );
    assert_eq!(r.units[1].data, payload(50, 62));
    assert_eq!(r.units[2].data, payload(50, 64));
}

/// The stream already playing, or one the PMT does not have: nothing.
#[test]
fn choosing_the_stream_that_plays_or_none_changes_nothing() {
    let mut mux = Mux::default();
    let mut all = mux.tables(0, PCR_PID, &[(0x04, AUDIO, vec![])]);
    all.extend(flat(mux.pes(AUDIO, &sound(90_000, &payload(50, 65)), &[184])));
    let mut reader = Reader::new();
    let mut out = Output::default();
    reader.select(AUDIO, None, &mut out);
    reader.feed(&all);
    assert_eq!(reader.choose(0, &mut out), None);
    assert_eq!(reader.choose(3, &mut out), None);
    assert_eq!(reader.sound_pid(), Some(AUDIO));
}
