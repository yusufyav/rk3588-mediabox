//! The reader's sound through real GStreamer parsers: mpegaudioparse,
//! aacparse and ac3parse, fed the way the sound branch is fed. The stream
//! goes TS -> `es::Reader` -> `route_sound` -> appsrc with `sound_caps` ->
//! `parser_for` -> appsink. The frames are real ones (`fixtures/README.md`),
//! cut into PES and TS packets at every kind of place.
//!
//! The parsers are those of the GStreamer the player runs on: 1.26.2 on the
//! Plus. Run for aarch64 under qemu-aarch64 with the target sysroot's
//! libraries and plugins. A parser missing is a failure, not a skip.

use std::sync::{Arc, Mutex};

use gst::prelude::*;

use super::{decoder_for, parser_for, route_sound, sound_caps};
use crate::es::spec::{flat, pes, pmt, Mux, Rng, AUDIO, PCR_PID, PMT_PID, PROGRAM};
use crate::es::{Codec, Reader};
use crate::stamp::PACKET;

struct Case {
    codec: Codec,
    stream_type: u8,
    descriptors: Vec<u8>,
    stream_id: u8,
    file: &'static [u8],
    /// Frames in the file (`ffprobe -count_frames`, fixtures/README.md).
    count: usize,
    /// Samples per frame at 48 kHz: the frame's duration.
    samples: u64,
}

fn cases() -> Vec<Case> {
    vec![
        Case { codec: Codec::MpegAudio, stream_type: 0x03, descriptors: vec![], stream_id: 0xC0, file: include_bytes!("fixtures/sound.mp2"), count: 9, samples: 1152 },
        Case { codec: Codec::AacAdts, stream_type: 0x0F, descriptors: vec![], stream_id: 0xC0, file: include_bytes!("fixtures/sound.adts"), count: 11, samples: 1024 },
        Case { codec: Codec::AacLatm, stream_type: 0x11, descriptors: vec![], stream_id: 0xC0, file: include_bytes!("fixtures/sound.latm"), count: 11, samples: 1024 },
        Case { codec: Codec::Ac3, stream_type: 0x06, descriptors: vec![0x6A, 1, 0], stream_id: 0xBD, file: include_bytes!("fixtures/sound.ac3"), count: 7, samples: 1536 },
        Case { codec: Codec::Eac3, stream_type: 0x06, descriptors: vec![0x7A, 1, 0], stream_id: 0xBD, file: include_bytes!("fixtures/sound.eac3"), count: 7, samples: 1536 },
    ]
}

/// The file's frames, each found from its own header.
fn frames(codec: Codec, data: &[u8]) -> Vec<Vec<u8>> {
    // ISO/IEC 11172-3 Layer II bit rates (kbit/s), 2.4.2.3.
    const MP2_KBPS: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384];
    // ATSC A/52 Table 5.18, 48 kHz: words per frame by frmsizecod / 2.
    const AC3_WORDS: [usize; 19] = [64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1152, 1280];
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let b = &data[at..];
        let length = match codec {
            Codec::MpegAudio => {
                assert_eq!((b[0], b[1] & 0xFE), (0xFF, 0xFC), "MPEG-1 Layer II sync at {at}");
                let kbps = MP2_KBPS[usize::from(b[2] >> 4)];
                let rate = [44_100, 48_000, 32_000][usize::from((b[2] >> 2) & 3)];
                (144_000 * kbps / rate + u32::from((b[2] >> 1) & 1)) as usize
            }
            Codec::AacAdts => {
                assert_eq!((b[0], b[1] & 0xF6), (0xFF, 0xF0), "ADTS sync at {at}");
                (usize::from(b[3] & 3) << 11) | (usize::from(b[4]) << 3) | usize::from(b[5] >> 5)
            }
            Codec::AacLatm => {
                assert_eq!((b[0], b[1] & 0xE0), (0x56, 0xE0), "LOAS sync at {at}");
                3 + ((usize::from(b[1] & 0x1F) << 8) | usize::from(b[2]))
            }
            Codec::Ac3 => {
                assert_eq!((b[0], b[1]), (0x0B, 0x77), "AC-3 sync at {at}");
                assert_eq!(b[4] >> 6, 0, "48 kHz");
                2 * AC3_WORDS[usize::from(b[4] & 0x3F) / 2]
            }
            Codec::Eac3 => {
                assert_eq!((b[0], b[1]), (0x0B, 0x77), "E-AC-3 sync at {at}");
                2 * (((usize::from(b[2] & 7) << 8) | usize::from(b[3])) + 1)
            }
            other => panic!("no frames for {other:?}"),
        };
        out.push(b[..length].to_vec());
        at += length;
    }
    out
}

