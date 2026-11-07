//! A sound stream's bytes out of the transport stream as each packet comes,
//! as a receiver's hardware demultiplexer feeds its audio decoder's buffer.
//!
//! tsdemux hands a sound PES on only once the whole of it is in (its
//! PES_packet_length, never 0 for sound, ISO/IEC 13818-1 2.4.3.7; GStreamer
//! 1.26.2 tsdemux.c, `current_size >= expected_size`). Some services carry
//! 200 to 300 ms of sound in one PES, so its first frame left the
//! demultiplexer 78 to 283 ms after its PTS (Plus with the Kanky,
//! 2026-11-02). Taken packet by packet, a frame is with the parser as soon
//! as its bytes have come.
//!
//! The reader follows ITU-T H.222.0 | ISO/IEC 13818-1 (08/2018); clause
//! numbers below are that edition's:
//!
//! - the transport packet (2.4.3.2/2.4.3.3): transport_error_indicator,
//!   transport_scrambling_control, adaptation_field_control (00 discarded,
//!   10 adaptation only, 11 both), continuity_counter with its wrap, its
//!   non-incrementing packets and duplicate packets (two, and only two,
//!   byte for byte the same but for the PCR);
//! - the adaptation field (2.4.3.4/2.4.3.5): adaptation_field_length 0-182
//!   with a payload and 183 without, its optional fields within it, and the
//!   discontinuity_indicator -- a continuity counter discontinuity on any
//!   PID, a system time-base discontinuity on the PCR_PID;
//! - the PES packet (2.4.3.6/2.4.3.7): packet_start_code_prefix, stream_id
//!   (with or without the optional header), PES_packet_length,
//!   PES_scrambling_control, PTS_DTS_flags ('01' forbidden), PTS/DTS with
//!   their prefixes and marker bits, PES_header_data_length -- the header
//!   gathered over as many packets as it takes;
//! - PAT and PMT (2.4.4.3-2.4.4.9) of the stream's programme, with their
//!   CRC_32: the sound's codec, the PCR_PID, a new version.
//!
//! What goes wrong is told apart (`Event`, `Break`), not one "lost": the
//! parser starts again on any break, the clock only on a new time base.
//!
//! Pure: packets in, payload with its PES's PTS and events out; the engine
//! puts it on the pipeline's clock and into the parser.

use std::collections::{BTreeMap, HashMap};

use crate::stamp::PACKET;

const PID_PAT: u16 = 0x0000;
const PID_NULL: u16 = 0x1FFF;

/// Why a PES's bytes do not run on from those before it: the parser is to
/// start again there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Break {
    /// The first PES given for this PID.
    Start,
    /// The PID was chosen again, or the PMT took it away and gave it back.
    Reset,
    /// A continuity counter discontinuity announced by the
    /// discontinuity_indicator (2.4.3.5): nothing is missing.
    Announced,
    /// The counter jumped unannounced, or a packet repeated its counter
    /// with other bytes: packets are missing.
    Loss,
    /// transport_error_indicator.
    TransportError,
    /// Scrambled: in the transport packet or the PES.
    Scrambled,
    /// An adaptation field out of 2.4.3.4: where the payload starts is not
    /// known.
    MalformedAdaptation,
    /// A PES header out of 2.4.3.7, or a PES shorter or longer than its
    /// PES_packet_length.
    MalformedPes,
}

impl Break {
    /// Whether it is the stream's fault: a PES given after it is a recovery.
    pub fn is_error(self) -> bool {
        !matches!(self, Break::Start | Break::Reset | Break::Announced)
    }
}

/// A run of one PES's payload: the start of a PES (with its PTS/DTS, 90 kHz,
/// if it had them) or more of the one before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub data: Vec<u8>,
    pub pts: Option<u64>,
    pub dts: Option<u64>,
    /// Starts a PES.
    pub start: bool,
    /// The bytes before it are not the ones it follows: why.
    pub discont: Option<Break>,
    /// The first PES after a system time-base discontinuity of the
    /// programme (2.4.3.5): its PTS is on the new time base.
    pub new_time_base: bool,
    /// The first chunk of a sound the PMT moved the reader to.
    pub switch: Option<Switch>,
}

