//! "Arayüz ve Hareket": everything about how the interface looks and moves,
//! kept in one place.
//!
//! One file, `interface-preferences.json`, beside the session file -- the
//! file that already held the face (`layout.rs`): the face, Quiet OS's
//! motion (the parameters the Motion Lab turns, `QuietMotion` in
//! ui/quiet/motion.slint), and the opening (ui/quiet/intro.slint). Read once
//! at start, written whole whenever any of it changes, from the settings or
//! from the lab, so the settings and the lab are never two answers to the
//! same question.
//!
//! Every default is what the interface did before there was a choice, and a
//! value this build does not know -- or one outside what still reads as
//! motion -- is the default for that field alone.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::Layout;

/// The Motion Lab's easing profiles (`QuietMotion.profile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    #[default]
    Product,
    Swift,
    Soft,
}

impl Curve {
    pub const ALL: [Curve; 3] = [Curve::Product, Curve::Swift, Curve::Soft];

    pub fn label(self) -> &'static str {
        match self {
            Curve::Product => "Ürün",
            Curve::Swift => "Çevik",
            Curve::Soft => "Yumuşak",
        }
    }

    pub fn profile(self) -> i32 {
        self as i32
    }

    pub fn from_profile(profile: i32) -> Self {
        Self::ALL.get(profile.rem_euclid(3) as usize).copied().unwrap_or_default()
    }
}

/// Quiet OS's motion: the Motion Lab's parameters, and the two switches the
/// settings add to them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    #[serde(default = "one", deserialize_with = "pace")]
    pub pace: f32,
    #[serde(default = "one", deserialize_with = "amount")]
    pub depth: f32,
    #[serde(default = "one", deserialize_with = "amount")]
    pub travel: f32,
    #[serde(default = "one", deserialize_with = "amount")]
    pub glow: f32,
    #[serde(default = "one", deserialize_with = "amount")]
    pub parallax: f32,
    #[serde(default, deserialize_with = "or_default")]
    pub curve: Curve,
    #[serde(default, deserialize_with = "or_default")]
    pub reduced: bool,
    /// The launcher's marks move as their row is taken (ui/quiet/marks.slint).
    #[serde(default = "yes", deserialize_with = "or_true")]
    pub icons: bool,
}

impl Default for Motion {
    fn default() -> Self {
        Self { pace: 1.0, depth: 1.0, travel: 1.0, glow: 1.0, parallax: 1.0, curve: Curve::Product, reduced: false, icons: true }
    }
}

/// What the Motion Lab keeps each parameter within (src/motion_lab.rs).
pub const PACE: (f32, f32) = (0.3, 3.0);
pub const AMOUNT: (f32, f32) = (0.0, 2.5);

impl Motion {
    /// Onto the properties every Quiet OS screen animates with.
    pub fn apply(&self, motion: &crate::QuietMotion<'_>) {
        motion.set_pace(self.pace);
        motion.set_depth(self.depth);
        motion.set_travel(self.travel);
        motion.set_glow(self.glow);
        motion.set_parallax(self.parallax);
        motion.set_profile(self.curve.profile());
        motion.set_reduced(self.reduced);
        motion.set_icons(self.icons);
    }

    /// As the properties are now: after the lab has turned one.
    pub fn read(motion: &crate::QuietMotion<'_>) -> Self {
        Self {
            pace: motion.get_pace(),
            depth: motion.get_depth(),
            travel: motion.get_travel(),
            glow: motion.get_glow(),
            parallax: motion.get_parallax(),
            curve: Curve::from_profile(motion.get_profile()),
            reduced: motion.get_reduced(),
            icons: motion.get_icons(),
        }
    }
}

/// One of the Motion Lab's turned parameters, as the settings offer it: a
/// short list of steps rather than a knob, because a remote's Left and Right
/// already mean "leave this card".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionParam {
    Pace,
    Depth,
    Travel,
    Glow,
    Parallax,
}

impl MotionParam {
    pub const ALL: [MotionParam; 5] =
        [MotionParam::Pace, MotionParam::Depth, MotionParam::Travel, MotionParam::Glow, MotionParam::Parallax];

