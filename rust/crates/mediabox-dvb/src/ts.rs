//! MPEG-TS: the multiplex's integrity, its programme tables, and one
//! service out of it.
//!
//! [`Analyser`] is kankylink/ts.py: synchronisation (five 0x47 at 188-byte
//! spacing to acquire or re-acquire), TEI, continuity counters (loss events,
//! a lower bound of lost packets, discontinuity flags not counted as errors),
//! scrambling, and PAT/PMT/SDT accepted only with a valid CRC-32 (ISO/IEC
//! 13818-1 Annex B). It never changes a byte.
//!
//! [`Selector`] turns the multiplex into a single-programme stream for one
//! service: a PAT of its own naming only that programme, and the programme's
//! PMT, PCR and elementary-stream packets exactly as they arrived. Nothing it
//! passes is rewritten, so PCR/PTS/DTS, the PMT's CRC and every passed PID's
//! continuity counters are the broadcaster's; the PAT it writes has its own
//! counter and CRC. What a demuxer then sees is one programme, and nothing to
//! choose between.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

pub const PACKET: usize = 188;
pub const PID_PAT: u16 = 0x0000;
pub const PID_NIT: u16 = 0x0010;
pub const PID_SDT: u16 = 0x0011;
pub const PID_EIT: u16 = 0x0012;
pub const PID_NULL: u16 = 0x1FFF;
const SYNC_RUN: usize = 5;
const CC_EVENT_GAP: u64 = 64;
const TABLE_PAT: u8 = 0x00;
const TABLE_PMT: u8 = 0x02;
const TABLE_SDT_ACTUAL: u8 = 0x42;
const TABLE_NIT_ACTUAL: u8 = 0x40;
/// EIT, actual transport stream: present/following, then the schedule.
const TABLE_EIT_PF_ACTUAL: u8 = 0x4E;
const TABLES_EIT_SCHEDULE_ACTUAL: std::ops::RangeInclusive<u8> = 0x50..=0x5F;
const DESC_SHORT_EVENT: u8 = 0x4D;
/// Events kept per service. The schedule of a day is some 30 events; four
/// days of it, as EN 300 468 tables 0x50.. carry, stay well under this.
const EVENTS_PER_SERVICE: usize = 256;
const DESC_CA: u8 = 0x09;
const DESC_LANGUAGE: u8 = 0x0A;
const DESC_SERVICE: u8 = 0x48;

fn stream_kind(stream_type: u8) -> (&'static str, String) {
    let known = match stream_type {
        0x01 => ("video", "MPEG-1 video"),
        0x02 => ("video", "MPEG-2 video"),
        0x03 => ("audio", "MPEG-1 audio"),
        0x04 => ("audio", "MPEG-2 audio"),
        0x0F => ("audio", "AAC (ADTS)"),
        0x11 => ("audio", "AAC (LATM)"),
        0x1B => ("video", "H.264"),
        0x24 => ("video", "HEVC"),
        0x81 => ("audio", "AC-3 (ATSC)"),
        0x87 => ("audio", "E-AC-3 (ATSC)"),
        0x05 => ("data", "private sections"),
        0x0B => ("data", "DSM-CC"),
        other => return ("other", format!("type 0x{other:02X}")),
    };
    (known.0, known.1.to_string())
}

/// Stream type 0x06 (PES private data) is told apart by its descriptor (EN 300 468).
fn private_kind(tag: u8) -> Option<(&'static str, &'static str)> {
    Some(match tag {
        0x6A => ("audio", "AC-3"),
        0x7A => ("audio", "E-AC-3"),
        0x7C => ("audio", "AAC"),
        0x59 => ("subtitle", "DVB subtitles"),
        0x56 => ("teletext", "teletext"),
        _ => return None,
    })
}

static CRC_TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut crc = (i as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
        *slot = crc;
    }
    table
});

/// CRC-32/MPEG-2: zero over a whole section, CRC included, when it is valid.
pub fn crc32_mpeg(data: &[u8]) -> u32 {
    data.iter().fold(0xFFFF_FFFF, |crc, &byte| (crc << 8) ^ CRC_TABLE[((crc >> 24) as u8 ^ byte) as usize])
}

fn descriptors(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos + 2 > data.len() {
            return None;
        }
        let (tag, length) = (data[pos], data[pos + 1] as usize);
        let body = &data[pos + 2..(pos + 2 + length).min(data.len())];
        pos += 2 + length;
        Some((tag, body))
    })
}