/// What the reader saw, each case on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    TransportError { pid: u16 },
    Scrambled { pid: u16, in_pes: bool },
    /// The counter jumped without the indicator.
    ContinuityLoss { pid: u16, expected: u8, got: u8 },
    /// A packet repeated its counter with other bytes.
    DuplicateMismatch { pid: u16 },
    Duplicate { pid: u16 },
    /// A third (or later) copy: more than 2.4.3.3 allows.
    ExcessiveDuplicate { pid: u16 },
    MalformedAdaptation { pid: u16, why: &'static str },
    MalformedPes { pid: u16, why: &'static str },
    /// A PES of a stream that is not sound (private_stream_2, video, ...):
    /// read past.
    ForeignPes { pid: u16, stream_id: u8 },
    InvalidTimestamp { pid: u16, why: &'static str },
    AnnouncedDiscontinuity { pid: u16 },
    TimeBaseDiscontinuity { pid: u16 },
    /// A PES given again after an error.
    Recovered { pid: u16, after: Break },
    /// A section that failed its CRC or its syntax.
    PsiError { pid: u16, why: &'static str },
    PmtVersion { program: u16, version: u8 },
    PcrPidChanged { from: u16, to: u16 },
    /// The sound's state was set back: chosen again, gone from the PMT,
    /// back in it, or another codec.
    SoundReset { pid: u16, why: &'static str },
}

/// Every event, counted by kind; cumulative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub tei: u64,
    pub scrambled: u64,
    pub cc_loss: u64,
    pub duplicate: u64,
    pub duplicate_excess: u64,
    pub adaptation_bad: u64,
    pub pes_bad: u64,
    pub pes_foreign: u64,
    pub timestamp_bad: u64,
    pub discontinuity_announced: u64,
    pub timebase_discontinuity: u64,
    pub recovered: u64,
    pub pid_reset: u64,
    pub pmt_changes: u64,
    pub pcr_pid_changes: u64,
    pub psi_bad: u64,
}

impl Counts {
    pub fn add(&mut self, event: &Event) {
        let n = match event {
            Event::TransportError { .. } => &mut self.tei,
            Event::Scrambled { .. } => &mut self.scrambled,
            Event::ContinuityLoss { .. } | Event::DuplicateMismatch { .. } => &mut self.cc_loss,
            Event::Duplicate { .. } => &mut self.duplicate,
            Event::ExcessiveDuplicate { .. } => &mut self.duplicate_excess,
            Event::MalformedAdaptation { .. } => &mut self.adaptation_bad,
            Event::MalformedPes { .. } => &mut self.pes_bad,
            Event::ForeignPes { .. } => &mut self.pes_foreign,
            Event::InvalidTimestamp { .. } => &mut self.timestamp_bad,
            Event::AnnouncedDiscontinuity { .. } => &mut self.discontinuity_announced,
            Event::TimeBaseDiscontinuity { .. } => &mut self.timebase_discontinuity,
            Event::Recovered { .. } => &mut self.recovered,
            Event::SoundReset { .. } => &mut self.pid_reset,
            Event::PmtVersion { .. } => &mut self.pmt_changes,
            Event::PcrPidChanged { .. } => &mut self.pcr_pid_changes,
            Event::PsiError { .. } => &mut self.psi_bad,
        };
        *n += 1;
    }

    /// Each counter's sum with `other`'s.
    pub fn plus(mut self, other: &Counts) -> Counts {
        self.tei += other.tei;
        self.scrambled += other.scrambled;
        self.cc_loss += other.cc_loss;
        self.duplicate += other.duplicate;
        self.duplicate_excess += other.duplicate_excess;
        self.adaptation_bad += other.adaptation_bad;
        self.pes_bad += other.pes_bad;
        self.pes_foreign += other.pes_foreign;
        self.timestamp_bad += other.timestamp_bad;
        self.discontinuity_announced += other.discontinuity_announced;
        self.timebase_discontinuity += other.timebase_discontinuity;
        self.recovered += other.recovered;
        self.pid_reset += other.pid_reset;
        self.pmt_changes += other.pmt_changes;
        self.pcr_pid_changes += other.pcr_pid_changes;
        self.psi_bad += other.psi_bad;
        self
    }
}

/// The sound's codec, from its stream_type and descriptors (2.4.4.9 Table
/// 2-34; EN 300 468 6.2.1, 6.2.16, 6.4.3, 6.4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    MpegAudio,
    AacAdts,
    AacLatm,
    Ac3,
    Eac3,
    Dts,
    /// A stream the reader does not take for sound; compared by its
    /// stream_type.
    Other(u8),
}

impl Codec {
    /// As GStreamer 1.26.2's tsdemux names the stream (tsdemux.c,
    /// create_pad_for_stream): stream_type 0x03/0x04 MPEG audio, 0x0F
    /// ADTS, 0x11 LATM, 0x81 AC-3 (E-AC-3 with an enhanced_AC-3
    /// descriptor), 0x87 E-AC-3, and 0x06 with the DVB enhanced_AC-3, AC-3
    /// or DTS descriptor (EN 300 468 6.1 Table 12, Annex D, Annex G).
    fn of(stream_type: u8, descriptors: &[(u8, &[u8])]) -> Self {
        let has = |tag: u8| descriptors.iter().any(|(t, _)| *t == tag);
        match stream_type {
            0x03 | 0x04 => Codec::MpegAudio,
            0x0F => Codec::AacAdts,
            0x11 => Codec::AacLatm,
            0x81 if has(0x7A) => Codec::Eac3,
            0x81 => Codec::Ac3,
            0x87 => Codec::Eac3,
            0x06 if has(0x7A) => Codec::Eac3,
            0x06 if has(0x6A) => Codec::Ac3,
            0x06 if has(0x7B) => Codec::Dts,
            other => Codec::Other(other),
        }
    }

    /// A sound stream: one the reader may follow the sound to.
    pub fn is_sound(self) -> bool {
        !matches!(self, Codec::Other(_))
    }

    /// Whether a PES of `stream_id` can carry it (H.222.0 Table 2-22:
    /// '110x xxxx' for MPEG audio and AAC; ETSI TS 101 154 6.2 and EN 300
    /// 468 Annex D, G: private_stream_1 for AC-3, E-AC-3 and DTS). A PES
    /// of the old codec after the PMT changed it is not the new sound.
    fn carried_by(self, stream_id: u8) -> bool {
        match self {
            Codec::MpegAudio | Codec::AacAdts | Codec::AacLatm => (0xC0..=0xDF).contains(&stream_id),
            Codec::Ac3 | Codec::Eac3 | Codec::Dts => matches!(stream_id, 0xBD | 0xFD),
            Codec::Other(_) => is_sound(stream_id),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::MpegAudio => "mpeg-audio",
            Codec::AacAdts => "aac-adts",
            Codec::AacLatm => "aac-latm",
            Codec::Ac3 => "ac3",
            Codec::Eac3 => "eac3",
            Codec::Dts => "dts",
            Codec::Other(_) => "other",
        }
    }
}

/// Where the sound stands against its programme's PMT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundState {
    /// Given out. Before the PMT is read, too: its PID came from tsdemux,
    /// which had read it.
    Playing,
    /// The PMT in force has no sound stream (a PMT for a moment empty,
    /// 2.4.3.5): nothing is given until one is back, then it is followed.
    Absent,
}

impl SoundState {
    pub fn name(self) -> &'static str {
        match self {
            SoundState::Playing => "playing",
            SoundState::Absent => "absent-from-pmt",
        }
    }
}

/// The PMT moved the sound: to another PID, another codec, or both. Said
/// on the first chunk of the new sound; nothing of the old one comes after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    pub pid: u16,
    pub codec: Codec,
    /// Another codec: the parser and decoder built for the old one cannot
    /// take it.
    pub rebuild: bool,
}

/// What one call to `Reader::feed` gives.
#[derive(Debug, Default)]
pub struct Output {
    pub chunks: Vec<Chunk>,
    pub events: Vec<Event>,
}

