//! A film's frame rate, and the display refresh that shows it without judder.
//!
//! A frame rate is a fraction the way a refresh is. The broadcast rates are
//! whole numbers of frames per second and their NTSC twins, one thousand and
//! first of them slower: 24 and 24000/1001, 30 and 30000/1001, 60 and
//! 60000/1001. A player reports a float (`container-fps` is `23.976023...`
//! from a Matroska track, `23.976` from an MP4 time base, `25` from a TS), so
//! the number is read back to the fraction it stands for before anything is
//! compared -- and compared to a refresh as fractions, never as floats.
//!
//! What "shows it without judder" means is written as the rule Kodi and the
//! reference Android box follow: the refresh is a whole multiple of the frame
//! rate, each frame held for the same number of refreshes. When the display
//! lists no such refresh at the chosen size, 3:2 pulldown -- a refresh two
//! and a half times the frame rate, of the same family (23.976 on 59.94, 24
//! on 60) -- is the next best thing, and it is what an unmatched 60 Hz panel
//! does to a 24p film anyway. Anything else is no match, and the display is
//! left alone.

use serde::{Deserialize, Serialize};

use crate::Refresh;

/// How far a reported rate may sit from the fraction it is read as.
///
/// 24 and 23.976 are one part in a thousand apart; a Matroska track reports
/// its NTSC rate to within a few parts in ten million and an MP4 time base to
/// within a few parts in a hundred thousand. One part in five thousand keeps
/// the two families apart with room on both sides.
const SNAP_PPM: u64 = 200;

/// How far a display refresh may sit from a multiple of the frame rate.
///
/// The kernel's refresh is the pixel clock over the totals, and the clock is
/// kept in whole kilohertz: 3840x2160 at 23.976 is 296703 kHz over 5500x2250,
/// 23.97600 Hz against 24000/1001 = 23.97602 -- one part in a million. A
/// hundred parts per million is two orders of magnitude of room and still an
/// order short of the thousand that separates 24 from 23.976.
const MATCH_PPM: u64 = 100;

/// Frames per second, as the fraction they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cadence {
    pub num: u64,
    pub den: u64,
}

impl Cadence {
    /// `num / den`, reduced. `None` for no rate at all.
    pub fn new(num: u64, den: u64) -> Option<Self> {
        if num == 0 || den == 0 {
            return None;
        }
        let divisor = gcd(num, den);
        Some(Self {
            num: num / divisor,
            den: den / divisor,
        })
    }

    /// What a player reported, as the fraction it stands for.
    ///
    /// The broadcast rates -- a whole number of frames, or that number over
    /// 1.001 -- are recognised within [`SNAP_PPM`]. Anything else (a 12.5 fps
    /// animation, a 15 fps screen capture) is kept to the millihertz rather
    /// than forced onto a rate it is not. `None` for a value that is no rate:
    /// zero, negative, not a number, or faster than any display.
    pub fn from_fps(fps: f64) -> Option<Self> {
        if !fps.is_finite() || !(1.0..=480.0).contains(&fps) {
            return None;
        }
        let whole = fps.round() as u64;
        let ntsc = (fps * 1.001).round() as u64;
        let candidates = [
            Self::new(whole, 1),
            Self::new(ntsc * 1000, 1001),
            // The NTSC rate a slower whole number rounds to: 23.976 * 1.001
            // is 24, and 29.97 * 1.001 is 30, but 59.94 read as 59.94 rounds
            // to 60 either way. Listed so neither is ever missed.
            Self::new(whole * 1000, 1001),
        ];
        let reported_mhz = (fps * 1000.0).round() as u64;
        candidates
            .into_iter()
            .flatten()
            .filter(|candidate| {
                let exact_mhz = candidate.num * 1000 / candidate.den;
                ppm(reported_mhz.max(1), exact_mhz.max(1)) <= SNAP_PPM
            })
            .min_by_key(|candidate| {
                let exact_mhz = candidate.num * 1000 / candidate.den;
                reported_mhz.abs_diff(exact_mhz)
            })
            .or_else(|| Self::new(reported_mhz, 1000))
    }

    /// `23.976`, `25`, `59.94`: the spelling a mode label uses.
    pub fn label(&self) -> String {
        Refresh::new(self.num, self.den).label()
    }

    pub fn as_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

/// How a refresh shows a frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CadenceFit {
    /// Every frame for `repeats` refreshes: 24p at 24 Hz is 1, 25p at 50 Hz
    /// is 2.
    Exact { repeats: u32 },
    /// Frames alternately held for three refreshes and for two: 24p at 60 Hz.
    Pulldown32,
}

impl CadenceFit {
    /// Better first: an exact fit beats pulldown, and fewer repeats beat more.
    pub fn rank(self) -> u32 {
        match self {
            CadenceFit::Exact { repeats } => repeats,
            CadenceFit::Pulldown32 => 1000,
        }
    }

    pub fn label(self) -> String {
        match self {
            CadenceFit::Exact { repeats: 1 } => "birebir".into(),
            CadenceFit::Exact { repeats } => format!("{repeats}×"),
            CadenceFit::Pulldown32 => "3:2".into(),
        }
    }
}