/// An EN 300 468 Annex A string; the default table approximated by Latin-1,
/// ISO 8859 parts and UTF-8/UTF-16 by their selectors.
pub fn dvb_text(raw: &[u8]) -> String {
    let Some(&first) = raw.first() else { return String::new() };
    let (codec, body): (&str, &[u8]) = match first {
        0x15 => ("utf-8", &raw[1..]),
        0x11 => ("utf-16be", &raw[1..]),
        0x05 => ("iso8859-9", &raw[1..]),
        0x01..=0x0B => ("latin-1", &raw[1..]),
        0x10 if raw.len() >= 3 => (if raw[2] == 9 { "iso8859-9" } else { "latin-1" }, &raw[3..]),
        0x00..=0x1F => ("latin-1", &raw[1..]),
        _ => ("latin-1", raw),
    };
    let text: String = match codec {
        "utf-8" => String::from_utf8_lossy(body).into_owned(),
        "utf-16be" => char::decode_utf16(body.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)))
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect(),
        // Turkish service names are ISO 8859-9; it differs from Latin-1 in six places.
        "iso8859-9" => body
            .iter()
            .filter(|b| !(0x80..=0x9F).contains(*b))
            .map(|&b| match b {
                0xD0 => 'Ğ',
                0xDD => 'İ',
                0xDE => 'Ş',
                0xF0 => 'ğ',
                0xFD => 'ı',
                0xFE => 'ş',
                other => other as char,
            })
            .collect(),
        _ => body.iter().filter(|b| !(0x80..=0x9F).contains(*b)).map(|&b| b as char).collect(),
    };
    text.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Stream {
    pub pid: u16,
    #[serde(rename = "type")]
    pub stream_type: u8,
    pub kind: &'static str,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Pmt {
    pub version: u8,
    pub pcr_pid: u16,
    pub streams: Vec<Stream>,
    pub ca_system_ids: Vec<u16>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Service {
    pub free_ca_mode: bool,
    pub service_type: Option<u8>,
    pub provider: Option<String>,
    pub name: Option<String>,
}

/// A section in the making, per PID: whole sections are taken off the front.
#[derive(Default)]
pub(crate) struct Sections {
    buffers: HashMap<u16, Vec<u8>>,
}

impl Sections {
    /// Feed a packet's payload; each complete, CRC-valid section is handed to `done`.
    pub(crate) fn payload(&mut self, pid: u16, unit_start: bool, payload: &[u8], mut done: impl FnMut(&[u8])) {
        if unit_start {
            let Some(&pointer) = payload.first() else { return };
            let tail_end = (1 + pointer as usize).min(payload.len());
            if let Some(mut buffer) = self.buffers.remove(&pid) {
                buffer.extend_from_slice(&payload[1..tail_end]);
                Self::take(&mut buffer, &mut done);
            }
            let mut buffer = payload[tail_end..].to_vec();
            if Self::take(&mut buffer, &mut done) {
                self.buffers.insert(pid, buffer);
            }
        } else if let Some(buffer) = self.buffers.get_mut(&pid) {
            buffer.extend_from_slice(payload);
            if !Self::take(buffer, &mut done) {
                self.buffers.remove(&pid);
            }
        }
    }

    /// Whole sections off the front; false when the rest is stuffing.
    fn take(buffer: &mut Vec<u8>, done: &mut impl FnMut(&[u8])) -> bool {
        while buffer.len() >= 3 {
            if buffer[0] == 0xFF {
                return false;
            }
            let length = ((buffer[1] as usize & 0x0F) << 8) | buffer[2] as usize;
            if buffer.len() < 3 + length {
                return true;
            }
            let section: Vec<u8> = buffer.drain(..3 + length).collect();
            if length >= 9 && crc32_mpeg(&section) == 0 {
                done(&section);
            }
        }
        true
    }

    fn clear(&mut self) {
        self.buffers.clear();
    }
}

/// Where a section's header puts things: (table_id, extension, version, current, body).
/// The sections of a table had so far, of its version on now: whole once
/// every one from 0 to its last_section_number is in (EN 300 468 5.1.1).
#[derive(Debug, Default, Clone)]
pub struct TableSections {
    version: Option<u8>,
    last: u8,
    had: std::collections::BTreeSet<u8>,
}

impl TableSections {
    fn take(&mut self, s: &[u8], version: u8) {
        if self.version != Some(version) {
            *self = TableSections { version: Some(version), ..TableSections::default() };
        }
        self.last = s[7];
        self.had.insert(s[6]);
    }

    pub fn complete(&self) -> bool {
        self.version.is_some() && (0..=self.last).all(|n| self.had.contains(&n))
    }
}

fn section_header(s: &[u8]) -> (u8, u16, u8, bool, &[u8]) {
    (s[0], u16::from_be_bytes([s[3], s[4]]), (s[5] >> 1) & 0x1F, s[5] & 0x01 != 0, &s[8..s.len() - 4])
}

fn parse_pat(body: &[u8]) -> BTreeMap<u16, u16> {
    body.as_chunks::<4>()
        .0
        .iter()
        .map(|e| (u16::from_be_bytes([e[0], e[1]]), u16::from_be_bytes([e[2] & 0x1F, e[3]])))
        .filter(|(program, _)| *program != 0) // 0 = NIT
        .collect()
}

fn parse_pmt(version: u8, body: &[u8]) -> Option<Pmt> {
    if body.len() < 4 {
        return None;
    }
    let pcr_pid = u16::from_be_bytes([body[0] & 0x1F, body[1]]);
    let info_len = (((body[2] & 0x0F) as usize) << 8) | body[3] as usize;
    let info = body.get(4..4 + info_len)?;
    let mut ca: BTreeSet<u16> = descriptors(info)
        .filter(|(tag, d)| *tag == DESC_CA && d.len() >= 2)
        .map(|(_, d)| u16::from_be_bytes([d[0], d[1]]))
        .collect();
    let mut streams = Vec::new();
    let mut pos = 4 + info_len;
    while pos + 5 <= body.len() {
        let stream_type = body[pos];
        let pid = u16::from_be_bytes([body[pos + 1] & 0x1F, body[pos + 2]]);
        let es_len = (((body[pos + 3] & 0x0F) as usize) << 8) | body[pos + 4] as usize;
        let (mut kind, mut codec) = stream_kind(stream_type);
        let mut lang = None;
        for (tag, d) in descriptors(&body[pos + 5..(pos + 5 + es_len).min(body.len())]) {
            if stream_type == 0x06
                && let Some((k, c)) = private_kind(tag)
            {
                (kind, codec) = (k, c.to_string());
            } else if tag == DESC_CA && d.len() >= 2 {
                ca.insert(u16::from_be_bytes([d[0], d[1]]));
            } else if tag == DESC_LANGUAGE && d.len() >= 3 {
                lang = Some(d[..3].iter().map(|&b| b as char).collect());
            }
        }
        streams.push(Stream { pid, stream_type, kind, codec, lang });
        pos += 5 + es_len;
    }
    Some(Pmt { version, pcr_pid, streams, ca_system_ids: ca.into_iter().collect() })
}

/// A NIT section's transport stream loop (EN 300 468 5.2.1): each TSID
/// with its original_network_id and satellite delivery descriptor (0x43).
pub(crate) fn parse_nit(body: &[u8], into: &mut BTreeMap<u16, (u16, crate::tkgs::TkgsDelivery)>) {
    let length = |at: usize| body.get(at..at + 2).map(|b| (((b[0] & 0x0F) as usize) << 8) | b[1] as usize);
    let Some(network_len) = length(0) else { return };
    let mut at = 2 + network_len;
    let Some(loop_len) = length(at) else { return };
    at += 2;
    let end = (at + loop_len).min(body.len());
    while at + 6 <= end {
        let tsid = u16::from_be_bytes([body[at], body[at + 1]]);
        let onid = u16::from_be_bytes([body[at + 2], body[at + 3]]);
        let Some(descriptors_len) = length(at + 4) else { return };
        let descriptors = &body[(at + 6).min(end)..(at + 6 + descriptors_len).min(end)];
        if let Some(delivery) = crate::tkgs::descriptor_loop(descriptors)
            .find(|(tag, _)| *tag == 0x43)
            .and_then(|(_, d)| crate::tkgs::delivery(d).ok())
        {
            into.insert(tsid, (onid, delivery));
        }
        at += 6 + descriptors_len;
    }
}

pub(crate) fn parse_sdt(body: &[u8], into: &mut BTreeMap<u16, Service>) {
    let mut pos = 3; // original_network_id(2) + reserved(1)
    while pos + 5 <= body.len() {
        let sid = u16::from_be_bytes([body[pos], body[pos + 1]]);
        let free_ca_mode = body[pos + 3] & 0x10 != 0;
        let loop_len = (((body[pos + 3] & 0x0F) as usize) << 8) | body[pos + 4] as usize;
        let mut entry = Service { free_ca_mode, ..Default::default() };
        for (tag, d) in descriptors(&body[pos + 5..(pos + 5 + loop_len).min(body.len())]) {
            if tag == DESC_SERVICE && d.len() >= 3 {
                let plen = d[1] as usize;
                let nlen = d.get(2 + plen).copied().unwrap_or(0) as usize;
                entry.service_type = Some(d[0]);
                entry.provider = Some(dvb_text(d.get(2..2 + plen).unwrap_or(&[])));
                entry.name = Some(dvb_text(d.get(3 + plen..3 + plen + nlen).unwrap_or(&[])));
            }
        }
        into.insert(sid, entry);
        pos += 5 + loop_len;
    }
}

/// One programme of a service's event information (EN 300 468 5.2.4), as
/// the broadcaster sent it. Nothing here is inferred: an event with no
/// start time or no name is not kept.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Event {
    pub event_id: u16,
    /// UTC, seconds since 1970.
    pub start: i64,
    /// Seconds.
    pub duration: u32,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// 4 = running, as the present/following table says it.
    pub running: u8,
}

/// Modified Julian Date and BCD time (EN 300 468 Annex C) to Unix seconds;
/// None for the all-ones "undefined".
pub fn dvb_time(t: &[u8]) -> Option<i64> {
    if t.len() < 5 || t.iter().all(|&b| b == 0xFF) {
        return None;
    }
    let mjd = u16::from_be_bytes([t[0], t[1]]) as i64;
    let bcd = |b: u8| -> Option<i64> {
        let (hi, lo) = (b >> 4, b & 0x0F);
        (hi < 10 && lo < 10).then_some((hi * 10 + lo) as i64)
    };
    let (h, m, sec) = (bcd(t[2])?, bcd(t[3])?, bcd(t[4])?);
    (h < 24 && m < 60 && sec < 61).then_some((mjd - 40_587) * 86_400 + h * 3_600 + m * 60 + sec)
}

fn dvb_duration(t: &[u8]) -> Option<u32> {
    let bcd = |b: u8| -> Option<u32> {
        let (hi, lo) = (b >> 4, b & 0x0F);
        (hi < 10 && lo < 10).then_some((hi * 10 + lo) as u32)
    };
    Some(bcd(*t.first()?)? * 3_600 + bcd(*t.get(1)?)? * 60 + bcd(*t.get(2)?)?)
}

/// The events of one EIT section's body (after the 8-byte header).
fn parse_eit(body: &[u8]) -> Vec<Event> {
    let mut events = Vec::new();
    let mut pos = 6; // transport_stream_id, original_network_id, segment_last, last_table_id
    while pos + 12 <= body.len() {
        let e = &body[pos..];
        let event_id = u16::from_be_bytes([e[0], e[1]]);
        let start = dvb_time(&e[2..7]);
        let duration = dvb_duration(&e[7..10]);
        let running = e[10] >> 5;
        let loop_len = (((e[10] & 0x0F) as usize) << 8) | e[11] as usize;
        let mut title = String::new();
        let mut text = String::new();
        for (tag, d) in descriptors(&e[12..(12 + loop_len).min(e.len())]) {
            if tag == DESC_SHORT_EVENT && d.len() >= 5 && title.is_empty() {
                let nlen = d[3] as usize;
                title = dvb_text(d.get(4..4 + nlen).unwrap_or(&[]));
                let tlen = d.get(4 + nlen).copied().unwrap_or(0) as usize;
                text = dvb_text(d.get(5 + nlen..5 + nlen + tlen).unwrap_or(&[]));
            }
        }
        if let (Some(start), Some(duration)) = (start, duration)
            && !title.is_empty()
            && duration > 0
        {
            events.push(Event { event_id, start, duration, title, text, running });
        }
        pos += 12 + loop_len;
    }
    events
}

/// The PCR in a packet's adaptation field, in 27 MHz ticks.
pub fn pcr_of(p: &[u8]) -> Option<u64> {
    if (p[3] >> 4) & 0x2 == 0 || p[4] < 7 || p[5] & 0x10 == 0 {
        return None;
    }
    let base = (p[6] as u64) << 25 | (p[7] as u64) << 17 | (p[8] as u64) << 9 | (p[9] as u64) << 1 | (p[10] as u64) >> 7;
    let extension = ((p[10] as u64 & 1) << 8) | p[11] as u64;
    Some(base * 300 + extension)
}

pub(crate) fn pid_of(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[1] & 0x1F, p[2]])
}