/// appsrc -> the parser -> appsink, as the sound branch has it.
struct Chain {
    pipeline: gst::Pipeline,
    source: gst_app::AppSrc,
    out: Arc<Mutex<Vec<gst::Sample>>>,
    /// The decoder's own output caps.
    decoded: Arc<Mutex<Option<gst::Caps>>>,
}

impl Chain {
    fn new(codec: Codec) -> Self {
        Self::build(codec, false, false)
    }

    /// The parser and then the decoder the sound branch takes for the caps
    /// (`decoder_for`): PCM out.
    fn decoding(codec: Codec) -> Self {
        Self::build(codec, true, false)
    }

    /// And then converted as the PCM branch converts it, to the sound
    /// clock's format (`audio_clock::contract_filter`).
    fn to_contract(codec: Codec) -> Self {
        Self::build(codec, true, true)
    }

    fn build(codec: Codec, decode: bool, contract: bool) -> Self {
        gst::init().expect("GStreamer");
        let caps = sound_caps(codec).expect("caps for the codec");
        let parser = parser_for(&caps).expect("a parser for the caps");
        let parser = gst::ElementFactory::make(parser)
            .build()
            .unwrap_or_else(|_| panic!("{parser} is not in this GStreamer: the test fails, it is not skipped"));
        let source = gst_app::AppSrc::builder().caps(&caps).format(gst::Format::Time).build();
        let sink = gst_app::AppSink::builder().sync(false).build();
        let pipeline = gst::Pipeline::new();
        let mut chain: Vec<gst::Element> = vec![source.clone().upcast(), parser];
        if decode {
            let decoder = decoder_for(&caps)
                .iter()
                .find_map(|factory| gst::ElementFactory::make(factory).build().ok())
                .unwrap_or_else(|| panic!("{codec:?}: none of {:?} is in this GStreamer", decoder_for(&caps)));
            chain.push(decoder);
        }
        if contract {
            for factory in ["audioconvert", "audioresample"] {
                chain.push(gst::ElementFactory::make(factory).build().expect(factory));
            }
            chain.push(crate::audio_clock::contract_filter().expect("the contract's filter"));
        }
        chain.push(sink.clone().upcast());
        pipeline.add_many(&chain).expect("added");
        gst::Element::link_many(&chain).expect("linked");
        let out = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&out);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    into.lock().unwrap().push(sample);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        let decoded = Arc::new(Mutex::new(None));
        if decode {
            let into = Arc::clone(&decoded);
            chain[2].static_pad("src").expect("the decoder's output").add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                if let Some(gst::PadProbeData::Event(e)) = &info.data
                    && let gst::EventView::Caps(c) = e.view()
                {
                    *into.lock().unwrap() = Some(c.caps_owned());
                }
                gst::PadProbeReturn::Ok
            });
        }
        pipeline.set_state(gst::State::Playing).expect("playing");
        Self { pipeline, source, out, decoded }
    }

    /// To its end: what the parser gave, and the bus's errors.
    fn finish(self) -> (Vec<gst::Sample>, Vec<String>) {
        let _ = self.source.end_of_stream();
        let bus = self.pipeline.bus().expect("a bus");
        let mut errors = Vec::new();
        loop {
            let Some(message) = bus.timed_pop_filtered(gst::ClockTime::from_seconds(20), &[gst::MessageType::Eos, gst::MessageType::Error]) else {
                errors.push("no EOS within 20 s".to_string());
                break;
            };
            match message.view() {
                gst::MessageView::Error(e) => errors.push(format!("{} ({:?})", e.error(), e.debug())),
                _ => break,
            }
        }
        self.pipeline.set_state(gst::State::Null).expect("stopped");
        let out = std::mem::take(&mut *self.out.lock().unwrap());
        (out, errors)
    }
}