/// How `refresh` shows `cadence`, if it does.
pub fn fit(cadence: Cadence, refresh: Refresh) -> Option<CadenceFit> {
    if refresh.num == 0 {
        return None;
    }
    // refresh / cadence as a fraction: (r.num * c.den) / (r.den * c.num).
    let top = u128::from(refresh.num) * u128::from(cadence.den);
    let bottom = u128::from(refresh.den) * u128::from(cadence.num);
    // The nearest whole multiple, and how far the refresh is from it.
    let repeats = (top + bottom / 2) / bottom;
    if repeats >= 1 && repeats <= 16 && close(top, repeats * bottom) {
        return Some(CadenceFit::Exact {
            repeats: repeats as u32,
        });
    }
    // Two and a half: 2 * refresh = 5 * cadence.
    if close(top * 2, bottom * 5) {
        return Some(CadenceFit::Pulldown32);
    }
    None
}

fn close(a: u128, b: u128) -> bool {
    a.abs_diff(b) * 1_000_000 <= b * u128::from(MATCH_PPM)
}

fn ppm(a: u64, b: u64) -> u64 {
    a.abs_diff(b) * 1_000_000 / b
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel(clock_khz: u64, htotal: u64, vtotal: u64) -> Refresh {
        Refresh::new(clock_khz * 1000, htotal * vtotal)
    }

    #[test]
    fn a_reported_rate_is_read_as_the_fraction_it_stands_for() {
        let ntsc = |n: u64| Cadence::new(n * 1000, 1001).unwrap();
        let whole = |n: u64| Cadence::new(n, 1).unwrap();
        // Matroska, MP4 and rounded spellings of the same rates.
        assert_eq!(Cadence::from_fps(23.976023976), Some(ntsc(24)));
        assert_eq!(Cadence::from_fps(23.976), Some(ntsc(24)));
        assert_eq!(Cadence::from_fps(23.98), Some(ntsc(24)));
        assert_eq!(Cadence::from_fps(24.0), Some(whole(24)));
        assert_eq!(Cadence::from_fps(25.0), Some(whole(25)));
        assert_eq!(Cadence::from_fps(29.97002997), Some(ntsc(30)));
        assert_eq!(Cadence::from_fps(29.97), Some(ntsc(30)));
        assert_eq!(Cadence::from_fps(30.0), Some(whole(30)));
        assert_eq!(Cadence::from_fps(50.0), Some(whole(50)));
        assert_eq!(Cadence::from_fps(59.94005994), Some(ntsc(60)));
        assert_eq!(Cadence::from_fps(59.94), Some(ntsc(60)));
        assert_eq!(Cadence::from_fps(60.0), Some(whole(60)));
        // Not a broadcast rate: kept as it is, not forced onto one.
        assert_eq!(Cadence::from_fps(12.5), Cadence::new(25, 2));
        // Not a rate.
        for nonsense in [0.0, -24.0, f64::NAN, f64::INFINITY, 1000.0] {
            assert_eq!(Cadence::from_fps(nonsense), None);
        }
    }

    #[test]
    fn the_two_families_never_meet() {
        let film = Cadence::from_fps(23.976).unwrap();
        let cinema = Cadence::from_fps(24.0).unwrap();
        // The kernel's own 4K timings on the Sony: 23.976, 24, 59.94, 60.
        let r23976 = kernel(296_703, 5500, 2250);
        let r24 = kernel(297_000, 5500, 2250);
        let r5994 = kernel(593_407, 4400, 2250);
        let r60 = kernel(594_000, 4400, 2250);
        assert_eq!(fit(film, r23976), Some(CadenceFit::Exact { repeats: 1 }));
        assert_eq!(fit(film, r24), None);
        assert_eq!(fit(cinema, r24), Some(CadenceFit::Exact { repeats: 1 }));
        assert_eq!(fit(cinema, r23976), None);
        assert_eq!(fit(film, r5994), Some(CadenceFit::Pulldown32));
        assert_eq!(fit(film, r60), None);
        assert_eq!(fit(cinema, r60), Some(CadenceFit::Pulldown32));
        assert_eq!(fit(cinema, r5994), None);
    }

    #[test]
    fn a_multiple_is_a_fit_and_its_repeats_are_counted() {
        let pal = Cadence::from_fps(25.0).unwrap();
        let ntsc_video = Cadence::from_fps(29.97).unwrap();
        let r25 = kernel(297_000, 5280, 2250);
        let r50 = kernel(594_000, 5280, 2250);
        let r2997 = kernel(296_703, 4400, 2250);
        let r5994 = kernel(593_407, 4400, 2250);
        let r60 = kernel(594_000, 4400, 2250);
        assert_eq!(fit(pal, r25), Some(CadenceFit::Exact { repeats: 1 }));
        assert_eq!(fit(pal, r50), Some(CadenceFit::Exact { repeats: 2 }));
        assert_eq!(fit(pal, r60), None);
        assert_eq!(fit(ntsc_video, r2997), Some(CadenceFit::Exact { repeats: 1 }));
        assert_eq!(fit(ntsc_video, r5994), Some(CadenceFit::Exact { repeats: 2 }));
        assert_eq!(fit(ntsc_video, r60), None);
        // 1080p on the same television: 74176 kHz over 2750x1125 is 23.976.
        let hd23976 = kernel(74_176, 2750, 1125);
        assert_eq!(
            fit(Cadence::from_fps(23.976).unwrap(), hd23976),
            Some(CadenceFit::Exact { repeats: 1 })
        );
        assert!(CadenceFit::Exact { repeats: 1 }.rank() < CadenceFit::Exact { repeats: 2 }.rank());
        assert!(CadenceFit::Exact { repeats: 5 }.rank() < CadenceFit::Pulldown32.rank());
    }
}