/// The payload of a packet, after its adaptation field; None if it has none.
pub(crate) fn payload_of(p: &[u8]) -> Option<&[u8]> {
    let afc = (p[3] >> 4) & 0x3;
    if afc & 1 == 0 {
        return None;
    }
    let pos = 4 + if afc & 2 != 0 { 1 + p[4] as usize } else { 0 };
    (pos < PACKET).then(|| &p[pos..])
}

// ------------------------------------------------------------------ analyser

#[derive(Debug, Clone, Default, Serialize)]
pub struct Integrity {
    pub bytes: u64,
    pub packets: u64,
    pub synced: bool,
    /// Stream offset of the first synchronised packet.
    pub offset: Option<u64>,
    pub sync_errors: u64,
    pub resync_skipped_bytes: u64,
    pub tei: u64,
    pub cc_errors: u64,
    pub cc_events: u64,
    /// Sum of (cc - last - 1) mod 16: a lower bound of the packets lost.
    pub cc_missing_min: u64,
    pub discontinuities: u64,
    pub duplicates: u64,
    pub scrambled_packets: u64,
    pub null_packets: u64,
    pub transport_stream_id: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Programme {
    pub program: u16,
    pub pmt_pid: u16,
    pub name: Option<String>,
    pub provider: Option<String>,
    pub pcr_pid: Option<u16>,
    pub streams: Vec<Stream>,
    pub ca_system_ids: Vec<u16>,
    pub free_ca_mode: Option<bool>,
    pub encryption: &'static str,
    pub es_packets: u64,
    pub scrambled_packets: u64,
}

#[derive(Default)]
pub struct Analyser {
    pending: Vec<u8>,
    synced: bool,
    total: u64,
    stats: Integrity,
    cc_by_pid: BTreeMap<u16, u64>,
    last_cc_error: Option<u64>,
    pids: HashMap<u16, u64>,
    scrambled: HashMap<u16, u64>,
    cc: HashMap<u16, u8>,
    sections: Sections,
    pub pat: BTreeMap<u16, u16>,
    pub pmts: BTreeMap<u16, Pmt>,
    pub sdt: BTreeMap<u16, Service>,
    /// The SDT's original_network_id: with the TSID and a SID, a channel's
    /// identity (TvContract.Channels' DVB triplet).
    pub original_network_id: Option<u16>,
    /// The NIT's transport streams of this network, by TSID: where each
    /// is (its satellite delivery descriptor, EN 300 468 6.2.13.2).
    pub network: BTreeMap<u16, (u16, crate::tkgs::TkgsDelivery)>,
    /// The table ids seen on the NIT's PID, and its packets.
    pub nit_tables: std::collections::BTreeSet<u8>,
    /// The SDT actual's and NIT actual's sections had.
    pub sdt_sections: TableSections,
    pub nit_sections: TableSections,
    /// Event information per service, keyed by start time.
    pub events: BTreeMap<u16, BTreeMap<i64, Event>>,
    /// Bumped whenever `sdt` or `events` gain something new.
    pub guide_changes: u64,
}

impl Analyser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Any chunking: the 188-byte alignment is carried across calls. Every
    /// synchronised packet is also handed to `each`, in order.
    pub fn feed(&mut self, data: &[u8], mut each: impl FnMut(&[u8])) {
        let base = self.total - self.pending.len() as u64;
        self.total += data.len() as u64;
        self.stats.bytes = self.total;
        let mut buffer = std::mem::take(&mut self.pending);
        buffer.extend_from_slice(data);
        let size = buffer.len();
        let mut pos = 0;
        loop {
            if !self.synced {
                match find_sync(&buffer, pos) {
                    None => {
                        let keep = pos.max(size.saturating_sub((SYNC_RUN - 1) * PACKET + 1));
                        self.skipped(keep - pos);
                        pos = keep;
                        break;
                    }
                    Some(found) => {
                        self.skipped(found - pos);
                        pos = found;
                        self.synced = true;
                        if self.stats.offset.is_none() {
                            self.stats.offset = Some(base + pos as u64);
                        }
                    }
                }
            }
            if size - pos < PACKET {
                break;
            }
            if buffer[pos] != 0x47 {
                // Sync lost: counted once, re-acquired from the next byte.
                self.stats.sync_errors += 1;
                self.synced = false;
                self.sections.clear();
                self.skipped(1);
                pos += 1;
                continue;
            }
            let packet = &buffer[pos..pos + PACKET];
            self.packet(packet);
            each(packet);
            pos += PACKET;
        }
        buffer.drain(..pos);
        self.pending = buffer;
    }

    fn skipped(&mut self, count: usize) {
        if self.stats.offset.is_some() {
            self.stats.resync_skipped_bytes += count as u64;
        }
    }

    fn packet(&mut self, p: &[u8]) {
        self.stats.packets += 1;
        let pid = pid_of(p);
        *self.pids.entry(pid).or_default() += 1;
        if p[1] & 0x80 != 0 {
            // transport_error_indicator: the header bits are not trusted
            self.stats.tei += 1;
            return;
        }
        if pid == PID_NULL {
            self.stats.null_packets += 1;
            return;
        }
        let (scrambling, afc, cc) = (p[3] >> 6, (p[3] >> 4) & 0x3, p[3] & 0xF);
        if scrambling != 0 {
            *self.scrambled.entry(pid).or_default() += 1;
            self.stats.scrambled_packets += 1;
        }
        if afc & 2 != 0 && p[4] != 0 && p[5] & 0x80 != 0 {
            self.stats.discontinuities += 1;
            self.cc.remove(&pid);
        }
        if afc & 1 == 0 {
            return; // adaptation field only: the counter does not advance
        }
        if let Some(&last) = self.cc.get(&pid) {
            if cc == last {
                self.stats.duplicates += 1;
            } else if cc != (last + 1) & 0xF {
                self.stats.cc_errors += 1;
                *self.cc_by_pid.entry(pid).or_default() += 1;
                self.stats.cc_missing_min += (cc.wrapping_sub(last).wrapping_sub(1) & 0xF) as u64;
                if self.last_cc_error.is_none_or(|at| self.stats.packets - at > CC_EVENT_GAP) {
                    self.stats.cc_events += 1;
                }
                self.last_cc_error = Some(self.stats.packets);
            }
        }
        self.cc.insert(pid, cc);
        if scrambling != 0
            || !(pid == PID_PAT || pid == PID_NIT || pid == PID_SDT || pid == PID_EIT || self.pat.values().any(|&pmt| pmt == pid))
        {
            return;
        }
        let Some(payload) = payload_of(p) else { return };
        let mut sections = std::mem::take(&mut self.sections);
        sections.payload(pid, p[1] & 0x40 != 0, payload, |section| self.section(pid, section));
        self.sections = sections;
    }

    fn section(&mut self, pid: u16, s: &[u8]) {
        let (table, ext, version, current, body) = section_header(s);
        if !current {
            return;
        }
        if pid == PID_PAT && table == TABLE_PAT {
            self.stats.transport_stream_id = Some(ext);
            self.pat.extend(parse_pat(body));
        } else if table == TABLE_PMT && self.pat.values().any(|&pmt| pmt == pid) {
            if let Some(pmt) = parse_pmt(version, body) {
                self.pmts.insert(ext, pmt);
            }
        } else if pid == PID_NIT && table != TABLE_NIT_ACTUAL {
            self.nit_tables.insert(table);
        } else if pid == PID_NIT && table == TABLE_NIT_ACTUAL {
            self.nit_tables.insert(table);
            self.nit_sections.take(s, version);
            let before = self.network.len();
            parse_nit(body, &mut self.network);
            if self.network.len() != before {
                self.guide_changes += 1;
            }
        } else if pid == PID_SDT && table == TABLE_SDT_ACTUAL {
            if body.len() >= 2 {
                self.original_network_id = Some(u16::from_be_bytes([body[0], body[1]]));
            }
            self.sdt_sections.take(s, version);
            let before = self.sdt.len();
            parse_sdt(body, &mut self.sdt);
            if self.sdt.len() != before {
                self.guide_changes += 1;
            }
        } else if pid == PID_EIT && (table == TABLE_EIT_PF_ACTUAL || TABLES_EIT_SCHEDULE_ACTUAL.contains(&table)) {
            self.take_events(ext, table == TABLE_EIT_PF_ACTUAL, parse_eit(body));
        }
    }

    /// Events of service `sid`. An event that overlaps one already kept
    /// replaces it: the broadcaster has rescheduled.
    fn take_events(&mut self, sid: u16, present_following: bool, events: Vec<Event>) {
        let kept = self.events.entry(sid).or_default();
        for mut event in events {
            if !present_following {
                // Only the present/following table says what is running.
                event.running = kept.get(&event.start).map_or(0, |e| e.running);
            }
            if kept.get(&event.start) == Some(&event) {
                continue;
            }
            let end = event.start + event.duration as i64;
            kept.retain(|&start, other| {
                let other_end = start + other.duration as i64;
                start == event.start || other_end <= event.start || start >= end
            });
            kept.insert(event.start, event);
            self.guide_changes += 1;
        }
        while kept.len() > EVENTS_PER_SERVICE {
            kept.pop_last();
        }
    }