/// A section being put together on one PID (2.4.4.1/2.4.4.2), and its
/// packets' continuity.
#[derive(Default)]
struct Sections {
    buffer: Vec<u8>,
    /// A section is being gathered: its bytes are in `buffer`.
    open: bool,
    cc: Option<u8>,
    last: Vec<u8>,
    copies: u8,
}

impl Sections {
    fn clear(&mut self) {
        self.buffer.clear();
        self.open = false;
    }

    /// One packet of the PID: the whole sections it completes. A packet
    /// lost, or a copy that is not one, throws the section being gathered
    /// away (2.4.3.3).
    fn packet(&mut self, pid: u16, p: &[u8], events: &mut Vec<Event>) -> Vec<Vec<u8>> {
        let mut whole = Vec::new();
        if p[1] & 0x80 != 0 {
            self.clear();
            self.cc = None;
            self.last.clear();
            return whole;
        }
        let afc = (p[3] >> 4) & 0x3;
        let cc = p[3] & 0x0F;
        if afc & 0b01 == 0 {
            // No payload: no section bytes, not counted on the counter.
            return whole;
        }
        let af = match adaptation(p, afc) {
            Ok(af) => af,
            Err(why) => {
                events.push(Event::MalformedAdaptation { pid, why });
                self.clear();
                return whole;
            }
        };
        match self.cc {
            Some(last) if last == cc => {
                if same_but_pcr(&self.last, p) {
                    events.push(if self.copies == 0 { Event::Duplicate { pid } } else { Event::ExcessiveDuplicate { pid } });
                    self.copies = self.copies.saturating_add(1);
                } else {
                    events.push(Event::DuplicateMismatch { pid });
                    self.clear();
                    self.last.clear();
                    self.last.extend_from_slice(p);
                    self.copies = 0;
                }
                return whole;
            }
            Some(last) if (last + 1) & 0x0F != cc => {
                events.push(if af.discontinuity { Event::AnnouncedDiscontinuity { pid } } else { Event::ContinuityLoss { pid, expected: (last + 1) & 0x0F, got: cc } });
                self.clear();
            }
            _ => {}
        }
        self.cc = Some(cc);
        self.copies = 0;
        self.last.clear();
        self.last.extend_from_slice(p);
        if p[3] >> 6 != 0 {
            events.push(Event::Scrambled { pid, in_pes: false });
            self.clear();
            return whole;
        }
        let payload = &p[af.payload..];
        if p[1] & 0x40 != 0 {
            let Some(&pointer) = payload.first() else { return whole };
            let pointer = usize::from(pointer);
            if 1 + pointer > payload.len() {
                self.clear();
                events.push(Event::PsiError { pid, why: "pointer_field past the packet" });
                return whole;
            }
            if self.open {
                self.buffer.extend_from_slice(&payload[1..1 + pointer]);
                self.take(pid, &mut whole, events);
            }
            self.buffer = payload[1 + pointer..].to_vec();
            self.open = true;
            self.take(pid, &mut whole, events);
        } else if self.open {
            self.buffer.extend_from_slice(payload);
            self.take(pid, &mut whole, events);
        }
        whole
    }

    /// Whole sections in `buffer`, CRC-checked; stops at stuffing.
    fn take(&mut self, pid: u16, out: &mut Vec<Vec<u8>>, events: &mut Vec<Event>) {
        while self.open && self.buffer.len() >= 3 {
            if self.buffer[0] == 0xFF {
                // Stuffing to the end of the packet (2.4.4.2).
                self.clear();
                return;
            }
            let length = (usize::from(self.buffer[1] & 0x0F) << 8) | usize::from(self.buffer[2]);
            if length > 1021 {
                events.push(Event::PsiError { pid, why: "section_length over 1021" });
                self.clear();
                return;
            }
            if self.buffer.len() < 3 + length {
                return;
            }
            let section: Vec<u8> = self.buffer.drain(..3 + length).collect();
            if section[1] & 0x80 == 0 || length < 9 {
                events.push(Event::PsiError { pid, why: "not a long-form section" });
            } else if crc32(&section) != 0 {
                events.push(Event::PsiError { pid, why: "CRC_32" });
            } else {
                out.push(section);
            }
        }
    }
}

/// CRC_32 of Annex A: zero over a whole section with its CRC.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

/// A descriptor loop (2.6.1): each descriptor's tag, length and bytes whole
/// within it -- or the loop is not to be read.
fn descriptors(bytes: &[u8]) -> Result<Vec<(u8, &[u8])>, &'static str> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if at + 2 > bytes.len() {
            return Err("a descriptor cut short at the end of its loop");
        }
        let length = usize::from(bytes[at + 1]);
        if at + 2 + length > bytes.len() {
            return Err("a descriptor past the end of its loop");
        }
        out.push((bytes[at], &bytes[at + 2..at + 2 + length]));
        at += 2 + length;
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stream {
    pid: u16,
    codec: Codec,
    /// The first language of its ISO_639_language_descriptor (2.6.18).
    language: Option<[u8; 3]>,
}

#[derive(Debug, Clone)]
struct Pmt {
    version: u8,
    pcr_pid: u16,
    /// In the PMT's order.
    streams: Vec<Stream>,
}

/// 2.4.4.8: a TS_program_map_section, CRC already checked. A descriptor
/// loop that does not add up rejects the whole section: a codec is not
/// told from a cut descriptor.
fn parse_pmt(s: &[u8]) -> Result<(u16, Pmt), &'static str> {
    if s[0] != 0x02 {
        return Err("table_id on the PMT PID is not 0x02");
    }
    let end = s.len() - 4;
    if end < 12 {
        return Err("PMT shorter than its fixed fields");
    }
    let program = u16::from_be_bytes([s[3], s[4]]);
    let version = (s[5] >> 1) & 0x1F;
    if s[6] != 0 || s[7] != 0 {
        return Err("PMT section_number/last_section_number not 0");
    }
    let pcr_pid = u16::from_be_bytes([s[8] & 0x1F, s[9]]);
    let info = (usize::from(s[10] & 0x0F) << 8) | usize::from(s[11]);
    let mut at = 12 + info;
    if at > end {
        return Err("program_info_length past the section");
    }
    descriptors(&s[12..at])?;
    let mut streams = Vec::new();
    while at < end {
        if at + 5 > end {
            return Err("an elementary stream entry cut short");
        }
        let stream_type = s[at];
        let pid = u16::from_be_bytes([s[at + 1] & 0x1F, s[at + 2]]);
        let length = (usize::from(s[at + 3] & 0x0F) << 8) | usize::from(s[at + 4]);
        if at + 5 + length > end {
            return Err("ES_info_length past the section");
        }
        let found = descriptors(&s[at + 5..at + 5 + length])?;
        let language = found.iter().find(|(tag, bytes)| *tag == 0x0A && bytes.len() >= 3).map(|(_, b)| [b[0], b[1], b[2]]);
        streams.push(Stream { pid, codec: Codec::of(stream_type, &found), language });
        at += 5 + length;
    }
    Ok((program, Pmt { version, pcr_pid, streams }))
}