    /// The lab's own names (ui/quiet/lab.slint).
    pub fn label(self) -> &'static str {
        match self {
            MotionParam::Pace => "Süre",
            MotionParam::Depth => "Derinlik",
            MotionParam::Travel => "Hareket mesafesi",
            MotionParam::Glow => "Atmosfer ışığı",
            MotionParam::Parallax => "Katman ayrışması",
        }
    }

    pub fn line(self) -> &'static str {
        match self {
            MotionParam::Pace => "Bütün geçişlerin süresi; büyük değer daha yavaş",
            MotionParam::Depth => "Sahnelerin derinliğe ne kadar çekildiği",
            MotionParam::Travel => "Kameranın uygulamalar arasında aldığı yol",
            MotionParam::Glow => "Zeminin seçili uygulamanın ışığını alması",
            MotionParam::Parallax => "Uzak, ana ve yakın katmanların ayrışması",
        }
    }

    /// The steps offered, in hundredths.
    pub fn steps(self) -> &'static [i32] {
        match self {
            MotionParam::Pace => &[50, 75, 100, 125, 150, 200],
            _ => &[0, 50, 75, 100, 125, 150, 200],
        }
    }

    pub fn get(self, motion: &Motion) -> f32 {
        match self {
            MotionParam::Pace => motion.pace,
            MotionParam::Depth => motion.depth,
            MotionParam::Travel => motion.travel,
            MotionParam::Glow => motion.glow,
            MotionParam::Parallax => motion.parallax,
        }
    }

    /// Set to `hundredths`, within the lab's bounds.
    pub fn set(self, motion: &mut Motion, hundredths: i32) {
        let (lo, hi) = if self == MotionParam::Pace { PACE } else { AMOUNT };
        let value = (hundredths as f32 / 100.0).clamp(lo, hi);
        match self {
            MotionParam::Pace => motion.pace = value,
            MotionParam::Depth => motion.depth = value,
            MotionParam::Travel => motion.travel = value,
            MotionParam::Glow => motion.glow = value,
            MotionParam::Parallax => motion.parallax = value,
        }
    }

    /// The step nearest what it is now: where its list opens. The lab turns
    /// in tenths, so the value may be between two steps.
    pub fn nearest(self, motion: &Motion) -> usize {
        let now = (self.get(motion) * 100.0).round() as i32;
        let steps = self.steps();
        (0..steps.len()).min_by_key(|&i| (steps[i] - now).abs()).unwrap_or(0)
    }
}

/// "× 1.25", as the lab writes it.
pub fn times(value: f32) -> String {
    let hundredths = (value * 100.0).round() as i32;
    let text = format!("{}.{:02}", hundredths / 100, hundredths % 100);
    format!("× {}", text.trim_end_matches('0').trim_end_matches('.'))
}

/// When the opening plays, once it is part of the boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntroPolicy {
    /// Every time the box is switched on; never on a restart of the
    /// interface alone, nor on a return from Kodi or the browser.
    #[default]
    EveryBoot,
    /// The first time the box is switched on each day.
    FirstOfDay,
    /// Only the first time after the interface was updated.
    AfterUpdate,
}

impl IntroPolicy {
    pub const ALL: [IntroPolicy; 3] = [IntroPolicy::EveryBoot, IntroPolicy::FirstOfDay, IntroPolicy::AfterUpdate];

    pub fn label(self) -> &'static str {
        match self {
            IntroPolicy::EveryBoot => "Her açılışta",
            IntroPolicy::FirstOfDay => "Günde bir kez",
            IntroPolicy::AfterUpdate => "Güncellemeden sonra",
        }
    }

    pub fn line(self) -> &'static str {
        match self {
            IntroPolicy::EveryBoot => "Cihaz her açıldığında oynar; arayüz yeniden başlarken oynamaz.",
            IntroPolicy::FirstOfDay => "Günün ilk açılışında oynar.",
            IntroPolicy::AfterUpdate => "Yalnız arayüz güncellendikten sonraki ilk açılışta oynar.",
        }
    }
}

/// What the remote can do to an opening that is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntroSkip {
    #[default]
    AnyKey,
    OkOrBack,
    Never,
}

impl IntroSkip {
    pub const ALL: [IntroSkip; 3] = [IntroSkip::AnyKey, IntroSkip::OkOrBack, IntroSkip::Never];