    /// Forget events that ended before `now` less `keep` seconds.
    pub fn expire_events(&mut self, now: i64, keep: i64) {
        for kept in self.events.values_mut() {
            kept.retain(|&start, e| start + e.duration as i64 >= now - keep);
        }
    }

    pub fn packets_on(&self, pid: u16) -> u64 {
        self.pids.get(&pid).copied().unwrap_or(0)
    }

    pub fn integrity(&self) -> Integrity {
        Integrity { synced: self.stats.offset.is_some(), ..self.stats.clone() }
    }

    pub fn cc_error_pids(&self) -> Vec<(u16, u64)> {
        let mut pids: Vec<_> = self.cc_by_pid.iter().map(|(&p, &n)| (p, n)).collect();
        pids.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
        pids.truncate(10);
        pids
    }

    pub fn programmes(&self) -> Vec<Programme> {
        self.pat
            .iter()
            .map(|(&number, &pmt_pid)| {
                let pmt = self.pmts.get(&number);
                let sdt = self.sdt.get(&number);
                let streams = pmt.map(|pmt| pmt.streams.clone()).unwrap_or_default();
                let scrambled: u64 = streams.iter().map(|s| self.scrambled.get(&s.pid).copied().unwrap_or(0)).sum();
                let seen: u64 = streams.iter().map(|s| self.pids.get(&s.pid).copied().unwrap_or(0)).sum();
                let ca = pmt.map(|pmt| pmt.ca_system_ids.clone()).unwrap_or_default();
                let encryption = if scrambled > 0 {
                    "scrambled packets seen"
                } else if !ca.is_empty() || sdt.is_some_and(|s| s.free_ca_mode) {
                    "CA signalled, no scrambled packets seen"
                } else if seen > 0 {
                    "clear"
                } else {
                    "unknown (no ES packets)"
                };
                Programme {
                    program: number,
                    pmt_pid,
                    name: sdt.and_then(|s| s.name.clone()),
                    provider: sdt.and_then(|s| s.provider.clone()),
                    pcr_pid: pmt.map(|pmt| pmt.pcr_pid),
                    streams,
                    ca_system_ids: ca,
                    free_ca_mode: sdt.map(|s| s.free_ca_mode),
                    encryption,
                    es_packets: seen,
                    scrambled_packets: scrambled,
                }
            })
            .collect()
    }
}

fn find_sync(buffer: &[u8], mut pos: usize) -> Option<usize> {
    let last = buffer.len().checked_sub((SYNC_RUN - 1) * PACKET)?;
    while pos < last {
        let found = pos + buffer[pos..last].iter().position(|&b| b == 0x47)?;
        if (1..SYNC_RUN).all(|k| buffer[found + k * PACKET] == 0x47) {
            return Some(found);
        }
        pos = found + 1;
    }
    None
}

// ------------------------------------------------------------------ video

/// What a programme's video says about itself in its sequence parameter
/// set, read before a frame is decoded: how big, whether it is coded as
/// fields, and how often (VUI timing: ITU-T H.264 E.2.1, H.265 E.2.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VideoInfo {
    pub codec: &'static str,
    pub width: u32,
    pub height: u32,
    /// frame_mbs_only_flag == 0: the stream may carry fields (1080i).
    pub interlaced: bool,
    /// Frames per second, num/den (time_scale / (2 * num_units_in_tick)).
    pub frame_rate: Option<(u32, u32)>,
    /// Fields per second for an interlaced stream: what a deinterlacer that
    /// keeps every field (MPP's, measured: 50 frames out of 1080i25) puts out.
    pub field_rate: Option<(u32, u32)>,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(bit as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0u32, |value, _| Some(value << 1 | self.bit()?))
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k % 2 == 1 { k.div_ceil(2) as i32 } else { -((k / 2) as i32) })
    }
}