/// The first frame's PTS, 90 kHz, and the PCR the clock is reckoned from:
/// the first frame plays at 100 ms of running time.
const BASE: u64 = 900_000;
const PCR0: u64 = (BASE - 9_000) * 300;
const REFERENCE: (u64, u64) = (0, PCR0);

fn frame_pts(case: &Case, k: usize) -> u64 {
    BASE + k as u64 * case.samples * 90_000 / 48_000
}

fn frame_ns(case: &Case, k: usize) -> u64 {
    super::pts_on_clock(frame_pts(case, k), REFERENCE)
}

/// The elementary stream cut into PES at `cuts` (byte offsets), each PES
/// with the PTS of the first frame that starts in it (2.4.3.7), its TS
/// packets carrying `sizes` bytes of it at random.
fn ts(case: &Case, frames: &[Vec<u8>], cuts: &[usize], rng: &mut Rng, mux: &mut Mux) -> Vec<Vec<u8>> {
    let es: Vec<u8> = frames.concat();
    let mut starts = Vec::new();
    let mut at = 0;
    for f in frames {
        starts.push(at);
        at += f.len();
    }
    let mut bounds: Vec<usize> = cuts.iter().copied().filter(|&c| c > 0 && c < es.len()).collect();
    bounds.insert(0, 0);
    bounds.push(es.len());
    bounds.dedup();
    let mut packets = Vec::new();
    for w in bounds.windows(2) {
        let (from, to) = (w[0], w[1]);
        let pts = starts.iter().position(|&s| s >= from && s < to).map(|k| frame_pts(case, k));
        let unit = pes(case.stream_id, pts, None, rng.below(24) as usize, &es[from..to], None);
        let sizes: Vec<usize> = (0..6).map(|_| 1 + rng.below(184) as usize).collect();
        packets.extend(mux.pes(AUDIO, &unit, &sizes));
    }
    packets
}

/// Through the reader and into a chain, in batches of a few packets.
fn play(case: &Case, packets: Vec<Vec<u8>>, rng: &mut Rng, mux: &mut Mux) -> (Vec<gst::Sample>, Vec<String>) {
    play_into(Chain::new(case.codec), case, packets, rng, mux)
}

fn play_into(chain: Chain, case: &Case, packets: Vec<Vec<u8>>, rng: &mut Rng, mux: &mut Mux) -> (Vec<gst::Sample>, Vec<String>) {
    let mut reader = Reader::new();
    let mut out = crate::es::Output::default();
    reader.select(AUDIO, None, &mut out);
    let mut stream = mux.tables(0, PCR_PID, &[(case.stream_type, AUDIO, case.descriptors.clone())]);
    stream.extend(flat(packets));
    let mut target = Some(chain.source.clone());
    let mut at = 0;
    while at < stream.len() {
        let n = ((1 + rng.below(6) as usize) * PACKET).min(stream.len() - at);
        let fed = reader.feed(&stream[at..at + n]);
        route_sound(&mut target, fed.chunks, Some(REFERENCE), false, &mut |_| panic!("no PMT change here"), &mut |_| {});
        at += n;
    }
    chain.finish()
}

fn data(sample: &gst::Sample) -> Vec<u8> {
    sample.buffer().expect("a buffer").map_readable().expect("readable").to_vec()
}

/// The caps a parser of the codec must give for these 48 kHz stereo
/// frames.
fn caps_are_right(case: &Case, caps: &gst::CapsRef) {
    let s = caps.structure(0).expect("a structure");
    let (name, framed) = match case.codec {
        Codec::MpegAudio => ("audio/mpeg", "parsed"),
        Codec::AacAdts | Codec::AacLatm => ("audio/mpeg", "framed"),
        Codec::Ac3 => ("audio/x-ac3", "framed"),
        Codec::Eac3 => ("audio/x-eac3", "framed"),
        _ => unreachable!(),
    };
    assert_eq!(s.name().as_str(), name, "{caps:?}");
    assert_eq!(s.get::<bool>(framed), Ok(true), "{caps:?}");
    assert_eq!(s.get::<i32>("rate"), Ok(48_000), "{caps:?}");
    assert_eq!(s.get::<i32>("channels"), Ok(2), "{caps:?}");
    match case.codec {
        Codec::MpegAudio => {
            assert_eq!(s.get::<i32>("mpegversion"), Ok(1), "{caps:?}");
            assert_eq!(s.get::<i32>("layer"), Ok(2), "{caps:?}");
        }
        Codec::AacAdts => assert_eq!(s.get::<&str>("stream-format"), Ok("adts"), "{caps:?}"),
        Codec::AacLatm => assert_eq!(s.get::<&str>("stream-format"), Ok("loas"), "{caps:?}"),
        _ => {}
    }
}