    pub fn label(self) -> &'static str {
        match self {
            IntroSkip::AnyKey => "Herhangi bir tuş",
            IntroSkip::OkOrBack => "Tamam veya Geri",
            IntroSkip::Never => "Atlanamaz",
        }
    }

    pub fn line(self) -> &'static str {
        match self {
            IntroSkip::AnyKey => "Kumandada basılan ilk tuş açılışı bitirir.",
            IntroSkip::OkOrBack => "Yalnız Tamam ve Geri bitirir; yön tuşları yok sayılır.",
            IntroSkip::Never => "Açılış sonuna kadar oynar.",
        }
    }

    /// Whether this key ends it.
    pub fn skips(self, ok_or_back: bool) -> bool {
        match self {
            IntroSkip::AnyKey => true,
            IntroSkip::OkOrBack => ok_or_back,
            IntroSkip::Never => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intro {
    #[serde(default = "yes", deserialize_with = "or_true")]
    pub enabled: bool,
    #[serde(default, deserialize_with = "or_default")]
    pub policy: IntroPolicy,
    #[serde(default, deserialize_with = "or_default")]
    pub skip: IntroSkip,
}

impl Default for Intro {
    fn default() -> Self {
        Self { enabled: true, policy: IntroPolicy::EveryBoot, skip: IntroSkip::AnyKey }
    }
}

/// A switch-on the opening may follow: which boot it is, which day, which
/// build. Not yet asked by the boot -- the opening is a preview until it
/// is approved -- but the policy is decided here, and tested, so wiring it
/// is a call and not a design.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Switch {
    pub boot: String,
    pub day: i64,
    pub version: String,
}

impl Intro {
    /// Whether the opening plays at `now`, after it last played at `last`.
    /// A restart of the interface within one boot never plays it again.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn plays(&self, reduced: bool, last: Option<&Switch>, now: &Switch) -> bool {
        if !self.enabled || reduced {
            return false;
        }
        let Some(last) = last else { return true };
        if last.boot == now.boot {
            return false;
        }
        match self.policy {
            IntroPolicy::EveryBoot => true,
            IntroPolicy::FirstOfDay => last.day != now.day,
            IntroPolicy::AfterUpdate => last.version != now.version,
        }
    }
}

/// The whole of `interface-preferences.json`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Interface {
    #[serde(default, deserialize_with = "or_default")]
    pub layout: Layout,
    #[serde(default, deserialize_with = "or_default")]
    pub motion: Motion,
    #[serde(default, deserialize_with = "or_default")]
    pub intro: Intro,
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn or_true<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_bool().unwrap_or(true))
}

fn within<'de, D: serde::Deserializer<'de>>(deserializer: D, (lo, hi): (f32, f32)) -> Result<f32, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_f64().map(|v| v as f32).filter(|v| v.is_finite() && (lo..=hi).contains(v)).unwrap_or(1.0))
}

fn pace<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    within(deserializer, PACE)
}

fn amount<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    within(deserializer, AMOUNT)
}

pub fn read(path: impl AsRef<Path>) -> Interface {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Interface>(&text).ok())
        .unwrap_or_default()
}