/// An RBSP: the NAL payload with its emulation-prevention bytes taken out.
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// Parse an H.264 SPS (the NAL without its one-byte header).
pub fn parse_h264_sps(nal_payload: &[u8]) -> Option<VideoInfo> {
    let data = rbsp(nal_payload);
    let mut b = Bits { data: &data, pos: 0 };
    let profile = b.bits(8)?;
    b.bits(16)?; // constraint flags, level
    b.ue()?; // seq_parameter_set_id
    let mut chroma_format = 1;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        chroma_format = b.ue()?;
        if chroma_format == 3 {
            b.bit()?; // separate_colour_plane_flag
        }
        b.ue()?; // bit_depth_luma_minus8
        b.ue()?; // bit_depth_chroma_minus8
        b.bit()?; // qpprime_y_zero_transform_bypass_flag
        if b.bit()? == 1 {
            for i in 0..if chroma_format == 3 { 12 } else { 8 } {
                if b.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + b.se()? + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    b.ue()?; // log2_max_frame_num_minus4
    match b.ue()? {
        0 => {
            b.ue()?;
        }
        1 => {
            b.bit()?;
            b.se()?;
            b.se()?;
            for _ in 0..b.ue()? {
                b.se()?;
            }
        }
        _ => {}
    }
    b.ue()?; // max_num_ref_frames
    b.bit()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = b.ue()? + 1;
    let height_map_units = b.ue()? + 1;
    let frame_mbs_only = b.bit()? == 1;
    if !frame_mbs_only {
        b.bit()?; // mb_adaptive_frame_field_flag
    }
    b.bit()?; // direct_8x8_inference_flag
    let (mut crop_x, mut crop_y) = (0, 0);
    if b.bit()? == 1 {
        let (left, right, top, bottom) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
        let (unit_x, unit_y) = match chroma_format {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        crop_x = (left + right) * unit_x;
        crop_y = (top + bottom) * unit_y * if frame_mbs_only { 1 } else { 2 };
    }
    let width = width_mbs * 16 - crop_x;
    let height = height_map_units * 16 * if frame_mbs_only { 1 } else { 2 } - crop_y;
    let mut frame_rate = None;
    if b.bit()? == 1 {
        // vui_parameters
        if b.bit()? == 1 && b.bits(8)? == 255 {
            b.bits(32)?; // sar_width, sar_height
        }
        if b.bit()? == 1 {
            b.bit()?; // overscan_appropriate_flag
        }
        if b.bit()? == 1 {
            b.bits(4)?; // video_format, video_full_range_flag
            if b.bit()? == 1 {
                b.bits(24)?; // colour_primaries, transfer, matrix
            }
        }
        if b.bit()? == 1 {
            b.ue()?;
            b.ue()?;
        }
        if b.bit()? == 1 {
            let units = b.bits(32)?;
            let scale = b.bits(32)?;
            if units > 0 && scale > 0 {
                frame_rate = Some(reduce(scale, 2 * units));
            }
        }
    }
    let field_rate = frame_rate.filter(|_| !frame_mbs_only).map(|(num, den)| reduce(num * 2, den));
    Some(VideoInfo { codec: "h264", width, height, interlaced: !frame_mbs_only, frame_rate, field_rate })
}

/// Parse an MPEG-1/2 sequence header (from just after its start code,
/// 00 00 01 B3) and, for MPEG-2, the sequence extension that follows it
/// (00 00 01 B5, identifier 1): ISO/IEC 13818-2 6.2.2.1 and 6.2.2.3. An
/// MPEG-2 channel (TRT BELGESEL's 576i) said nothing before, and its
/// display was matched from the frames shown -- under the playing channel,
/// and wrong: 25 Hz for its 50 fields.
pub fn parse_mpeg2_sequence(header: &[u8], extension: Option<&[u8]>) -> Option<VideoInfo> {
    let mut b = Bits { data: header, pos: 0 };
    let mut width = b.bits(12)?;
    let mut height = b.bits(12)?;
    b.bits(4)?; // aspect_ratio_information
    let (num, den) = match b.bits(4)? {
        1 => (24000, 1001),
        2 => (24, 1),
        3 => (25, 1),
        4 => (30000, 1001),
        5 => (30, 1),
        6 => (50, 1),
        7 => (60000, 1001),
        8 => (60, 1),
        _ => return None,
    };
    let (mut num, mut den) = (num, den);
    // MPEG-1 has no extension: progressive.
    let mut progressive = true;
    if let Some(extension) = extension {
        let mut e = Bits { data: extension, pos: 0 };
        if e.bits(4)? == 1 {
            e.bits(8)?; // profile_and_level_indication
            progressive = e.bit()? == 1;
            e.bits(2)?; // chroma_format
            width |= e.bits(2)? << 12;
            height |= e.bits(2)? << 12;
            e.bits(12)?; // bit_rate_extension
            e.bit()?; // marker_bit
            e.bits(8)?; // vbv_buffer_size_extension
            e.bit()?; // low_delay
            num *= e.bits(2)? + 1;
            den *= e.bits(5)? + 1;
        }
    }
    let frame_rate = Some(reduce(num, den));
    let field_rate = frame_rate.filter(|_| !progressive).map(|(num, den)| reduce(num * 2, den));
    Some(VideoInfo { codec: "mpeg2video", width, height, interlaced: !progressive, frame_rate, field_rate })
}

/// The rest of an H.265 profile_tier_level(1, max_sub_layers_minus1)
/// (ITU-T H.265 7.3.3), skipped: nothing in it says how often.
fn skip_h265_profile_tier_level(b: &mut Bits, max_sub_layers_minus1: u32) -> Option<()> {
    b.bits(8)?; // general profile space, tier, idc
    b.bits(32)?; // general_profile_compatibility_flag[32]
    b.bits(32)?; // four source flags and 43 reserved bits ...
    b.bits(16)?; // ... and general_inbld_flag
    b.bits(8)?; // general_level_idc
    let mut present = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        present.push((b.bit()?, b.bit()?));
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            b.bits(2)?;
        }
    }
    for (profile, level) in present {
        if profile == 1 {
            b.bits(32)?;
            b.bits(32)?;
            b.bits(24)?;
        }
        if level == 1 {
            b.bits(8)?;
        }
    }
    Some(())
}

/// Parse an H.265 SPS (the NAL without its two-byte header), as far as its
/// VUI timing (ITU-T H.265 7.3.2.2, E.2.1). A channel coded in HEVC (TRT
/// ULTRA, 3840x2160p50) said nothing before, and its display was matched
/// only from the frames shown, under the playing channel.
pub fn parse_h265_sps(nal_payload: &[u8]) -> Option<VideoInfo> {
    let data = rbsp(nal_payload);
    let mut b = Bits { data: &data, pos: 0 };
    b.bits(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = b.bits(3)?;
    b.bit()?; // sps_temporal_id_nesting_flag
    skip_h265_profile_tier_level(&mut b, max_sub_layers_minus1)?;
    b.ue()?; // sps_seq_parameter_set_id
    let chroma_format = b.ue()?;
    if chroma_format == 3 {
        b.bit()?; // separate_colour_plane_flag
    }
    let (mut width, mut height) = (b.ue()?, b.ue()?);
    if b.bit()? == 1 {
        // conformance_window, in chroma samples
        let (left, right, top, bottom) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
        let (unit_x, unit_y) = match chroma_format {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        width = width.checked_sub((left + right) * unit_x)?;
        height = height.checked_sub((top + bottom) * unit_y)?;
    }
    b.ue()?; // bit_depth_luma_minus8
    b.ue()?; // bit_depth_chroma_minus8
    let poc_lsb_bits = b.ue()? + 4;
    let ordering_for_all = b.bit()? == 1;
    for _ in if ordering_for_all { 0 } else { max_sub_layers_minus1 }..=max_sub_layers_minus1 {
        b.ue()?;
        b.ue()?;
        b.ue()?;
    }
    for _ in 0..6 {
        b.ue()?; // coding and transform block sizes, hierarchy depths
    }
    if b.bit()? == 1 && b.bit()? == 1 {
        // scaling_list_data (7.3.4)
        for size in 0..4u32 {
            let mut matrix = 0;
            while matrix < 6 {
                if b.bit()? == 0 {
                    b.ue()?; // scaling_list_pred_matrix_id_delta
                } else {
                    let coefficients = 64.min(1 << (4 + (size << 1)));
                    if size > 1 {
                        b.se()?; // scaling_list_dc_coef_minus8
                    }
                    for _ in 0..coefficients {
                        b.se()?;
                    }
                }
                matrix += if size == 3 { 3 } else { 1 };
            }
        }
    }
    b.bit()?; // amp_enabled_flag
    b.bit()?; // sample_adaptive_offset_enabled_flag
    if b.bit()? == 1 {
        b.bits(8)?; // pcm sample bit depths
        b.ue()?;
        b.ue()?;
        b.bit()?; // pcm_loop_filter_disabled_flag
    }
    // st_ref_pic_set (7.3.7), each one's size needed by the next.
    let sets = b.ue()?;
    let mut delta_pocs: Vec<u32> = Vec::new();
    for i in 0..sets {
        let predicted = i != 0 && b.bit()? == 1;
        if predicted {
            b.bit()?; // delta_rps_sign
            b.ue()?; // abs_delta_rps_minus1
            let reference = *delta_pocs.last()?;
            let mut count = 0;
            for _ in 0..=reference {
                let used = b.bit()? == 1;
                if used || b.bit()? == 1 {
                    count += 1;
                }
            }
            delta_pocs.push(count);
        } else {
            let (negative, positive) = (b.ue()?, b.ue()?);
            for _ in 0..negative.checked_add(positive)? {
                b.ue()?; // delta_poc_minus1
                b.bit()?; // used_by_curr_pic_flag
            }
            delta_pocs.push(negative + positive);
        }
    }
    if b.bit()? == 1 {
        for _ in 0..b.ue()? {
            b.bits(poc_lsb_bits)?;
            b.bit()?;
        }
    }
    b.bit()?; // sps_temporal_mvp_enabled_flag
    b.bit()?; // strong_intra_smoothing_enabled_flag
    let (mut frame_rate, mut fields) = (None, false);
    if b.bit()? == 1 {
        // vui_parameters (E.2.1)
        if b.bit()? == 1 && b.bits(8)? == 255 {
            b.bits(32)?; // sar_width, sar_height
        }
        if b.bit()? == 1 {
            b.bit()?; // overscan_appropriate_flag
        }
        if b.bit()? == 1 {
            b.bits(4)?; // video_format, video_full_range_flag
            if b.bit()? == 1 {
                b.bits(24)?; // colour_primaries, transfer, matrix
            }
        }
        if b.bit()? == 1 {
            b.ue()?;
            b.ue()?;
        }
        b.bit()?; // neutral_chroma_indication_flag
        fields = b.bit()? == 1; // field_seq_flag: every picture a field
        b.bit()?; // frame_field_info_present_flag
        if b.bit()? == 1 {
            for _ in 0..4 {
                b.ue()?; // default display window
            }
        }
        if b.bit()? == 1 {
            let units = b.bits(32)?;
            let scale = b.bits(32)?;
            if units > 0 && scale > 0 {
                // Unlike H.264's, a tick is a picture.
                frame_rate = Some(reduce(scale, units));
            }
        }
    }
    // Coded as fields, a picture is a field: two to a frame.
    let (frame_rate, field_rate) = if fields {
        (frame_rate.map(|(num, den)| reduce(num, den * 2)), frame_rate)
    } else {
        (frame_rate, None)
    };
    Some(VideoInfo { codec: "hevc", width, height, interlaced: fields, frame_rate, field_rate })
}

fn reduce(num: u32, den: u32) -> (u32, u32) {
    let gcd = |mut a: u32, mut b: u32| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let g = gcd(num, den).max(1);
    (num / g, den / g)
}

/// The first SPS in a video PID's PES payloads.
#[derive(Default)]
struct VideoProbe {
    pid: Option<u16>,
    codec: VideoCodec,
    buffer: Vec<u8>,
    in_pes: bool,
}

impl VideoProbe {
    const LIMIT: usize = 512 * 1024;

    fn packet(&mut self, p: &[u8]) -> Option<VideoInfo> {
        let payload = payload_of(p)?;
        let mut data = payload;
        if p[1] & 0x40 != 0 {
            // A PES header: 00 00 01 id, length(2), flags(2), header length.
            if payload.len() < 9 || payload[..3] != [0, 0, 1] {
                return None;
            }
            data = payload.get(9 + payload[8] as usize..)?;
            self.in_pes = true;
        }
        if !self.in_pes {
            return None;
        }
        self.buffer.extend_from_slice(data);
        let found = find_sps(&self.buffer, self.codec);
        if found.is_none() && self.buffer.len() > Self::LIMIT {
            let keep = self.buffer.len() - 1024;
            self.buffer.drain(..keep);
        }
        found
    }
}

/// The video codecs whose parameters are read before a frame is decoded.
#[derive(Default, Clone, Copy, PartialEq)]
enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Mpeg2,
}

fn find_sps(buffer: &[u8], codec: VideoCodec) -> Option<VideoInfo> {
    let starts: Vec<usize> = buffer.windows(3).enumerate().filter(|(_, w)| *w == [0, 0, 1]).map(|(i, _)| i + 3).collect();
    if codec == VideoCodec::Mpeg2 {
        // The sequence header, and the start code after it; MPEG-2's
        // sequence extension, when that is what comes next.
        let k = starts.iter().position(|&start| buffer.get(start) == Some(&0xB3))?;
        let header = buffer.get(starts[k] + 1..starts.get(k + 1)?.saturating_sub(3))?;
        let next = starts[k + 1];
        let extension = if buffer.get(next) == Some(&0xB5) {
            Some(buffer.get(next + 1..starts.get(k + 2)?.saturating_sub(3))?)
        } else {
            None
        };
        return parse_mpeg2_sequence(header, extension);
    }
    let hevc = codec == VideoCodec::Hevc;
    for (k, &start) in starts.iter().enumerate() {
        // The NAL unit type: H.264's in the low five bits of one header
        // byte (SPS 7), H.265's in bits 1-6 of the first of two (SPS 33).
        let sps = buffer
            .get(start)
            .is_some_and(|header| if hevc { header >> 1 & 0x3F == 33 } else { header & 0x1F == 7 });
        if sps {
            // Wait for the next start code: the SPS must be whole.
            let end = *starts.get(k + 1)?;
            let nal = &buffer[start..end.saturating_sub(3)];
            return if hevc { parse_h265_sps(nal.get(2..)?) } else { parse_h264_sps(nal.get(1..)?) };
        }
    }
    None
}

// ------------------------------------------------------------------ selector

#[derive(Debug, Clone, Default, Serialize)]
pub struct Selection {
    pub service_id: u16,
    pub transport_stream_id: Option<u16>,
    pub pmt_pid: Option<u16>,
    pub pcr_pid: Option<u16>,
    /// The PIDs passed through: the PMT's, the PCR's and every stream's.
    pub pids: Vec<u16>,
    pub pmt_version: Option<u8>,
    pub streams: Vec<Stream>,
    pub input_packets: u64,
    pub output_packets: u64,
    pub pat_written: u64,
    pub pmt_packets: u64,
    pub es_packets: u64,
    /// Of those, the ones whose transport_scrambling_control says they are
    /// encrypted: a service the SDT calls free can still be scrambled on
    /// the night (measured on TRT1 HD, 2026-10-22 19:39).
    pub scrambled_packets: u64,
    pub tei_dropped: u64,
    /// PATs that did not name the service.
    pub service_missing: u64,
    /// The programme's video, from its SPS, once one has been read.
    pub video: Option<VideoInfo>,
    /// The programme's own clock and timestamps, as ETSI TR 101 290 judges
    /// them: what the player will see, apart from anything the output adds.
    pub timing: Timing,
}

/// ETSI TR 101 290 on the selected programme, from the stream's own clock.
///
/// The multiplex-wide counts (`Integrity`) cannot say whether *this* channel
/// lost anything: a continuity error on another service's PID is not one the
/// player sees. These are this programme's PIDs only.
///
/// Intervals are measured in programme time (PCR and PTS), not in arrival
/// time: a receiver that delivers in bursts (USB) would otherwise count its
/// own buffering as the broadcaster's fault.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Timing {
    /// 1.4 Continuity_count_error, on the programme's PIDs.
    pub cc_errors: u64,
    pub pcr_count: u64,
    /// 2.3a PCR_repetition_error: more than 40 ms between two PCRs.
    pub pcr_repetition_errors: u64,
    /// 2.3b PCR_discontinuity_indicator_error: a jump of more than 100 ms,
    /// or backwards, that the broadcaster did not announce.
    pub pcr_discontinuities: u64,
    /// Announced ones (discontinuity_indicator): not errors.
    pub pcr_announced_discontinuities: u64,
    pub pcr_max_interval_ms: f64,
    /// 2.5 PTS_error: more than 700 ms between two PTSs of one sound or
    /// picture stream. Subtitles and teletext carry a PTS only when they
    /// have something to show, so they are not judged by it.
    pub pts_errors: u64,
    pub pts_max_interval_ms: f64,
    pub pes_starts: u64,
    /// The programme clock as last received, seconds (PCR / 27 MHz): where
    /// the broadcast is now, for how far behind it the player shows.
    pub pcr_seconds: f64,
    /// When that PCR was read, Unix seconds: status is written once a
    /// second, so the clock is told with the moment it was true.
    pub pcr_at: f64,
}

/// What `Timing` needs to remember between packets.
#[derive(Default)]
struct TimingState {
    last_pcr: Option<u64>,
    cc: std::collections::BTreeMap<u16, u8>,
    pts: std::collections::BTreeMap<u16, u64>,
    /// The programme's sound and picture PIDs: the ones 2.5 applies to.
    timed: BTreeSet<u16>,
}

const PCR_HZ: f64 = 27_000_000.0;
const PTS_HZ: f64 = 90_000.0;
const PCR_WRAP: u64 = (1 << 33) * 300;
const PTS_WRAP: u64 = 1 << 33;

/// The adaptation field's discontinuity_indicator.
fn discontinuity_of(p: &[u8]) -> bool {
    (p[3] >> 4) & 0x2 != 0 && p[4] > 0 && p[5] & 0x80 != 0
}

/// The PTS of a PES that starts in this packet, 90 kHz ticks.
pub(crate) fn pts_of(p: &[u8]) -> Option<u64> {
    if p[1] & 0x40 == 0 {
        return None;
    }
    let pes = payload_of(p)?;
    if pes.len() < 14 || pes[0..3] != [0, 0, 1] || pes[7] & 0x80 == 0 {
        return None;
    }
    let b = &pes[9..14];
    Some(
        ((b[0] as u64 >> 1) & 0x07) << 30
            | (b[1] as u64) << 22
            | ((b[2] as u64) >> 1) << 15
            | (b[3] as u64) << 7
            | (b[4] as u64) >> 1,
    )
}

impl TimingState {
    fn packet(&mut self, t: &mut Timing, p: &[u8], pid: u16, pcr_pid: Option<u16>) {
        let announced = discontinuity_of(p);
        // Continuity: only packets with a payload count; a repeat is allowed
        // once (duplicate), and an announced discontinuity restarts it.
        if (p[3] >> 4) & 0x1 != 0 {
            let cc = p[3] & 0x0F;
            if let Some(last) = self.cc.insert(pid, cc)
                && !announced
                && cc != last
                && cc != (last + 1) & 0x0F
            {
                t.cc_errors += 1;
            }
        }
        if Some(pid) == pcr_pid
            && let Some(pcr) = pcr_of(p)
        {
            t.pcr_count += 1;
            t.pcr_seconds = pcr as f64 / PCR_HZ;
            t.pcr_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |since| since.as_secs_f64());
            if let Some(last) = self.last_pcr.replace(pcr) {
                let delta = (pcr + PCR_WRAP - last) % PCR_WRAP;
                let ms = delta as f64 / PCR_HZ * 1000.0;
                if announced {
                    t.pcr_announced_discontinuities += 1;
                } else if ms > 100.0 {
                    // Backwards shows up as a delta near the wrap.
                    t.pcr_discontinuities += 1;
                } else {
                    if ms > 40.0 {
                        t.pcr_repetition_errors += 1;
                    }
                    t.pcr_max_interval_ms = t.pcr_max_interval_ms.max(ms);
                }
            }
        }
        if p[3] >> 6 == 0
            && self.timed.contains(&pid)
            && let Some(pts) = pts_of(p)
        {
            t.pes_starts += 1;
            if let Some(last) = self.pts.insert(pid, pts)
                && !announced
            {
                let delta = (pts + PTS_WRAP - last) % PTS_WRAP;
                let ms = delta as f64 / PTS_HZ * 1000.0;
                // Backwards (B-frames reorder PTS) is not a gap.
                if delta < PTS_WRAP / 2 {
                    if ms > 700.0 {
                        t.pts_errors += 1;
                    }
                    t.pts_max_interval_ms = t.pts_max_interval_ms.max(ms);
                }
            }
        }
    }
}