/// The adaptation field of a packet: where the payload starts, the
/// discontinuity_indicator, whether a PCR is in it.
struct Adaptation {
    payload: usize,
    discontinuity: bool,
    pcr: bool,
}

/// 2.4.3.4/2.4.3.5 for a packet with adaptation_field_control `afc`.
fn adaptation(p: &[u8], afc: u8) -> Result<Adaptation, &'static str> {
    if afc & 0b10 == 0 {
        return Ok(Adaptation { payload: 4, discontinuity: false, pcr: false });
    }
    let length = usize::from(p[4]);
    if afc == 0b10 && length != 183 {
        return Err("adaptation_field_length not 183 without a payload");
    }
    if afc == 0b11 && length > 182 {
        return Err("adaptation_field_length over 182 with a payload");
    }
    if length == 0 {
        return Ok(Adaptation { payload: 5, discontinuity: false, pcr: false });
    }
    let flags = p[5];
    // The optional fields the flags announce must fit in it; the byte of
    // the field at `need` is p[5 + need].
    let mut need = 1;
    if flags & 0x10 != 0 {
        need += 6; // PCR
    }
    if flags & 0x08 != 0 {
        need += 6; // OPCR
    }
    if flags & 0x04 != 0 {
        need += 1; // splice_countdown
    }
    for flag in [0x02, 0x01] {
        // transport_private_data_length, adaptation_field_extension_length:
        // each followed by that many bytes.
        if flags & flag != 0 {
            if need + 1 > length {
                return Err("adaptation fields past adaptation_field_length");
            }
            need += 1 + usize::from(p[5 + need]);
        }
    }
    if need > length {
        return Err("adaptation fields past adaptation_field_length");
    }
    Ok(Adaptation { payload: 5 + length, discontinuity: flags & 0x80 != 0, pcr: flags & 0x10 != 0 })
}

/// stream_ids whose PES has no optional header (2.4.3.7): program_stream_map,
/// padding_stream, private_stream_2, ECM, EMM, program_stream_directory,
/// DSMCC_stream, ITU-T H.222.1 type E.
fn without_header(stream_id: u8) -> bool {
    matches!(stream_id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF)
}

/// stream_ids sound comes in (Table 2-22): MPEG audio (and AAC, 0xC0-0xDF),
/// private_stream_1 (AC-3, E-AC-3, DTS in DVB), extended_stream_id.
fn is_sound(stream_id: u8) -> bool {
    matches!(stream_id, 0xC0..=0xDF | 0xBD | 0xFD)
}

/// A PTS or DTS (2.4.3.7): its 4-bit prefix and three marker bits.
fn timestamp(b: &[u8], prefix: u8) -> Result<u64, &'static str> {
    if b[0] >> 4 != prefix {
        return Err("PTS/DTS prefix");
    }
    if b[0] & 1 == 0 || b[2] & 1 == 0 || b[4] & 1 == 0 {
        return Err("PTS/DTS marker_bit");
    }
    Ok(((u64::from(b[0] >> 1) & 0x07) << 30) | (u64::from(b[1]) << 22) | ((u64::from(b[2]) >> 1) << 15) | (u64::from(b[3]) << 7) | (u64::from(b[4]) >> 1))
}

/// One PAT section's (program_number, program_map_PID) pairs.
type PatEntries = Vec<(u16, u16)>;

/// A PAT version being gathered: it is put in force only once every section
/// from 0 to its last_section_number is in (2.4.4.5/2.4.4.6).
#[derive(Default)]
struct PatBuild {
    version: Option<u8>,
    last: u8,
    parts: BTreeMap<u8, PatEntries>,
}

enum Pes {
    /// Waiting for a PES to start (payload_unit_start_indicator).
    Idle,
    /// A header being gathered, over as many packets as it takes.
    Header(Vec<u8>),
    /// A PES being given: how much of it is left (None: unbounded), its
    /// times, whether its first byte has gone.
    Body { left: Option<usize>, pts: Option<u64>, dts: Option<u64>, started: bool },
    /// A PES that is not given (padding, foreign): read past to the next.
    Skip,
}

/// The sound being given.
struct Sound {
    pid: u16,
    state: SoundState,
    /// The codec the PMT gives it (None: no PMT yet).
    codec: Option<Codec>,
    /// The programme whose PMT carries it.
    program: Option<u16>,
    /// Its language and place among the programme's sound streams: what
    /// it is followed by when the PMT takes its PID away.
    language: Option<[u8; 3]>,
    index: Option<usize>,
    cc: Option<u8>,
    /// The last packet with a payload, for a duplicate's comparison.
    last: Vec<u8>,
    copies: u8,
    pes: Pes,
    /// Why the next PES does not follow on, if it does not.
    pending: Option<Break>,
    new_time_base: bool,
    /// The PMT moved it: told on the first chunk of the new sound.
    switch: Option<Switch>,
}

impl Sound {
    fn new(pid: u16, first: Break) -> Self {
        Self {
            pid,
            state: SoundState::Playing,
            codec: None,
            program: None,
            language: None,
            index: None,
            cc: None,
            last: Vec::new(),
            copies: 0,
            pes: Pes::Idle,
            pending: Some(first),
            new_time_base: false,
            switch: None,
        }
    }