/// Every frame out, byte for byte, in order, at its time; DISCONT only on
/// the first.
fn whole(case: &Case, frames: &[Vec<u8>], samples: &[gst::Sample], label: &str) {
    assert_eq!(samples.len(), frames.len(), "{label}: frames out");
    for (k, sample) in samples.iter().enumerate() {
        assert_eq!(data(sample), frames[k], "{label}: frame {k}'s bytes");
        let buffer = sample.buffer().unwrap();
        let pts = buffer.pts().expect("a PTS").nseconds();
        // 90 kHz against ns: within a microsecond.
        assert!(pts.abs_diff(frame_ns(case, k)) <= 1_000, "{label}: frame {k} at {pts}, not {}", frame_ns(case, k));
        assert_eq!(buffer.flags().contains(gst::BufferFlags::DISCONT), k == 0, "{label}: frame {k}'s DISCONT");
        caps_are_right(case, sample.caps().expect("caps"));
    }
}

#[test]
fn every_codec_through_its_real_parser_at_every_kind_of_cut() {
    for case in cases() {
        let frames = frames(case.codec, case.file);
        assert_eq!(frames.len(), case.count, "{:?}: frames in the file", case.codec);
        assert_eq!(frames.concat(), case.file, "{:?}", case.codec);
        let starts: Vec<usize> = frames.iter().scan(0, |at, f| {
            let s = *at;
            *at += f.len();
            Some(s)
        }).collect();
        let total: usize = case.file.len();
        let mut rng = Rng(0xC0DE_0001 ^ case.stream_type as u64);
        let mut plans: Vec<(String, Vec<usize>)> = vec![
            ("a PES per frame".into(), starts.clone()),
            // Each PES starts in the middle of a frame: the PTS is the
            // next frame's, the one that starts in it (2.4.3.7).
            ("PES cut mid-frame".into(), starts.iter().map(|&s| s + 37).collect()),
            // Cut inside the codec's sync word and header.
            ("PES cut in the codec header".into(), starts.iter().skip(1).map(|&s| s + 1).collect()),
            ("one PES".into(), vec![]),
        ];
        for n in 0..6 {
            let mut cuts: Vec<usize> = (0..1 + rng.below(8)).map(|_| rng.below(total as u64) as usize).collect();
            cuts.sort_unstable();
            plans.push((format!("random cuts {n}"), cuts));
        }
        for (label, cuts) in plans {
            let label = format!("{:?}, {label}", case.codec);
            let mut mux = Mux::default();
            let packets = ts(&case, &frames, &cuts, &mut rng, &mut mux);
            let (samples, errors) = play(&case, packets, &mut rng, &mut mux);
            assert!(errors.is_empty(), "{label}: {errors:?}");
            whole(&case, &frames, &samples, &label);
        }
    }
}

/// Each codec decoded by the decoder the sound branch chooses for it, one
/// PES holding every frame: PCM out, no error. LATM went to avdec_aac,
/// which refuses aacparse's `loas`: not-negotiated, no sound at all on a
/// DVB LATM channel (2026-11-06, codec-latm-dvb.ts on the Plus).
#[test]
fn every_codec_decodes_with_the_decoder_its_branch_takes() {
    for case in cases() {
        let frames = frames(case.codec, case.file);
        let mut rng = Rng(0xDEC0_0001 ^ case.stream_type as u64);
        let mut mux = Mux::default();
        let packets = ts(&case, &frames, &[], &mut rng, &mut mux);
        let (samples, errors) = play_into(Chain::decoding(case.codec), &case, packets, &mut rng, &mut mux);
        assert!(errors.is_empty(), "{:?}: {errors:?}", case.codec);
        let pcm: usize = samples.iter().map(|sample| data(sample).len()).sum();
        let caps = samples.first().and_then(|sample| sample.caps().map(|caps| caps.to_string())).unwrap_or_default();
        assert!(caps.starts_with("audio/x-raw"), "{:?}: decoded to {caps}", case.codec);
        assert!(pcm > 0, "{:?}: no PCM", case.codec);
    }
}