pub struct Selector {
    /// The last PCR of the programme's PCR PID, in 27 MHz ticks.
    last_pcr: Option<u64>,
    video: VideoProbe,
    sections: Sections,
    pmt_sections: Sections,
    selection: Selection,
    pat_version: u8,
    pat_cc: u8,
    pids: BTreeSet<u16>,
    timing: TimingState,
}

impl Selector {
    pub fn new(service_id: u16) -> Self {
        Self {
            last_pcr: None,
            video: VideoProbe::default(),
            sections: Sections::default(),
            pmt_sections: Sections::default(),
            selection: Selection { service_id, ..Default::default() },
            pat_version: 0,
            pat_cc: 0,
            pids: BTreeSet::new(),
            timing: TimingState::default(),
        }
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The programme clock as last seen, 27 MHz ticks.
    pub fn last_pcr(&self) -> Option<u64> {
        self.last_pcr
    }

    /// Whether the service's programme has been found (PAT and PMT seen).
    pub fn ready(&self) -> bool {
        self.selection.pmt_version.is_some()
    }

    /// One synchronised packet of the multiplex; what to write is appended to `out`.
    pub fn packet(&mut self, p: &[u8], out: &mut Vec<u8>) {
        self.selection.input_packets += 1;
        if p[1] & 0x80 != 0 {
            // A packet whose header cannot be trusted is not passed on: its
            // PID may not even be the one it claims.
            if self.pids.contains(&pid_of(p)) || Some(pid_of(p)) == self.selection.pmt_pid {
                self.selection.tei_dropped += 1;
            }
            return;
        }
        let pid = pid_of(p);
        if pid == PID_PAT {
            if p[3] >> 6 == 0
                && let Some(payload) = payload_of(p)
            {
                let mut found = Vec::new();
                self.sections.payload(pid, p[1] & 0x40 != 0, payload, |s| found.push(s.to_vec()));
                for section in found {
                    self.pat_section(&section, out);
                }
            }
            return;
        }
        if Some(pid) == self.selection.pmt_pid {
            if self.selection.pat_written == 0 {
                return;
            }
            if p[3] >> 6 == 0
                && let Some(payload) = payload_of(p)
            {
                let mut found = Vec::new();
                self.pmt_sections.payload(pid, p[1] & 0x40 != 0, payload, |s| found.push(s.to_vec()));
                for section in found {
                    self.pmt_section(&section);
                }
            }
            self.selection.pmt_packets += 1;
            self.emit(p, out);
            return;
        }
        if self.ready() && self.pids.contains(&pid) {
            self.timing.packet(&mut self.selection.timing, p, pid, self.selection.pcr_pid);
            if Some(pid) == self.selection.pcr_pid
                && let Some(pcr) = pcr_of(p)
            {
                self.last_pcr = Some(pcr);
            }
            if self.selection.video.is_none()
                && self.video.pid == Some(pid)
                && let Some(info) = self.video.packet(p)
            {
                self.selection.video = Some(info);
                self.video.buffer = Vec::new();
            }
            self.selection.es_packets += 1;
            if p[3] >> 6 != 0 {
                self.selection.scrambled_packets += 1;
            }
            self.emit(p, out);
        }
    }

    fn emit(&mut self, p: &[u8], out: &mut Vec<u8>) {
        self.selection.output_packets += 1;
        out.extend_from_slice(p);
    }

    fn pat_section(&mut self, s: &[u8], out: &mut Vec<u8>) {
        let (table, tsid, version, current, body) = section_header(s);
        if table != TABLE_PAT || !current {
            return;
        }
        let programmes = parse_pat(body);
        let Some(&pmt_pid) = programmes.get(&self.selection.service_id) else {
            self.selection.service_missing += 1;
            return;
        };
        if self.selection.pmt_pid != Some(pmt_pid) {
            // A new mapping: the PMT is read afresh before anything else passes.
            self.selection.pmt_pid = Some(pmt_pid);
            self.selection.pmt_version = None;
            self.pmt_sections.clear();
            self.pids.clear();
        }
        self.selection.transport_stream_id = Some(tsid);
        self.pat_version = version;
        // One PAT of ours for every one of theirs: the same repetition rate.
        self.write_pat(tsid, pmt_pid, out);
    }

    fn pmt_section(&mut self, s: &[u8]) {
        let (table, program, version, current, body) = section_header(s);
        if table != TABLE_PMT || !current || program != self.selection.service_id {
            return;
        }
        let Some(pmt) = parse_pmt(version, body) else { return };
        if self.selection.pmt_version == Some(version) {
            return;
        }
        self.pids = pmt.streams.iter().map(|s| s.pid).chain([pmt.pcr_pid]).filter(|&pid| pid != PID_NULL).collect();
        self.selection.pcr_pid = Some(pmt.pcr_pid);
        self.selection.pmt_version = Some(version);
        self.timing.timed =
            pmt.streams.iter().filter(|s| matches!(s.kind, "video" | "audio")).map(|s| s.pid).collect();
        // H.264 (0x1B), H.265 (0x24), MPEG-2 (0x02) or MPEG-1 (0x01).
        let video = pmt.streams.iter().find(|s| matches!(s.stream_type, 0x1B | 0x24 | 0x02 | 0x01));
        let pid = video.map(|s| s.pid);
        if self.video.pid != pid {
            let codec = match video.map(|s| s.stream_type) {
                Some(0x24) => VideoCodec::Hevc,
                Some(0x01 | 0x02) => VideoCodec::Mpeg2,
                _ => VideoCodec::H264,
            };
            self.video = VideoProbe { pid, codec, ..Default::default() };
            self.selection.video = None;
        }
        self.selection.streams = pmt.streams;
        self.selection.pids = self.pids.iter().copied().chain(self.selection.pmt_pid).collect();
        self.selection.pids.sort_unstable();
    }

    fn write_pat(&mut self, tsid: u16, pmt_pid: u16, out: &mut Vec<u8>) {
        let mut section = vec![
            TABLE_PAT,
            0xB0, // section_syntax_indicator, '0', reserved; length follows
            13,   // 5 header bytes after the length, one programme, the CRC
        ];
        section.extend_from_slice(&tsid.to_be_bytes());
        section.push(0xC0 | (self.pat_version & 0x1F) << 1 | 0x01); // current_next
        section.extend_from_slice(&[0, 0]); // section_number, last_section_number
        section.extend_from_slice(&self.selection.service_id.to_be_bytes());
        section.extend_from_slice(&(0xE000 | pmt_pid).to_be_bytes());
        let crc = crc32_mpeg(&section);
        section.extend_from_slice(&crc.to_be_bytes());

        let mut packet = [0xFFu8; PACKET];
        packet[..4].copy_from_slice(&[0x47, 0x40, 0x00, 0x10 | self.pat_cc]);
        packet[4] = 0; // pointer_field
        packet[5..5 + section.len()].copy_from_slice(&section);
        self.pat_cc = (self.pat_cc + 1) & 0xF;
        self.selection.pat_written += 1;
        self.emit(&packet, out);
    }
}

// ------------------------------------------------------------------ RAP cache

/// Packets a programme can start from, for a channel change inside the
/// multiplex the receiver is already on.
///
/// A decoder can start only at a random access point (an IDR, its SPS before
/// it); joined at any other packet it waits for the next one, which on TRT's
/// services is up to a second (GOP measured 1.0 s, 2026-10-22). Broadcast
/// receivers close that gap by keeping what arrived since the last access
/// point and starting the new channel from it ("stored-frame playback from
/// the most recent anchor point"). DVB marks the point itself: the
/// adaptation field's random_access_indicator on the packet that starts it
/// (ETSI TS 101 154; set on every TRT access point, measured).
///
/// For every programme: the last PAT packet, its last PMT packet, and every
/// packet of its streams since its video's last access point.
#[derive(Default)]
pub struct RapCache {
    pat: Option<Vec<u8>>,
    /// programme -> (PMT PID, video PID, stream PIDs)
    layout: BTreeMap<u16, (u16, Option<u16>, BTreeSet<u16>)>,
    by_pid: HashMap<u16, u16>,
    pmt: HashMap<u16, Vec<u8>>,
    since_rap: HashMap<u16, Vec<u8>>,
}

/// A programme's packets since its access point beyond this are let go: a
/// stream without access points (or with a broken flag) costs no more.
const RAP_CACHE_LIMIT: usize = 8 << 20;

impl RapCache {
    /// The programmes' PIDs, from what the analyser knows of the PMTs.
    pub fn learn(&mut self, pat: &BTreeMap<u16, u16>, pmts: &BTreeMap<u16, Pmt>) {
        for (&program, pmt) in pmts {
            let Some(&pmt_pid) = pat.get(&program) else { continue };
            let video = pmt.streams.iter().find(|s| s.kind == "video").map(|s| s.pid);
            let pids: BTreeSet<u16> = pmt.streams.iter().map(|s| s.pid).chain([pmt.pcr_pid]).collect();
            let entry = (pmt_pid, video, pids);
            if self.layout.get(&program) != Some(&entry) {
                self.layout.insert(program, entry);
                self.by_pid.clear();
                for (&program, (_, _, pids)) in &self.layout {
                    for &pid in pids {
                        self.by_pid.insert(pid, program);
                    }
                }
            }
        }
    }