/// Written whole and atomically, off the event loop, by the one ordered
/// writer every preference file shares (`prefs::write_in_background`).
pub fn write(path: PathBuf, interface: Interface) {
    crate::prefs::write_in_background(path, interface);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box that never chose, or whose file is broken, is the interface as
    /// it was: the classic face, the product's motion, the opening on.
    #[test]
    fn nothing_chosen_is_the_interface_as_it_was() {
        let none = Interface::default();
        assert_eq!(none.layout, Layout::Classic);
        assert_eq!(none.motion, Motion { pace: 1.0, depth: 1.0, travel: 1.0, glow: 1.0, parallax: 1.0, curve: Curve::Product, reduced: false, icons: true });
        assert_eq!(none.intro, Intro { enabled: true, policy: IntroPolicy::EveryBoot, skip: IntroSkip::AnyKey });
        for broken in ["", "{not json", "[]", "{\"motion\": 3}"] {
            let read: Interface = serde_json::from_str(broken).unwrap_or_default();
            assert_eq!(read.motion, Motion::default(), "{broken}");
        }
    }

    /// The file the face was kept in before there was anything else to keep
    /// is read as it was meant.
    #[test]
    fn the_file_of_the_face_alone_still_reads() {
        let read: Interface = serde_json::from_str("{\"layout\":\"quiet\"}").unwrap();
        assert_eq!(read, Interface { layout: Layout::Quiet, ..Default::default() });
    }

    /// One field this build does not know, or turned past what the lab
    /// allows, is that field's default; the rest is as written.
    #[test]
    fn a_wrong_value_costs_its_own_field_only() {
        let text = r#"{
            "layout": "quiet",
            "motion": {"pace": 9.0, "depth": 1.6, "travel": "far", "glow": 0.0, "curve": "bouncy", "reduced": true, "icons": "x"},
            "intro": {"enabled": false, "policy": "weekly", "skip": "ok_or_back"}
        }"#;
        let read: Interface = serde_json::from_str(text).unwrap();
        assert_eq!(read.layout, Layout::Quiet);
        assert_eq!(read.motion.pace, 1.0, "past the lab's bound");
        assert_eq!(read.motion.depth, 1.6);
        assert_eq!(read.motion.travel, 1.0);
        assert_eq!(read.motion.glow, 0.0, "the bottom of the range is in it");
        assert_eq!(read.motion.curve, Curve::Product);
        assert!(read.motion.reduced);
        assert!(read.motion.icons);
        assert!(!read.intro.enabled);
        assert_eq!(read.intro.policy, IntroPolicy::EveryBoot);
        assert_eq!(read.intro.skip, IntroSkip::OkOrBack);
    }

    #[test]
    fn written_whole_it_reads_back_whole() {
        let dir = std::env::temp_dir().join(format!("mediabox-tv-appearance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("interface-preferences.json");
        assert_eq!(read(&path), Interface::default(), "no file");
        let kept = Interface {
            layout: Layout::Quiet,
            motion: Motion { pace: 1.3, parallax: 0.5, curve: Curve::Soft, icons: false, ..Default::default() },
            intro: Intro { enabled: false, policy: IntroPolicy::FirstOfDay, skip: IntroSkip::Never },
        };
        for n in 0..20 {
            write(path.clone(), Interface { motion: Motion { glow: n as f32 / 10.0, ..kept.motion }, ..kept });
        }
        write(path.clone(), kept);
        crate::prefs::flush();
        assert_eq!(read(&path), kept);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_profile_and_its_curve_are_one_number() {
        for curve in Curve::ALL {
            assert_eq!(Curve::from_profile(curve.profile()), curve);
        }
        assert_eq!(Curve::from_profile(-1), Curve::Soft);
    }

    /// The policies, against the boots: the same boot never twice, and each
    /// policy's own condition otherwise.
    #[test]
    fn when_the_opening_plays() {
        let at = |boot: &str, day: i64, version: &str| Switch { boot: boot.into(), day, version: version.into() };
        let last = at("a", 100, "1.0");
        let intro = |policy| Intro { policy, ..Default::default() };
        // Never on a restart of the interface within the boot.
        for policy in IntroPolicy::ALL {
            assert!(!intro(policy).plays(false, Some(&last), &at("a", 100, "1.0")), "{policy:?}");
            assert!(intro(policy).plays(false, None, &at("a", 100, "1.0")), "never played: {policy:?}");
        }
        assert!(intro(IntroPolicy::EveryBoot).plays(false, Some(&last), &at("b", 100, "1.0")));
        assert!(!intro(IntroPolicy::FirstOfDay).plays(false, Some(&last), &at("b", 100, "1.0")));
        assert!(intro(IntroPolicy::FirstOfDay).plays(false, Some(&last), &at("b", 101, "1.0")));
        assert!(!intro(IntroPolicy::AfterUpdate).plays(false, Some(&last), &at("b", 101, "1.0")));
        assert!(intro(IntroPolicy::AfterUpdate).plays(false, Some(&last), &at("b", 100, "1.1")));
        // Off, or reduced motion: never.
        assert!(!Intro { enabled: false, ..Default::default() }.plays(false, None, &last));
        assert!(!Intro::default().plays(true, None, &last));
    }

    #[test]
    fn the_steps_stay_within_the_lab_and_say_what_they_are() {
        let mut motion = Motion::default();
        for param in MotionParam::ALL {
            for &step in param.steps() {
                param.set(&mut motion, step);
                assert_eq!((param.get(&motion) * 100.0).round() as i32, step.max(30 * (param == MotionParam::Pace) as i32));
            }
            assert_eq!(param.steps()[param.nearest(&Motion::default())], 100, "{param:?} opens on × 1");
        }
        MotionParam::Depth.set(&mut motion, 900);
        assert_eq!(motion.depth, AMOUNT.1);
        assert_eq!(times(1.0), "× 1");
        assert_eq!(times(1.25), "× 1.25");
        assert_eq!(times(0.5), "× 0.5");
        assert_eq!(times(1.3), "× 1.3");
    }

    #[test]
    fn what_ends_it() {
        assert!(IntroSkip::AnyKey.skips(false) && IntroSkip::AnyKey.skips(true));
        assert!(!IntroSkip::OkOrBack.skips(false) && IntroSkip::OkOrBack.skips(true));
        assert!(!IntroSkip::Never.skips(false) && !IntroSkip::Never.skips(true));
    }
}