    /// Whatever PES was being read is not; the next one says why -- the
    /// first error since the last PES given.
    fn broken(&mut self, why: Break) {
        self.pes = Pes::Idle;
        if self.pending.is_none_or(|b| !b.is_error() && why.is_error()) {
            self.pending = Some(why);
        }
    }

    /// The state set back: counter and PES forgotten, not a byte of what
    /// was being read given after.
    fn reset(&mut self) {
        self.cc = None;
        self.last.clear();
        self.copies = 0;
        self.pes = Pes::Idle;
        self.pending = Some(Break::Reset);
    }
}

/// The reader of one channel's stream: its PAT and PMT, its PCR_PID, and
/// the sound's PES, packet by packet. A channel is read by a reader of its
/// own: nothing of the one before carries over.
#[derive(Default)]
pub struct Reader {
    /// program_number -> program_map_PID, from the PAT in force.
    pat: BTreeMap<u16, u16>,
    pat_next: PatBuild,
    sections: HashMap<u16, Sections>,
    pmts: BTreeMap<u16, Pmt>,
    pcr_pid: Option<u16>,
    /// Inside a discontinuity state on the PCR_PID (2.4.3.5): the new
    /// time base is told once, at the PCR that starts it.
    time_base_told: bool,
    sound: Option<Sound>,
    counts: Counts,
}