#[test]
fn a_lost_packet_restarts_the_parser_at_the_next_pes() {
    for case in cases() {
        let frames = frames(case.codec, case.file);
        let starts: Vec<usize> = frames.iter().scan(0, |at, f| {
            let s = *at;
            *at += f.len();
            Some(s)
        }).collect();
        let mut rng = Rng(0xC0DE_0002 ^ case.stream_type as u64);
        let mut mux = Mux::default();
        // A PES per frame, each in packets of 100 bytes; a packet of the
        // third frame's PES lost.
        let mut packets = Vec::new();
        let es: Vec<u8> = frames.concat();
        let mut lost_frame = None;
        for (k, f) in frames.iter().enumerate() {
            let unit = pes(case.stream_id, Some(frame_pts(&case, k)), None, 0, f, None);
            let mut these = mux.pes(AUDIO, &unit, &[100]);
            if k == 2 && these.len() > 1 {
                these.remove(1);
                lost_frame = Some(k);
            }
            packets.extend(these);
        }
        assert_eq!(es.len(), starts.last().unwrap() + frames.last().unwrap().len());
        let lost = lost_frame.expect("a frame of more than one packet");
        let (samples, errors) = play(&case, packets, &mut rng, &mut mux);
        let label = format!("{:?}", case.codec);
        assert!(errors.is_empty(), "{label}: {errors:?}");
        // The damaged frame never comes out; every other one does, whole,
        // at its time; the first after the loss is a DISCONT.
        let got: Vec<usize> = samples
            .iter()
            .map(|s| {
                let pts = s.buffer().unwrap().pts().unwrap().nseconds();
                (0..frames.len()).min_by_key(|&k| pts.abs_diff(frame_ns(&case, k))).unwrap()
            })
            .collect();
        let expected: Vec<usize> = (0..frames.len()).filter(|&k| k != lost).collect();
        assert_eq!(got, expected, "{label}: frames out");
        for (sample, &k) in samples.iter().zip(&got) {
            assert_eq!(data(sample), frames[k], "{label}: frame {k}'s bytes");
            assert!(sample.buffer().unwrap().pts().unwrap().nseconds().abs_diff(frame_ns(&case, k)) <= 1_000, "{label}: frame {k}'s time");
            let discont = sample.buffer().unwrap().flags().contains(gst::BufferFlags::DISCONT);
            assert_eq!(discont, k == 0 || k == lost + 1, "{label}: frame {k}'s DISCONT");
            caps_are_right(&case, sample.caps().unwrap());
        }
    }
}