    pub fn packet(&mut self, p: &[u8]) {
        if p.len() != PACKET || p[1] & 0x80 != 0 {
            return;
        }
        let pid = pid_of(p);
        if pid == PID_PAT {
            if p[1] & 0x40 != 0 {
                self.pat = Some(p.to_vec());
            }
            return;
        }
        if p[1] & 0x40 != 0
            && let Some((&program, _)) = self.layout.iter().find(|(_, (pmt, _, _))| *pmt == pid)
        {
            self.pmt.insert(program, p.to_vec());
            return;
        }
        let Some(&program) = self.by_pid.get(&pid) else { return };
        let video = self.layout.get(&program).and_then(|(_, video, _)| *video);
        let afc = (p[3] >> 4) & 0x3;
        let random_access = afc & 2 != 0 && p[4] > 0 && p[5] & 0x40 != 0;
        let kept = self.since_rap.entry(program).or_default();
        if random_access && Some(pid) == video {
            kept.clear();
        } else if kept.is_empty() {
            // Nothing before the first access point is any use.
            return;
        }
        if kept.len() + PACKET > RAP_CACHE_LIMIT {
            kept.clear();
            return;
        }
        kept.extend_from_slice(p);
    }

    /// What `program` can start from now: PAT, PMT, then its streams since
    /// the last access point. Empty when there is no access point yet.
    pub fn replay(&self, program: u16) -> Vec<u8> {
        let (Some(pat), Some(pmt), Some(kept)) = (&self.pat, self.pmt.get(&program), self.since_rap.get(&program)) else {
            return Vec::new();
        };
        if kept.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(2 * PACKET + kept.len());
        out.extend_from_slice(pat);
        out.extend_from_slice(pmt);
        out.extend_from_slice(kept);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, unit_start: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x47, (if unit_start { 0x40 } else { 0 }) | (pid >> 8) as u8, pid as u8, 0x10 | cc];
        p.extend_from_slice(payload);
        p.resize(PACKET, 0xFF);
        p
    }