impl Reader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    pub fn sound_state(&self) -> Option<SoundState> {
        self.sound.as_ref().map(|s| s.state)
    }

    pub fn sound_pid(&self) -> Option<u16> {
        self.sound.as_ref().map(|s| s.pid)
    }

    /// The program_map_PIDs of the PAT in force, and the PCR_PID.
    pub fn pmt_pids(&self) -> Vec<u16> {
        self.pat.values().copied().collect()
    }

    pub fn pcr_pid(&self) -> Option<u16> {
        self.pcr_pid
    }

    pub fn codec(&self) -> Option<Codec> {
        self.sound.as_ref().and_then(|s| s.codec)
    }

    /// The sound to start with: `pid`, as the demultiplexer's pad for the
    /// chosen audio stream names it, the branch built for `built_for`. From
    /// then on the reader follows the PMT itself; another PID chosen here
    /// starts from nothing.
    ///
    /// The reader can be ahead of the demultiplexer: a PMT that changed
    /// right at a channel's start may be in force here while the branch was
    /// built from the one before it. Then the PMT in force is followed at
    /// once (`Switch`) -- the branch is not fed a codec it was not built for.
    pub fn select(&mut self, pid: u16, built_for: Option<Codec>, out: &mut Output) {
        if self.sound.as_ref().is_some_and(|s| s.pid == pid) {
            return;
        }
        let first = match self.sound.take() {
            Some(old) => {
                self.event(Event::SoundReset { pid: old.pid, why: "another PID chosen" }, out);
                Break::Reset
            }
            None => Break::Start,
        };
        let mut sound = Sound::new(pid, first);
        let mut told = Vec::new();
        let found = self.pmts.iter().find(|(_, pmt)| pmt.streams.iter().any(|s| s.pid == pid && s.codec.is_sound()));
        // Not in any PMT in force: the only programme's moved it already.
        let found = found.or_else(|| (self.pmts.len() == 1).then(|| self.pmts.iter().next()).flatten());
        if let Some((&program, pmt)) = found {
            sound.program = Some(program);
            match built_for {
                Some(codec) => {
                    sound.codec = Some(codec);
                    follow(&mut sound, program, pmt, &mut told);
                }
                None => {
                    let sounds: Vec<&Stream> = pmt.streams.iter().filter(|s| s.codec.is_sound()).collect();
                    if let Some(i) = sounds.iter().position(|s| s.pid == pid) {
                        sound.codec = Some(sounds[i].codec);
                        sound.language = sounds[i].language;
                        sound.index = Some(i);
                    }
                }
            }
        }
        // A start is a start, whatever follow found: the reason the first
        // PES gives is that it is the first.
        sound.pending = Some(first);
        self.sound = Some(sound);
        for event in told {
            self.event(event, out);
        }
    }

    /// The viewer's choice: the sound to the `order`-th sound stream of the
    /// programme's PMT in force, from that stream's next PES. Always a new
    /// branch (`Switch::rebuild`), whatever the codec: nothing the old
    /// stream's part still holds may be heard after the new one starts.
    /// None when there is no such stream, or it is the one playing.
    pub fn choose(&mut self, order: usize, out: &mut Output) -> Option<Switch> {
        let sound = self.sound.as_mut()?;
        let program = sound.program.or_else(|| (self.pmts.len() == 1).then(|| self.pmts.keys().next().copied()).flatten())?;
        let pmt = self.pmts.get(&program)?;
        let stream = *pmt.streams.iter().filter(|s| s.codec.is_sound()).nth(order)?;
        if stream.pid == sound.pid && sound.state == SoundState::Playing {
            return None;
        }
        sound.pid = stream.pid;
        sound.reset();
        sound.state = SoundState::Playing;
        sound.program = Some(program);
        sound.codec = Some(stream.codec);
        sound.language = stream.language;
        sound.index = Some(order);
        let switch = Switch { pid: stream.pid, codec: stream.codec, rebuild: true };
        sound.switch = Some(switch);
        self.event(Event::SoundReset { pid: stream.pid, why: "the viewer chose another audio stream" }, out);
        Some(switch)
    }

    fn event(&mut self, event: Event, out: &mut Output) {
        self.counts.add(&event);
        out.events.push(event);
    }

    /// Whole packets, as `stamp::Packetizer` cuts them; bytes out of sync
    /// before them are passed over.
    pub fn feed(&mut self, data: &[u8]) -> Output {
        let mut out = Output::default();
        let mut at = 0;
        while at + PACKET <= data.len() {
            if data[at] != 0x47 || data.get(at + PACKET).is_some_and(|&b| b != 0x47) {
                at += 1;
                continue;
            }
            self.packet(&data[at..at + PACKET], &mut out);
            at += PACKET;
        }
        out
    }

    fn packet(&mut self, p: &[u8], out: &mut Output) {
        let pid = (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]);
        if pid == PID_NULL {
            return;
        }
        let tei = p[1] & 0x80 != 0;
        if pid == PID_PAT || self.pat.values().any(|&pmt| pmt == pid) {
            self.psi(pid, p, out);
        }
        if !tei && Some(pid) == self.pcr_pid {
            self.clock(pid, p, out);
        }
        if let Some(mut sound) = self.sound.take() {
            if sound.pid == pid {
                self.sound_packet(&mut sound, pid, p, tei, out);
            }
            self.sound = Some(sound);
        }
    }

    /// A packet of the PCR_PID: a system time-base discontinuity is the
    /// PCR in a packet whose discontinuity_indicator is set (2.4.3.5).
    fn clock(&mut self, pid: u16, p: &[u8], out: &mut Output) {
        let afc = (p[3] >> 4) & 0x3;
        let Ok(af) = adaptation(p, afc) else { return };
        if !af.discontinuity {
            self.time_base_told = false;
            return;
        }
        if af.pcr && !self.time_base_told {
            self.time_base_told = true;
            self.event(Event::TimeBaseDiscontinuity { pid }, out);
            if let Some(sound) = self.sound.as_mut() {
                sound.new_time_base = true;
            }
        }
    }

    fn psi(&mut self, pid: u16, p: &[u8], out: &mut Output) {
        let mut events = Vec::new();
        let whole = self.sections.entry(pid).or_default().packet(pid, p, &mut events);
        for event in events {
            self.event(event, out);
        }
        for section in whole {
            if section[5] & 0x01 == 0 {
                // current_next_indicator 0: not yet applicable (2.4.4.6,
                // 2.4.4.9); never put in force.
                continue;
            }
            if pid == PID_PAT {
                self.pat_section(&section, out);
            } else {
                match parse_pmt(&section) {
                    Ok((program, pmt)) => self.pmt_section(pid, program, pmt, out),
                    Err(why) => self.event(Event::PsiError { pid, why }, out),
                }
            }
        }
    }

    fn pat_section(&mut self, s: &[u8], out: &mut Output) {
        if s[0] != 0x00 {
            self.event(Event::PsiError { pid: PID_PAT, why: "table_id on PID 0 is not 0x00" }, out);
            return;
        }
        let (version, number, last) = ((s[5] >> 1) & 0x1F, s[6], s[7]);
        if number > last {
            self.event(Event::PsiError { pid: PID_PAT, why: "section_number past last_section_number" }, out);
            return;
        }
        let next = &mut self.pat_next;
        if next.version != Some(version) || next.last != last {
            *next = PatBuild { version: Some(version), last, parts: BTreeMap::new() };
        }
        let end = s.len() - 4;
        let entries = s[8..end].as_chunks::<4>().0.iter().map(|e| (u16::from_be_bytes([e[0], e[1]]), u16::from_be_bytes([e[2] & 0x1F, e[3]])));
        // program_number 0 is the network PID, not a programme.
        next.parts.insert(number, entries.filter(|&(program, _)| program != 0).collect());
        if !(0..=last).all(|n| next.parts.contains_key(&n)) {
            // Not whole yet: the PAT in force stays in force.
            return;
        }
        let pat: BTreeMap<u16, u16> = next.parts.values().flatten().copied().collect();
        // A programme whose PMT moved or went is read again from scratch.
        for (program, pid) in &self.pat {
            if pat.get(program) != Some(pid) {
                self.sections.remove(pid);
                self.pmts.remove(program);
            }
        }
        self.pat = pat;
    }

    fn pmt_section(&mut self, pid: u16, program: u16, pmt: Pmt, out: &mut Output) {
        // 2.4.4.6: the PAT binds each program_number to its PMT's PID.
        if self.pat.get(&program) != Some(&pid) {
            self.event(Event::PsiError { pid, why: "program_number not the PAT's for this PID" }, out);
            return;
        }
        let before = self.pmts.get(&program);
        if before.is_some_and(|b| b.version == pmt.version) {
            return;
        }
        if before.is_some() {
            self.event(Event::PmtVersion { program, version: pmt.version }, out);
        }
        let sound_program = self.sound.as_ref().and_then(|s| s.program);
        let carries_sound = self.sound.as_ref().is_some_and(|s| pmt.streams.iter().any(|st| st.pid == s.pid));
        // The clock is the sound's programme's, or the only one's.
        let ours = sound_program == Some(program) || (sound_program.is_none() && (self.pat.len() <= 1 || carries_sound));
        if ours {
            if let Some(from) = self.pcr_pid
                && from != pmt.pcr_pid
            {
                self.event(Event::PcrPidChanged { from, to: pmt.pcr_pid }, out);
                self.time_base_told = false;
            }
            // 0x1FFF: no PCR for the programme (2.4.4.9).
            self.pcr_pid = (pmt.pcr_pid != PID_NULL).then_some(pmt.pcr_pid);
        }
        let mut told = Vec::new();
        if let Some(sound) = self.sound.as_mut() {
            follow(sound, program, &pmt, &mut told);
        }
        for event in told {
            self.event(event, out);
        }
        self.pmts.insert(program, pmt);
    }

    /// A packet of the chosen sound PID.
    fn sound_packet(&mut self, sound: &mut Sound, pid: u16, p: &[u8], tei: bool, out: &mut Output) {
        if sound.state != SoundState::Playing {
            return;
        }
        if tei {
            // Nothing of the header can be trusted, the counter neither.
            self.event(Event::TransportError { pid }, out);
            sound.broken(Break::TransportError);
            sound.cc = None;
            return;
        }
        let afc = (p[3] >> 4) & 0x3;
        let cc = p[3] & 0x0F;
        if afc == 0b00 {
            // Reserved: decoders shall discard it (2.4.3.3); it does not
            // count on the counter.
            self.event(Event::MalformedAdaptation { pid, why: "adaptation_field_control 00" }, out);
            return;
        }
        let af = adaptation(p, afc);
        let indicator = af.as_ref().is_ok_and(|a| a.discontinuity);
        if afc & 0b01 != 0 {
            match sound.cc {
                Some(last) if (last + 1) & 0x0F == cc => sound.copies = 0,
                Some(last) if last == cc => {
                    // 2.4.3.3: two, and only two, copies, byte for byte but
                    // for the PCR's value.
                    if same_but_pcr(&sound.last, p) {
                        if sound.copies == 0 {
                            self.event(Event::Duplicate { pid }, out);
                        } else {
                            self.event(Event::ExcessiveDuplicate { pid }, out);
                        }
                        sound.copies = sound.copies.saturating_add(1);
                        return;
                    }
                    self.event(Event::DuplicateMismatch { pid }, out);
                    sound.broken(Break::Loss);
                    sound.last.clear();
                    sound.last.extend_from_slice(p);
                    return;
                }
                Some(last) => {
                    sound.copies = 0;
                    if indicator {
                        self.event(Event::AnnouncedDiscontinuity { pid }, out);
                        sound.broken(Break::Announced);
                    } else {
                        self.event(Event::ContinuityLoss { pid, expected: (last + 1) & 0x0F, got: cc }, out);
                        sound.broken(Break::Loss);
                    }
                }
                None => sound.copies = 0,
            }
            sound.cc = Some(cc);
            sound.last.clear();
            sound.last.extend_from_slice(p);
        } else if indicator && sound.cc.is_some_and(|last| last != cc) {
            // Adaptation only, with a new counter: the discontinuity point
            // is here (2.4.3.5); the next packet with a payload carries on
            // from it, whatever its counter.
            self.event(Event::AnnouncedDiscontinuity { pid }, out);
            sound.broken(Break::Announced);
            sound.cc = None;
            return;
        }
        let af = match af {
            Ok(af) => af,
            Err(why) => {
                self.event(Event::MalformedAdaptation { pid, why }, out);
                sound.broken(Break::MalformedAdaptation);
                return;
            }
        };
        if p[3] >> 6 != 0 {
            // Scrambled (2.4.3.3): never for the parser. A scrambled
            // service is not played; it is counted and told.
            self.event(Event::Scrambled { pid, in_pes: false }, out);
            sound.broken(Break::Scrambled);
            return;
        }
        if afc & 0b01 == 0 {
            return;
        }
        let payload = &p[af.payload..];
        if p[1] & 0x40 != 0 {
            self.pes_start(sound, pid, payload, out);
        } else {
            self.pes_more(sound, pid, payload, out);
        }
    }

    fn pes_start(&mut self, sound: &mut Sound, pid: u16, payload: &[u8], out: &mut Output) {
        match &sound.pes {
            Pes::Body { left: Some(left), .. } if *left > 0 => {
                self.event(Event::MalformedPes { pid, why: "PES shorter than its PES_packet_length" }, out);
                sound.broken(Break::MalformedPes);
            }
            Pes::Header(_) => {
                self.event(Event::MalformedPes { pid, why: "PES header cut short by the next PES" }, out);
                sound.broken(Break::MalformedPes);
            }
            _ => {}
        }
        sound.pes = Pes::Header(payload.to_vec());
        self.pes_header(sound, pid, out);
    }

    fn pes_more(&mut self, sound: &mut Sound, pid: u16, payload: &[u8], out: &mut Output) {
        match &mut sound.pes {
            Pes::Idle | Pes::Skip => {}
            Pes::Header(header) => {
                header.extend_from_slice(payload);
                self.pes_header(sound, pid, out);
            }
            Pes::Body { .. } => self.give(sound, pid, payload, out),
        }
    }

    /// The header so far: read when whole, waited for when not.
    fn pes_header(&mut self, sound: &mut Sound, pid: u16, out: &mut Output) {
        let Pes::Header(h) = &sound.pes else { return };
        if h.len() < 6 {
            return;
        }
        if h[0..3] != [0, 0, 1] {
            self.event(Event::MalformedPes { pid, why: "packet_start_code_prefix" }, out);
            sound.broken(Break::MalformedPes);
            return;
        }
        let stream_id = h[3];
        let length = usize::from(u16::from_be_bytes([h[4], h[5]]));
        if stream_id < 0xBC {
            self.event(Event::MalformedPes { pid, why: "stream_id below 0xBC" }, out);
            sound.broken(Break::MalformedPes);
            return;
        }
        if without_header(stream_id) {
            // Its bytes start at the seventh; padding is dropped without a
            // word, anything else is not sound.
            if stream_id != 0xBE {
                self.event(Event::ForeignPes { pid, stream_id }, out);
            }
            sound.pes = Pes::Skip;
            return;
        }
        if h.len() < 9 {
            return;
        }
        if h[6] & 0xC0 != 0x80 {
            self.event(Event::MalformedPes { pid, why: "'10' before PES_scrambling_control" }, out);
            sound.broken(Break::MalformedPes);
            return;
        }
        let flags = h[7];
        let header_length = usize::from(h[8]);
        if h.len() < 9 + header_length {
            return;
        }
        if length != 0 && length < 3 + header_length {
            self.event(Event::MalformedPes { pid, why: "PES_packet_length shorter than its header" }, out);
            sound.broken(Break::MalformedPes);
            return;
        }
        if (h[6] >> 4) & 0x3 != 0 {
            self.event(Event::Scrambled { pid, in_pes: true }, out);
            sound.broken(Break::Scrambled);
            return;
        }
        if !is_sound(stream_id) || sound.codec.is_some_and(|c| !c.carried_by(stream_id)) {
            self.event(Event::ForeignPes { pid, stream_id }, out);
            sound.pes = Pes::Skip;
            return;
        }
        let pts_dts = flags >> 6;
        if pts_dts == 0b01 {
            self.event(Event::InvalidTimestamp { pid, why: "PTS_DTS_flags '01'" }, out);
            sound.broken(Break::MalformedPes);
            return;
        }
        let mut need = match pts_dts {
            0b10 => 5,
            0b11 => 10,
            _ => 0,
        };
        // ESCR, ES_rate, DSM_trick_mode, additional_copy_info,
        // previous_PES_CRC, PES_extension (at least its flags byte).
        for (bit, bytes) in [(0x20, 6), (0x10, 3), (0x08, 1), (0x04, 1), (0x02, 2), (0x01, 1)] {
            if flags & bit != 0 {
                need += bytes;
            }
        }
        if need > header_length {
            self.event(Event::MalformedPes { pid, why: "optional fields past PES_header_data_length" }, out);
            sound.broken(Break::MalformedPes);
            return;
        }
        let times = match pts_dts {
            0b10 => timestamp(&h[9..14], 0b0010).map(|pts| (Some(pts), None)),
            0b11 => timestamp(&h[9..14], 0b0011).and_then(|pts| timestamp(&h[14..19], 0b0001).map(|dts| (Some(pts), Some(dts)))),
            _ => Ok((None, None)),
        };
        let (pts, dts) = match times {
            Ok(times) => times,
            Err(why) => {
                self.event(Event::InvalidTimestamp { pid, why }, out);
                sound.broken(Break::MalformedPes);
                return;
            }
        };
        let left = if length == 0 {
            // 0 is for video only (2.4.3.7); read to the next PES.
            self.event(Event::MalformedPes { pid, why: "PES_packet_length 0 on sound" }, out);
            None
        } else {
            Some(length - 3 - header_length)
        };
        let rest = h[9 + header_length..].to_vec();
        sound.pes = Pes::Body { left, pts, dts, started: false };
        self.give(sound, pid, &rest, out);
    }

    /// PES payload out, as far as its PES_packet_length goes.
    fn give(&mut self, sound: &mut Sound, pid: u16, bytes: &[u8], out: &mut Output) {
        let Pes::Body { left, pts, dts, started } = &mut sound.pes else { return };
        let (pts, dts, was_started) = (*pts, *dts, *started);
        let mut bytes = bytes;
        let mut long = false;
        if let Some(left) = left {
            if bytes.len() > *left {
                bytes = &bytes[..*left];
                long = true;
            }
            *left -= bytes.len();
        }
        if !bytes.is_empty() {
            *started = true;
        }
        self.emit(sound, pid, bytes, pts, dts, was_started, out);
        if long {
            self.event(Event::MalformedPes { pid, why: "PES longer than its PES_packet_length" }, out);
            sound.broken(Break::MalformedPes);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(&mut self, sound: &mut Sound, pid: u16, bytes: &[u8], pts: Option<u64>, dts: Option<u64>, started: bool, out: &mut Output) {
        if bytes.is_empty() {
            return;
        }
        if started {
            match out.chunks.last_mut() {
                Some(last) => last.data.extend_from_slice(bytes),
                None => out.chunks.push(Chunk { data: bytes.to_vec(), pts: None, dts: None, start: false, discont: None, new_time_base: false, switch: None }),
            }
            return;
        }
        let discont = sound.pending.take();
        if let Some(after) = discont.filter(|b| b.is_error()) {
            self.event(Event::Recovered { pid, after }, out);
        }
        let new_time_base = std::mem::take(&mut sound.new_time_base);
        let switch = sound.switch.take();
        out.chunks.push(Chunk { data: bytes.to_vec(), pts, dts, start: true, discont, new_time_base, switch });
    }
}

/// The sound against a new PMT of `program`: kept, moved to another PID or
/// codec, or held while the PMT has none. Where its PID is gone it is
/// followed to the stream of its language and codec, of its language, at
/// its place, or the first (the order a viewer's choice was made by).
fn follow(sound: &mut Sound, program: u16, pmt: &Pmt, told: &mut Vec<Event>) {
    let sounds: Vec<&Stream> = pmt.streams.iter().filter(|s| s.codec.is_sound()).collect();
    match sound.program {
        Some(p) if p != program => return,
        None => {
            if let Some(i) = sounds.iter().position(|s| s.pid == sound.pid) {
                sound.program = Some(program);
                sound.codec = Some(sounds[i].codec);
                sound.language = sounds[i].language;
                sound.index = Some(i);
            }
            return;
        }
        Some(_) => {}
    }
    let target = sounds
        .iter()
        .position(|s| s.pid == sound.pid)
        .or_else(|| sound.language.and_then(|l| sounds.iter().position(|s| s.language == Some(l) && Some(s.codec) == sound.codec)))
        .or_else(|| sound.language.and_then(|l| sounds.iter().position(|s| s.language == Some(l))))
        .or_else(|| sound.index.filter(|&i| i < sounds.len()))
        .or_else(|| (!sounds.is_empty()).then_some(0));
    let Some(i) = target else {
        if sound.state == SoundState::Playing {
            sound.state = SoundState::Absent;
            sound.pes = Pes::Idle;
            told.push(Event::SoundReset { pid: sound.pid, why: "no sound stream in the PMT" });
        }
        return;
    };
    let stream = *sounds[i];
    let moved = stream.pid != sound.pid;
    let recoded = sound.codec.is_some_and(|c| c != stream.codec);
    if moved || recoded {
        sound.pid = stream.pid;
        sound.reset();
        sound.state = SoundState::Playing;
        sound.switch = Some(Switch { pid: stream.pid, codec: stream.codec, rebuild: recoded });
        told.push(Event::SoundReset { pid: stream.pid, why: if recoded { "the PMT gave the sound another codec" } else { "the PMT moved the sound to another PID" } });
    } else if sound.state == SoundState::Absent {
        sound.reset();
        sound.state = SoundState::Playing;
        told.push(Event::SoundReset { pid: sound.pid, why: "the sound is back in the PMT" });
    }
    sound.codec = Some(stream.codec);
    sound.language = stream.language;
    sound.index = Some(i);
}

/// 2.4.3.3: a duplicate is byte for byte its original, but a PCR in it
/// carries its own valid value.
fn same_but_pcr(a: &[u8], b: &[u8]) -> bool {
    if a.len() != PACKET || b.len() != PACKET {
        return false;
    }
    let pcr = (b[3] & 0x20 != 0 && b[4] >= 7 && b[5] & 0x10 != 0).then_some(6..12);
    a.iter().zip(b).enumerate().all(|(i, (x, y))| x == y || pcr.as_ref().is_some_and(|r| r.contains(&i)))
}

#[cfg(test)]
#[path = "es_tests.rs"]
pub(crate) mod spec;