/// The PMT moves the sound while it plays: the reader follows it, and
/// `route_sound` takes the new sound to the source `on_switch` gives -- for
/// another codec a new chain, as `Engine::sound_changed` builds a new
/// branch, for the same codec the same one.
#[test]
fn the_sound_follows_the_pmt_into_a_new_parser_or_the_same() {
    let all = cases();
    let mp2 = &all[0];
    let ac3 = &all[3];
    let mp2_frames = frames(mp2.codec, mp2.file);
    let ac3_frames = frames(ac3.codec, ac3.file);
    for move_pid in [false, true] {
        let label = if move_pid { "AC-3 on another PID" } else { "AC-3 on the same PID" };
        let new_pid = if move_pid { 0x102 } else { AUDIO };
        let mut mux = Mux::default();
        let mut stream = mux.tables(0, PCR_PID, &[(mp2.stream_type, AUDIO, vec![])]);
        for (k, f) in mp2_frames.iter().enumerate().take(5) {
            stream.extend(flat(mux.pes(AUDIO, &pes(0xC0, Some(frame_pts(mp2, k)), None, 0, f, None), &[100])));
        }
        // Half a frame more of MP2, then the PMT: the rest of it, and a
        // whole frame after, are not the sound any more.
        let cut = mux.pes(AUDIO, &pes(0xC0, Some(frame_pts(mp2, 5)), None, 0, &mp2_frames[5], None), &[100]);
        stream.extend(flat(cut[..2].to_vec()));
        stream.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x06, new_pid, vec![0x6A, 1, 0])])));
        stream.extend(flat(cut[2..].to_vec()));
        stream.extend(flat(mux.pes(AUDIO, &pes(0xC0, Some(frame_pts(mp2, 6)), None, 0, &mp2_frames[6], None), &[184])));
        let base = frame_pts(mp2, 7);
        for (k, f) in ac3_frames.iter().enumerate() {
            stream.extend(flat(mux.pes(new_pid, &pes(0xBD, Some(base + k as u64 * 2_880), None, 0, f, None), &[100])));
        }
        let first = Chain::new(Codec::MpegAudio);
        let mut built: Vec<Chain> = Vec::new();
        let mut reader = Reader::new();
        let mut out = crate::es::Output::default();
        reader.select(AUDIO, None, &mut out);
        let mut target = Some(first.source.clone());
        let mut switches = Vec::new();
        for piece in stream.chunks(3 * PACKET) {
            let fed = reader.feed(piece);
            route_sound(
                &mut target,
                fed.chunks,
                Some(REFERENCE),
                false,
                &mut |switch| {
                    switches.push(*switch);
                    assert!(switch.rebuild, "{label}: another codec is built anew");
                    let chain = Chain::new(switch.codec);
                    let source = chain.source.clone();
                    built.push(chain);
                    Some(source)
                },
                &mut |_| {},
            );
        }
        assert_eq!(switches.len(), 1, "{label}");
        assert_eq!((switches[0].pid, switches[0].codec), (new_pid, Codec::Ac3), "{label}");
        let (old, errors) = first.finish();
        assert!(errors.is_empty(), "{label}: {errors:?}");
        // The first chain: MP2 frames 0-4 only, not a byte after the PMT.
        assert_eq!(old.iter().map(data).collect::<Vec<_>>(), mp2_frames[..5].to_vec(), "{label}: the MP2 parser");
        let (new, errors) = built.pop().expect("a chain built").finish();
        assert!(errors.is_empty(), "{label}: {errors:?}");
        assert_eq!(new.iter().map(data).collect::<Vec<_>>(), ac3_frames, "{label}: the AC-3 parser");
        for (k, sample) in new.iter().enumerate() {
            let buffer = sample.buffer().unwrap();
            assert_eq!(buffer.flags().contains(gst::BufferFlags::DISCONT), k == 0, "{label}: AC-3 frame {k}'s DISCONT");
            let at = super::pts_on_clock(base + k as u64 * 2_880, REFERENCE);
            assert!(buffer.pts().unwrap().nseconds().abs_diff(at) <= 1_000, "{label}: AC-3 frame {k}'s time");
            caps_are_right(ac3, sample.caps().unwrap());
        }
    }

    // The same codec on another PID: the same parser goes on, the new
    // PID's first frame a DISCONT.
    let mut mux = Mux::default();
    let mut stream = mux.tables(0, PCR_PID, &[(mp2.stream_type, AUDIO, vec![])]);
    for (k, f) in mp2_frames.iter().enumerate().take(4) {
        stream.extend(flat(mux.pes(AUDIO, &pes(0xC0, Some(frame_pts(mp2, k)), None, 0, f, None), &[184])));
    }
    stream.extend(mux.section(PMT_PID, &pmt(PROGRAM, 1, PCR_PID, &[(0x04, 0x102, vec![])])));
    for (k, f) in mp2_frames.iter().enumerate().skip(4) {
        stream.extend(flat(mux.pes(AUDIO, &pes(0xC0, Some(frame_pts(mp2, k)), None, 0, f, None), &[184])));
        stream.extend(flat(mux.pes(0x102, &pes(0xC0, Some(frame_pts(mp2, k)), None, 0, f, None), &[184])));
    }
    let chain = Chain::new(Codec::MpegAudio);
    let mut reader = Reader::new();
    let mut out = crate::es::Output::default();
    reader.select(AUDIO, None, &mut out);
    let mut target = Some(chain.source.clone());
    let same = chain.source.clone();
    let fed = reader.feed(&stream);
    route_sound(
        &mut target,
        fed.chunks,
        Some(REFERENCE),
        false,
        &mut |switch| {
            assert!(!switch.rebuild && switch.pid == 0x102);
            Some(same.clone())
        },
        &mut |_| {},
    );
    let (samples, errors) = chain.finish();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(samples.iter().map(data).collect::<Vec<_>>(), mp2_frames, "frames 0-3 from 0x101, 4-8 from 0x102");
    for (k, sample) in samples.iter().enumerate() {
        assert_eq!(sample.buffer().unwrap().flags().contains(gst::BufferFlags::DISCONT), k == 0 || k == 4, "frame {k}'s DISCONT");
    }
}