    fn section(table: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
        let length = 5 + body.len() + 4;
        let mut s = vec![table, 0xB0 | (length >> 8) as u8, length as u8];
        s.extend_from_slice(&ext.to_be_bytes());
        s.extend_from_slice(&[0xC1 | version << 1, 0, 0]);
        s.extend_from_slice(body);
        let crc = crc32_mpeg(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    fn psi(pid: u16, cc: u8, section: &[u8]) -> Vec<u8> {
        let mut payload = vec![0];
        payload.extend_from_slice(section);
        packet(pid, true, cc, &payload)
    }

    fn multiplex() -> Vec<Vec<u8>> {
        // Two programmes: 10801 (PMT 34: video 2301, audio 2401, PCR 2301), 10802 (PMT 35: video 2302).
        let pat = section(TABLE_PAT, 41306, 3, &[0, 0, 0xE0, 0x10, 0x2A, 0x31, 0xE0, 34, 0x2A, 0x32, 0xE0, 35]);
        let pmt1 = section(TABLE_PMT, 10801, 1, &[0xE8, 0xFD, 0xF0, 0, 0x1B, 0xE8, 0xFD, 0xF0, 0, 0x03, 0xE9, 0x61, 0xF0, 0]);
        let pmt2 = section(TABLE_PMT, 10802, 1, &[0xE8, 0xFE, 0xF0, 0, 0x1B, 0xE8, 0xFE, 0xF0, 0]);
        let mut out = Vec::new();
        for round in 0..3u8 {
            out.push(psi(0, round, &pat));
            out.push(psi(34, round, &pmt1));
            out.push(psi(35, round, &pmt2));
            for k in 0..4u8 {
                let cc = (round * 4 + k) & 0xF;
                out.push(packet(2301, k == 0, cc, &[0, 0, 1, 0xE0]));
                out.push(packet(2302, k == 0, cc, &[0, 0, 1, 0xE0]));
                out.push(packet(2401, k == 0, cc, &[0, 0, 1, 0xC0]));
                out.push(packet(PID_NULL, false, 0, &[]));
            }
        }
        out
    }

    #[test]
    fn a_valid_section_checks_to_zero() {
        let s = section(TABLE_PAT, 1, 0, &[0, 1, 0xE0, 0x20]);
        assert_eq!(crc32_mpeg(&s), 0);
        let mut broken = s.clone();
        broken[9] ^= 1;
        assert_ne!(crc32_mpeg(&broken), 0);
    }

    #[test]
    fn the_analyser_reads_the_programmes_and_counts_nothing_lost() {
        let mut analyser = Analyser::new();
        let bytes: Vec<u8> = multiplex().concat();
        // Odd chunking, and a half packet in front as a USB read leaves one.
        analyser.feed(&bytes[..1000][..0], |_| {});
        let mut stream = vec![0x12; 136];
        stream.extend_from_slice(&bytes);
        for chunk in stream.chunks(777) {
            analyser.feed(chunk, |_| {});
        }
        let integrity = analyser.integrity();
        assert_eq!(integrity.offset, Some(136));
        assert_eq!(integrity.cc_errors, 0);
        assert_eq!(integrity.sync_errors, 0);
        assert_eq!(integrity.transport_stream_id, Some(41306));
        let programmes = analyser.programmes();
        assert_eq!(programmes.len(), 2);
        assert_eq!(programmes[0].program, 10801);
        assert_eq!(programmes[0].pcr_pid, Some(2301));
        assert_eq!(programmes[0].streams.iter().map(|s| (s.pid, s.kind)).collect::<Vec<_>>(), vec![(2301, "video"), (2401, "audio")]);
        assert_eq!(programmes[0].encryption, "clear");
    }

    #[test]
    fn a_lost_packet_is_one_cc_error() {
        let mut analyser = Analyser::new();
        let mut packets = multiplex();
        packets.remove(7); // the second 2301 packet (cc 1)
        analyser.feed(&packets.concat(), |_| {});
        assert_eq!(analyser.integrity().cc_errors, 1);
        assert_eq!(analyser.integrity().cc_missing_min, 1);
    }

    #[test]
    fn the_selector_leaves_one_programme_with_its_own_pat() {
        let mut analyser = Analyser::new();
        let mut selector = Selector::new(10801);
        let mut out = Vec::new();
        analyser.feed(&multiplex().concat(), |p| selector.packet(p, &mut out));
        assert_eq!(out.len() % PACKET, 0);

        let mut check = Analyser::new();
        let mut passed = Vec::new();
        check.feed(&out, |p| passed.push(pid_of(p)));
        assert_eq!(check.integrity().cc_errors, 0, "the PAT's own counter and every passed PID's are continuous");
        assert_eq!(check.pat, BTreeMap::from([(10801, 34)]));
        assert_eq!(check.programmes()[0].streams.len(), 2);
        // Starts with a PAT, then the PMT; nothing of programme 10802, no null packets.
        assert_eq!(&passed[..2], &[PID_PAT, 34]);
        assert!(passed.iter().all(|pid| [PID_PAT, 34, 2301, 2401].contains(pid)));
        assert_eq!(selector.selection().pids, vec![34, 2301, 2401]);
        assert_eq!(selector.selection().pat_written, 3);
    }

    #[test]
    fn elementary_packets_pass_unchanged() {
        let mut selector = Selector::new(10801);
        let mut out = Vec::new();
        let packets = multiplex();
        for p in &packets {
            selector.packet(p, &mut out);
        }
        let video: Vec<&[u8]> = out.chunks(PACKET).filter(|p| pid_of(p) == 2301).collect();
        let original: Vec<&[u8]> = packets.iter().filter(|p| pid_of(p) == 2301).map(|p| p.as_slice()).collect();
        assert_eq!(video, original);
    }

    #[test]
    fn turkish_names_are_read_as_iso_8859_9() {
        assert_eq!(dvb_text(&[0x05, b'T', b'R', b'T', b' ', 0xDE, b'E', b'H', 0xDD, b'R']), "TRT ŞEHİR");
        assert_eq!(dvb_text(b"TRT HABER HD"), "TRT HABER HD");
    }

    #[test]
    fn a_channel_change_starts_from_the_last_access_point() {
        let mut analyser = Analyser::new();
        let mut cache = RapCache::default();
        let mut feed = |packets: &[Vec<u8>], analyser: &mut Analyser, cache: &mut RapCache| {
            for p in packets {
                analyser.feed(p, |_| {});
                cache.learn(&analyser.pat, &analyser.pmts);
                cache.packet(p);
            }
        };
        feed(&multiplex(), &mut analyser, &mut cache);
        assert!(cache.replay(10802).is_empty(), "no access point seen yet");
        // An access point on 10802's video, then two more of its packets.
        let mut rap = packet(2302, true, 0, &[]);
        rap[3] = 0x30; // adaptation field and payload
        rap[4] = 1;
        rap[5] = 0x40; // random_access_indicator
        let tail = [packet(2302, false, 1, &[1]), packet(2301, false, 1, &[2]), packet(2302, false, 2, &[3])];
        feed(&[rap.clone()], &mut analyser, &mut cache);
        feed(&tail, &mut analyser, &mut cache);
        let replay = cache.replay(10802);
        let pids: Vec<u16> = replay.chunks(PACKET).map(pid_of).collect();
        assert_eq!(pids, [PID_PAT, 35, 2302, 2302, 2302], "PAT, PMT, then 10802 from its access point only");
        // Through a selector, the replay is a playable start.
        let mut selector = Selector::new(10802);
        let mut out = Vec::new();
        for p in replay.chunks(PACKET) {
            selector.packet(p, &mut out);
        }
        assert!(selector.ready());
        assert_eq!(out.chunks(PACKET).filter(|p| pid_of(p) == 2302).count(), 3);
    }
}