/// The GStreamer these tests run on is the one mediabox-live is built for
/// (scripts/build-gstreamer-runtime.sh): 1.26.8 or later, where aacparse
/// keeps a LATM StreamMuxConfig (upstream a34d9006f5). Not Debian trixie's
/// 1.26.2.
#[test]
fn the_gstreamer_is_the_one_built_for_mediabox_live() {
    gst::init().expect("GStreamer");
    let (major, minor, micro, _) = gst::version();
    assert!((major, minor) == (1, 26) && micro >= 8, "GStreamer {major}.{minor}.{micro}: run scripts/test-mediabox-live-arm64.sh");
    let parser = gst::ElementFactory::find("aacparse").expect("aacparse");
    let plugin = parser.plugin().expect("its plugin");
    assert_eq!(plugin.version().as_str(), format!("{major}.{minor}.{micro}"), "aacparse of the same release");
}

/// DVB repeats the LATM StreamMuxConfig only now and then (ETSI TS 101 154
/// V2.10.1 6.5.1.1). Every frame comes out, the ones that use the config
/// before them too -- 1.26.2's aacparse lost frames 1-3 here
/// (gstaacparse.c 272-273, 739-741; fixed in 1.26.8 by a34d9006f5).
#[test]
fn latm_with_a_stream_mux_config_now_and_then_comes_out_whole() {
    let case = Case {
        codec: Codec::AacLatm,
        stream_type: 0x11,
        descriptors: vec![],
        stream_id: 0xC0,
        file: include_bytes!("fixtures/sound-smc4.latm"),
        count: 11,
        samples: 1024,
    };
    let frames = frames(case.codec, case.file);
    assert_eq!(frames.len(), case.count);
    let starts: Vec<usize> = frames.iter().scan(0, |at, f| {
        let s = *at;
        *at += f.len();
        Some(s)
    }).collect();
    let mut rng = Rng(0xC0DE_0003);
    for (label, cuts) in [("a PES per frame", starts.clone()), ("PES cut mid-frame", starts.iter().map(|&s| s + 37).collect()), ("one PES", vec![])] {
        let mut mux = Mux::default();
        let packets = ts(&case, &frames, &cuts, &mut rng, &mut mux);
        let (samples, errors) = play(&case, packets, &mut rng, &mut mux);
        assert!(errors.is_empty(), "{label}: {errors:?}");
        whole(&case, &frames, &samples, label);
    }
}

#[test]
fn every_broadcast_decoders_pcm_reaches_the_sound_clocks_format_at_its_own_rate_and_channels() {
    let mut converted = 0;
    for case in cases() {
        let chain = Chain::to_contract(case.codec);
        let decoded = Arc::clone(&chain.decoded);
        for (k, frame) in frames(case.codec, case.file).into_iter().enumerate() {
            let mut buffer = gst::Buffer::from_mut_slice(frame);
            buffer.get_mut().unwrap().set_pts(gst::ClockTime::from_nseconds(frame_ns(&case, k)));
            chain.source.push_buffer(buffer).expect("pushed");
        }
        let (samples, errors) = chain.finish();
        assert!(errors.is_empty(), "{:?}: {errors:?}", case.codec);
        let decoded = decoded.lock().unwrap().clone().expect("the decoder's caps");
        let d = decoded.structure(0).unwrap();
        let (rate, channels) = (d.get::<i32>("rate").unwrap() as u32, d.get::<i32>("channels").unwrap() as usize);
        if crate::audio_clock::admit(&decoded).is_err() {
            converted += 1;
        }
        let last = samples.last().unwrap_or_else(|| panic!("{:?}: no PCM", case.codec));
        assert_eq!(crate::audio_clock::admit(last.caps().unwrap()), Ok(crate::audio_clock::Format { rate, channels }), "{:?}: {decoded}", case.codec);
    }
    // Not all S16 interleaved already: the conversion is what is tested.
    assert!(converted > 0, "every decoder gave the contract's format as it is");
}
