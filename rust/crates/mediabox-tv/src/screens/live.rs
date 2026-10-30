//! Live television: the screen is the broadcast, and everything else is an
//! overlay that comes when a key asks for it and goes by itself.
//!
//! The behaviour is AOSP's Live TV reference application
//! (platform/packages/apps/TV, Apache-2.0), read from its source, not from
//! its screenshots:
//!
//! * `MainActivity.onKeyDown/onKeyUp` -- with nothing over the picture, Up
//!   and Down (and the channel keys) change channel, Ok brings the banner and
//!   the menu, a digit opens the number entry, Back leaves.
//! * `TvOverlayManager.onKeyUp` -- Back closes whatever overlay is up before
//!   it can mean anything else; the guide, the side panels and the menu each
//!   own the keys while they are up; a digit over the menu goes to the number
//!   entry.
//! * `ChannelBannerView` -- number, programme, channel name, programme time
//!   and how far into it, for `channel_banner_show_duration` (8 s).
//! * `KeypadChannelSwitchView` -- up to five digits, candidates whose number
//!   is the typed one first, then those it begins; the first is selected,
//!   and `keypad_channel_switch_show_duration` (2 s) after the last key the
//!   selection is tuned to (nothing typed matched: the entry just closes).
//! * `menu/Menu` -- rows of cards, "Kanallar" (the guide first, then the
//!   channels watched recently) and "TV seçenekleri"; `menu_show_duration`
//!   (7 s) after the last key it goes.
//! * `guide/ProgramGuide`, `ProgramGrid`, `ProgramManager` -- channel rows on
//!   a horizontal time line that starts on the half hour before now, a gap
//!   with no information shown as one ("No information" there, "Bilgi yok"
//!   here), Up and Down keeping the time the focus is at, Ok on what is on
//!   now tuning that channel, `program_guide_show_duration` (60 s).
//!
//! * `ui/sidepanel/CustomizeChannelListFragment` -- the channel list in
//!   groups. The reference groups by source or by HD/SD; here the groups are
//!   television, radio and TKGS's own categories, since a satellite list is
//!   Türksat's, and radio is a third of it.
//! * `TunableTvView` (`VIDEO_UNAVAILABLE_REASON_AUDIO_ONLY`) -- a radio
//!   channel is black with "Yalnızca ses" (the reference's own Turkish).
//! * `tuner/setup/ScanFragment`, `ScanResultFragment` -- the channel setup:
//!   a start, a scan that says how far it has got and can be stopped, and a
//!   result ("N kanal bulundu", Bitti / Yeniden tara). The scan here reads
//!   TKGS; with no channels at all, live television opens on it.
//!
//! What is not taken: time-shift play controls, recordings, PIP, inputs,
//! parental PINs -- MediaBox has none of them, and a card for any would be a
//! card for something that does not work.
//!
//! Plain state, as every screen here: no socket, no Slint. Time is passed in
//! -- `at` for the overlays' lifetimes, `now` (UTC seconds) for the guide --
//! so the tests below are the behaviour.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use mediabox_core::{InputAction, LiveChannel, LiveEvent, LiveScanKeep, LiveSource, SatellitePolarisation};

pub const BANNER_SHOW: Duration = Duration::from_millis(8000);
pub const KEYPAD_SHOW: Duration = Duration::from_millis(2000);
pub const MENU_SHOW: Duration = Duration::from_millis(7000);
pub const GUIDE_SHOW: Duration = Duration::from_secs(60);
pub const PANEL_SHOW: Duration = Duration::from_secs(60);

/// `KeypadChannelSwitchView.MAX_CHANNEL_NUMBER_DIGIT` and `MAX_CHANNEL_ITEM`.
pub const MAX_DIGITS: usize = 5;
pub const MAX_CANDIDATES: usize = 8;
/// The channels the menu's first row offers after the guide.
pub const RECENT: usize = 6;

/// The guide's time line: a half-hour grid, its view the width the
/// reference's table has at 384 dp an hour (`program_guide_table_width_per_hour`)
/// on a 960 dp screen less its margin and channel column: 1 h 45 min.
pub const HALF_HOUR: i64 = 1800;
pub const GUIDE_VIEW: i64 = 6300;
/// How far ahead the guide goes.
pub const GUIDE_HORIZON: i64 = 2 * 86_400;
/// `ProgramGuide.MIN_DURATION_FROM_START_TIME_TO_CURRENT_TIME`.
/// How long after the last Up or Down the channel the banner shows is tuned:
/// past a held key's repeats (the dispatcher's 110 ms step) and short of
/// AOSP's 500 ms before its own fast stepping starts.
pub const ZAP_SETTLE: Duration = Duration::from_millis(350);
const GUIDE_LEAD: i64 = 60;
/// `FOCUS_AREA_RIGHT_MARGIN_MILLIS`: the focus is kept this far from the
/// right edge.
const GUIDE_RIGHT_MARGIN: i64 = 900;
/// Rows on the panel at once, and the one the focus parks on
/// (`program_guide_selection_row`).
pub const GUIDE_ROWS: usize = 5;
const GUIDE_SELECTION_ROW: usize = 2;
/// Rows of a side panel at once.
pub const PANEL_ROWS: usize = 9;

/// What owns the remote, over the picture. The banner is not one: it is
/// read, and comes and goes beside any of these (Ok shows it with the menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    None,
    Keypad,
    Menu,
    Guide,
    Panel(Panel),
    /// The channel setup, the whole screen.
    Setup,
}

/// A group of the channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Group {
    Television,
    Radio,
    /// A TKGS category, by name.
    Category(String),
}

impl Group {
    pub fn title(&self) -> &str {
        match self {
            Group::Television => "TV kanalları",
            Group::Radio => "Radyo",
            Group::Category(name) => name,
        }
    }

    pub fn holds(&self, channel: &LiveChannel) -> bool {
        match self {
            Group::Television => !channel.radio,
            Group::Radio => channel.radio,
            Group::Category(name) => channel.categories.iter().any(|c| c == name),
        }
    }
}

/// Where the channel setup is. Its pages are the receivers' own (TCL's
/// "Kanal Tarama": Otomatik arama / Manuel tarama / Turksat TKGS ayarları);
/// only what works here has a row.
#[derive(Debug, Clone, PartialEq)]
pub enum SetupStage {
    /// "Kanal kurulumu": the ways to set the channels up.
    Menu,
    /// "TKGS ayarları".
    Settings,
    /// "Manuel tarama": the transponder, and what of it to keep.
    /// The satellite scan's settings (Sony's satellite detail screen).
    Satellite,
    Manual,
    /// The TKGS read or a manual scan, under way.
    Running(Run),
    /// The TKGS read is in: its channels, and the table's own text.
    TkgsDone { television: usize, radio: usize, title: String },
    /// A manual scan is in: what it kept, and of that what is new to the
    /// list; the first new channel, television first.
    ScanDone { found: usize, added: usize, first: Option<u16> },
    Failed { scan: bool, error: String },
}

/// How far a search has got, as the control plane says.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Run {
    /// A manual scan, not the TKGS read.
    pub scan: bool,
    /// TKGS: the receiver is on the multiplex and reading the table, its
    /// `sections` of `expected` had. The table is one block: its channels
    /// are known only once all of it is in.
    pub reading: bool,
    pub sections: u64,
    pub expected: u64,
    /// A scan: the services the multiplex has named so far, and the
    /// demodulator's word on the carrier.
    pub found: u64,
    pub locked: bool,
    pub strength_pct: Option<u64>,
    pub snr_db: Option<f64>,
    /// A satellite scan: the carrier on (from 1) of `carriers`, its
    /// frequency, and the channels added so far.
    pub carrier: u64,
    pub carriers: u64,
    pub frequency_khz: u64,
    pub added: u64,
    /// The channels found so far, all carriers (Sony's "Dijital kanallar").
    pub found_total: u64,
    /// The services found so far, all carriers, by AOSP's service type:
    /// television, radio, data and other.
    pub kinds: [u64; 3],
}

/// The manual scan's form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualForm {
    /// MHz and kS/s, as typed.
    pub frequency: String,
    pub symbol_rate: String,
    pub vertical: bool,
    /// "Tarama türü" (Sony's Scan type): 0 all, 1 only free, 2 only
    /// encrypted.
    pub access: u8,
    pub keep: LiveScanKeep,
    /// The next digit starts the focused number over.
    pub fresh: bool,
}

impl ManualForm {
    pub fn source(&self) -> Option<LiveSource> {
        let frequency: u32 = self.frequency.parse().ok()?;
        let symbol_rate: u32 = self.symbol_rate.parse().ok()?;
        let source = LiveSource::Satellite {
            frequency_khz: frequency.checked_mul(1000)?,
            symbol_rate_ksps: symbol_rate,
            polarisation: if self.vertical { SatellitePolarisation::Vertical } else { SatellitePolarisation::Horizontal },
        };
        source.validate().ok().map(|()| source)
    }

    fn set(&mut self, source: &LiveSource) {
        if let LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation } = *source {
            self.frequency = (frequency_khz / 1000).to_string();
            self.symbol_rate = symbol_rate_ksps.to_string();
            self.vertical = polarisation == SatellitePolarisation::Vertical;
        }
    }
}

/// A row of the setup's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupRow {
    Automatic,
    Satellite,
    SatelliteName,
    ScanMode,
    /// The satellite scan that keeps the list and adds what is new.
    ScanNew,
    /// Every channel of the list deleted (Vestel's "Kanal listesini sil").
    ClearList,
    /// The LNB's DiSEqC 1.0 port.
    Diseqc,
    Manual,
    TkgsSettings,
    UpdateNow,
    TkgsMode,
    Ordering,
    Categories,
    Frequency,
    SymbolRate,
    Polarisation,
    Scrambled,
    Keep,
    Scan,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    pub stage: SetupStage,
    pub focus: usize,
    pub manual: ManualForm,
    /// The scan on, or last, is the whole satellite's.
    pub satellite: bool,
    /// The search on was started elsewhere (mediaboxctl, another screen):
    /// shown as it goes, its result not this screen's to have.
    pub adopted: bool,
    /// The satellite scan on, or last, adds to the list (else rebuilds it).
    pub add_only: bool,
    /// "Kanal listesini sil" asked once: the next Ok deletes.
    pub confirm_clear: bool,
    /// What the deletion did: the channels gone, or why none.
    pub cleared: Option<Result<u64, String>>,
    /// The manual form's carrier as the receiver reads it: locked, level
    /// (%), SNR (dB).
    pub signal: Option<(bool, Option<u64>, Option<f64>)>,
    /// The last satellite scan's services by type, for its result.
    pub kinds: [u64; 3],
}

impl Setup {
    /// The buttons of a search's card, in order; none for the lists.
    pub fn actions(&self) -> &'static [&'static str] {
        match self.stage {
            SetupStage::Running(_) => &["Durdur"],
            SetupStage::TkgsDone { .. } => &["Bitti", "Yeniden ara"],
            SetupStage::ScanDone { .. } if self.satellite => &["Bitti", "Yeniden tara"],
            SetupStage::ScanDone { .. } => &["Bitti", "Başka taşıyıcı"],
            SetupStage::Failed { .. } => &["Yeniden dene", "Geri"],
            SetupStage::Menu | SetupStage::Settings | SetupStage::Satellite | SetupStage::Manual => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Channels,
    Favorites,
    EditFavorites,
    Audio,
    Subtitles,
    Settings,
    /// The groups of the channel list, to choose one.
    Groups,
}

impl Panel {
    pub fn title(self) -> &'static str {
        match self {
            Panel::Channels => "Kanal listesi",
            Panel::Favorites => "Favoriler",
            Panel::EditFavorites => "Favorileri düzenle",
            Panel::Audio => "Ses",
            Panel::Subtitles => "Altyazı",
            Panel::Settings => "TV ayarları",
            Panel::Groups => "Kanal grubu",
        }
    }
}

/// A card of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Guide,
    ChannelList,
    Favorites,
    /// A channel watched recently, by service id.
    Recent(u16),
    Audio,
    Subtitles,
    Settings,
}

/// What the interface has to do about a key or a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Nothing changed.
    Nothing,
    /// Redraw.
    Paint,
    /// Tune to the channel with this service id (the banner is already up).
    Tune(u16),
    /// Leave live television.
    Exit,
    Audio(i64),
    /// -1 is off.
    Subtitle(i64),
    /// The favourites changed: keep them.
    Favorites,
    /// Start the channel setup.
    TkgsUpdate,
    /// Stop the channel setup.
    TkgsCancel,
    TkgsOrdering(bool),
    /// TKGS off (true) or on.
    TkgsMode(bool),
    /// TKGS's categories shown as the list's groups.
    TkgsCategories(bool),
    /// The manual scan of a transponder.
    Scan { source: Option<LiveSource>, add_only: bool, free_only: bool, scrambled_only: bool, keep: LiveScanKeep },
    ChannelsClear,
    Diseqc(Option<u8>),
}

#[derive(Debug, Clone, Default)]
pub struct TkgsStatus {
    /// A search (the TKGS read or a scan) is under way.
    pub updating: bool,
    pub channels: usize,
    pub transponders: usize,
    pub updated: Option<i64>,
    pub hd_first: bool,
    pub error: Option<String>,
    /// "TKGS Kapalı".
    pub off: bool,
    pub categories_hidden: bool,
    pub diseqc_port: Option<u8>,
}

/// A track the player has, for the sound and subtitle panels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRow {
    pub id: i64,
    pub label: String,
    pub detail: String,
    pub selected: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Keypad {
    pub typed: String,
    /// Indices into the channels.
    pub candidates: Vec<usize>,
    pub selected: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MenuFocus {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct GuideFocus {
    /// The focused channel.
    pub row: usize,
    /// The first channel drawn.
    pub top: usize,
    /// Where the view starts: on a half hour.
    pub start: i64,
    /// The time the focus is at; Up and Down keep it.
    pub at: i64,
    /// Where the view may not go before.
    pub earliest: i64,
}

/// One cell of a guide row: a programme, or a stretch nothing was said of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub start: i64,
    pub end: i64,
    /// None for a gap.
    pub event: Option<LiveEvent>,
}

#[derive(Debug, Clone, Default)]
pub struct PanelFocus {
    pub focus: usize,
    pub top: usize,
}

pub struct Live {
    pub channels: Vec<LiveChannel>,
    /// Programme guide by service id.
    pub guide: BTreeMap<u16, Vec<LiveEvent>>,
    /// Whether the control plane has answered with the channels at all.
    pub loaded: bool,
    /// The channel on now (service id), and the one before it.
    pub current: Option<u16>,
    pub previous: Option<u16>,
    /// Most recent first, the current one included.
    pub recent: Vec<u16>,
    pub favorites: BTreeSet<u16>,
    pub layer: Layer,
    pub banner_until: Option<Instant>,
    /// Up or Down moves the banner only; the channel it is on is tuned once
    /// the presses stop, at this instant (AOSP Live TV's MainActivity tunes
    /// on the key's release, `finishChannelChangeIfNeeded`).
    pub zap: Option<(u16, Instant)>,
    /// The banner's large area (the description), as the Info key opens it.
    pub banner_detail: bool,
    pub layer_until: Option<Instant>,
    pub keypad: Keypad,
    pub menu: MenuFocus,
    pub guide_focus: GuideFocus,
    pub panel: PanelFocus,
    pub audio: Vec<TrackRow>,
    pub subtitles: Vec<TrackRow>,
    pub tkgs: TkgsStatus,
    /// The group the channel list shows.
    pub group: Group,
    pub setup: Setup,
    /// What is wrong with the channel on now, in words, if anything is: a
    /// failed tune, no picture. Drawn over the black.
    pub problem: Option<String>,
    /// The receiver lost the carrier and is locking again (its state
    /// `no_signal`): the picture is taken off and `no_signal` says so.
    pub no_signal: bool,
}

/// The half hour at or before `t`.
pub fn floor_half_hour(t: i64) -> i64 {
    t.div_euclid(HALF_HOUR) * HALF_HOUR
}

impl Live {
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
            guide: BTreeMap::new(),
            loaded: false,
            current: None,
            previous: None,
            recent: Vec::new(),
            favorites: BTreeSet::new(),
            layer: Layer::None,
            banner_until: None,
            banner_detail: false,
            layer_until: None,
            keypad: Keypad::default(),
            menu: MenuFocus::default(),
            guide_focus: GuideFocus { row: 0, top: 0, start: 0, at: 0, earliest: 0 },
            panel: PanelFocus::default(),
            audio: Vec::new(),
            subtitles: Vec::new(),
            tkgs: TkgsStatus { hd_first: true, ..Default::default() },
            group: Group::Television,
            setup: Setup {
                stage: SetupStage::Menu,
                focus: 0,
                manual: ManualForm {
                    frequency: String::new(),
                    symbol_rate: "27500".into(),
                    vertical: true,
                    access: 0,
                    keep: LiveScanKeep::All,
                    fresh: true,
                },
                satellite: false,
                adopted: false,
                add_only: false,
                confirm_clear: false,
                cleared: None,
                signal: None,
                kinds: [0; 3],
            },
            problem: None,
            no_signal: false,
            zap: None,
        }
    }

    // ------------------------------------------------------------ channels

    pub fn index_of(&self, sid: u16) -> Option<usize> {
        self.channels.iter().position(|c| c.service_id == sid)
    }

    pub fn channel(&self, sid: u16) -> Option<&LiveChannel> {
        self.channels.iter().find(|c| c.service_id == sid)
    }

    pub fn current_channel(&self) -> Option<&LiveChannel> {
        self.current.and_then(|sid| self.channel(sid))
    }

    /// The channel `step` places from the current one, wrapping as the
    /// reference's `moveToAdjacentBrowsableChannel` does: among television
    /// channels from a television channel, among radio from a radio one.
    pub fn adjacent(&self, step: isize) -> Option<u16> {
        let radio = self.current_channel().is_some_and(|c| c.radio);
        let same: Vec<&LiveChannel> = self.channels.iter().filter(|c| c.radio == radio).collect();
        let count = same.len() as isize;
        if count == 0 {
            return None;
        }
        let at = self.current.and_then(|sid| same.iter().position(|c| c.service_id == sid)).map_or(-1, |i| i as isize);
        let next = if at < 0 { 0 } else { (at + step).rem_euclid(count) };
        Some(same[next as usize].service_id)
    }

    /// The channel to start on: the one left on, else the first television
    /// channel, else the first.
    pub fn starting_channel(&self, last: Option<u16>) -> Option<u16> {
        last.filter(|sid| self.index_of(*sid).is_some())
            .or_else(|| self.channels.iter().find(|c| !c.radio).or(self.channels.first()).map(|c| c.service_id))
    }

    /// The channel list's groups: television, radio, then TKGS's categories
    /// in the order the list first has them. A group with nothing in it is
    /// not offered.
    pub fn groups(&self) -> Vec<Group> {
        let mut groups = vec![Group::Television, Group::Radio];
        for channel in &self.channels {
            for name in &channel.categories {
                let group = Group::Category(name.clone());
                if !groups.contains(&group) {
                    groups.push(group);
                }
            }
        }
        groups.retain(|group| self.channels.iter().any(|c| group.holds(c)));
        groups
    }

    /// The channels of the group the list shows.
    pub fn grouped(&self) -> Vec<&LiveChannel> {
        self.channels.iter().filter(|c| self.group.holds(c)).collect()
    }

    /// Take the channel list and guide the control plane answered.
    pub fn set_channels(&mut self, channels: Vec<LiveChannel>) {
        self.channels = channels;
        self.loaded = true;
        let known: BTreeSet<u16> = self.channels.iter().map(|c| c.service_id).collect();
        self.recent.retain(|sid| known.contains(sid));
        if !self.groups().contains(&self.group) {
            self.group = Group::Television;
        }
    }

    /// Open the channel setup at its menu; a search under way is shown as
    /// it is.
    pub fn open_setup(&mut self) {
        if !self.tkgs.updating {
            self.setup.stage = SetupStage::Menu;
            self.setup.focus = 0;
        }
        self.layer = Layer::Setup;
        self.layer_until = None;
        self.banner_until = None;
    }

    /// How far the control plane says the search has got.
    pub fn setup_progress(&mut self, run: Run, running: bool) {
        if running && run.carriers > 0 {
            self.setup.kinds = run.kinds;
        }
        match &mut self.setup.stage {
            SetupStage::Running(now) if running => *now = Run { scan: now.scan, ..run },
            SetupStage::Running(_) if self.setup.adopted => {
                self.setup.adopted = false;
                self.tkgs.updating = false;
                let on = if self.setup.satellite { SetupRow::Satellite } else { SetupRow::Automatic };
                self.setup_menu(on);
            }
            // A search the control plane runs that this screen did not
            // start: the setup shows it rather than a menu that cannot start
            // another.
            SetupStage::Menu if running && self.layer == Layer::Setup && !self.tkgs.updating => {
                self.setup.adopted = true;
                self.setup.satellite = run.carriers > 0;
                self.tkgs.updating = true;
                self.setup.stage = SetupStage::Running(run);
                self.setup.focus = 0;
            }
            _ => {}
        }
    }

    /// The TKGS read's answer: the television and radio channels it gave,
    /// and the table's text; or why there are none.
    pub fn tkgs_finished(&mut self, result: Result<(usize, usize, String), String>) {
        self.setup.stage = match result {
            Ok((television, radio, title)) => SetupStage::TkgsDone { television, radio, title },
            Err(error) => SetupStage::Failed { scan: false, error },
        };
        self.setup.focus = 0;
    }

    /// A manual scan's answer: the services it kept, how many are new to
    /// the list and the first of those; or why there are none.
    pub fn scan_finished(&mut self, result: Result<(usize, usize, Option<u16>), String>) {
        self.setup.stage = match result {
            Ok((found, added, first)) => SetupStage::ScanDone { found, added, first },
            Err(error) => SetupStage::Failed { scan: true, error },
        };
        self.setup.focus = 0;
    }

    /// The rows of the setup's list on now. A row is there only when what
    /// it does works: with TKGS off, nothing of TKGS's but turning it on.
    pub fn setup_rows(&self) -> Vec<SetupRow> {
        match self.setup.stage {
            SetupStage::Menu => {
                let mut rows = Vec::new();
                if !self.tkgs.off {
                    rows.push(SetupRow::Automatic);
                }
                rows.extend([SetupRow::Satellite, SetupRow::Manual, SetupRow::TkgsSettings, SetupRow::ClearList]);
                rows
            }
            SetupStage::Settings if self.tkgs.off => vec![SetupRow::TkgsMode],
            SetupStage::Settings => vec![SetupRow::UpdateNow, SetupRow::TkgsMode, SetupRow::Ordering, SetupRow::Categories],
            SetupStage::Satellite => vec![
                SetupRow::Scan,
                SetupRow::ScanNew,
                SetupRow::SatelliteName,
                SetupRow::Diseqc,
                SetupRow::ScanMode,
                SetupRow::Scrambled,
                SetupRow::Keep,
            ],
            SetupStage::Manual => vec![
                SetupRow::Frequency,
                SetupRow::SymbolRate,
                SetupRow::Polarisation,
                SetupRow::Diseqc,
                SetupRow::Scrambled,
                SetupRow::Keep,
                SetupRow::Scan,
            ],
            _ => Vec::new(),
        }
    }

    /// A row's label and value.
    pub fn setup_row_text(&self, row: SetupRow) -> (&'static str, String) {
        let form = &self.setup.manual;
        let number = |typed: &str, unit: &str| if typed.is_empty() { "—".to_string() } else { format!("{typed} {unit}") };
        match row {
            SetupRow::Automatic => ("Otomatik arama", "TKGS".into()),
            SetupRow::Satellite => ("Uydu taraması", "Türksat 42°E".into()),
            SetupRow::Manual => ("Manuel tarama", String::new()),
            SetupRow::TkgsSettings => ("TKGS ayarları", String::new()),
            SetupRow::UpdateNow => ("Şimdi güncelle", String::new()),
            SetupRow::TkgsMode => ("TKGS modu", if self.tkgs.off { "Kapalı" } else { "Otomatik" }.into()),
            SetupRow::Ordering => ("Liste", if self.tkgs.hd_first { "HD" } else { "SD" }.into()),
            SetupRow::Categories => ("Türler", if self.tkgs.categories_hidden { "Kapalı" } else { "Açık" }.into()),
            SetupRow::Frequency => ("Frekans", number(&form.frequency, "MHz")),
            SetupRow::SymbolRate => ("Sembol oranı", number(&form.symbol_rate, "kS/s")),
            SetupRow::Polarisation => ("Polarizasyon", if form.vertical { "Dikey" } else { "Yatay" }.into()),
            SetupRow::SatelliteName => ("Uydu", "Türksat 42E".into()),
            SetupRow::ScanMode => ("Tarama modu", "Ağ".into()),
            SetupRow::Scrambled => ("Tarama türü", ["Tümü", "Yalnız ücretsiz", "Yalnız şifreli"][form.access as usize % 3].into()),
            SetupRow::Keep => (
                "Hafızaya alma türü",
                match form.keep {
                    LiveScanKeep::All => "Tümü",
                    LiveScanKeep::Television => "Yalnız TV",
                    LiveScanKeep::Radio => "Yalnız radyo",
                }
                .into(),
            ),
            SetupRow::Scan if self.setup.stage == SetupStage::Satellite => ("Yeniden tara", String::new()),
            SetupRow::Scan => ("Tara", String::new()),
            SetupRow::ScanNew => ("Yeni kanalları ekle", String::new()),
            SetupRow::ClearList => (
                "Kanal listesini sil",
                match (&self.setup.cleared, self.setup.confirm_clear) {
                    (Some(Ok(removed)), _) => format!("{removed} kanal silindi"),
                    (Some(Err(error)), _) => error.clone(),
                    (None, true) => "Onay: OK".into(),
                    (None, false) => String::new(),
                },
            ),
            SetupRow::Diseqc => (
                "DiSEqC",
                match self.tkgs.diseqc_port {
                    None => "Kapalı".into(),
                    Some(port) => format!("1.0 · {}", ["A", "B", "C", "D"][port as usize % 4]),
                },
            ),
        }
    }

    /// "Tara" only with a frequency and rate the receiver takes.
    pub fn setup_row_enabled(&self, row: SetupRow) -> bool {
        match row {
            SetupRow::SatelliteName | SetupRow::ScanMode => false,
            SetupRow::Scan => self.setup.stage == SetupStage::Satellite || self.setup.manual.source().is_some(),
            _ => true,
        }
    }

    /// The satellite transponders of the channel list, by frequency.
    pub fn carriers(&self) -> Vec<LiveSource> {
        let mut out: Vec<LiveSource> = Vec::new();
        for channel in &self.channels {
            if matches!(channel.source, LiveSource::Satellite { .. }) && !out.contains(&channel.source) {
                out.push(channel.source.clone());
            }
        }
        out.sort_by_key(|source| match *source {
            LiveSource::Satellite { frequency_khz, polarisation, .. } => (frequency_khz, polarisation == SatellitePolarisation::Vertical),
            LiveSource::Recording => (0, false),
        });
        out
    }

    fn open_manual(&mut self) {
        // It starts on the channel on now's transponder, else the first the
        // list has; what was typed last time stays.
        if self.setup.manual.frequency.is_empty() {
            let from = self
                .current_channel()
                .map(|channel| channel.source.clone())
                .filter(|source| matches!(source, LiveSource::Satellite { .. }))
                .or_else(|| self.carriers().into_iter().next());
            if let Some(source) = from {
                self.setup.manual.set(&source);
            }
        }
        self.setup.manual.fresh = true;
        self.setup.stage = SetupStage::Manual;
        self.setup.focus = 0;
    }

    fn start_tkgs(&mut self) -> Effect {
        self.setup.adopted = false;
        self.setup.stage = SetupStage::Running(Run::default());
        self.setup.focus = 0;
        self.tkgs.updating = true;
        self.tkgs.error = None;
        Effect::TkgsUpdate
    }

    fn start_scan(&mut self) -> Effect {
        let Some(source) = self.setup.manual.source() else { return Effect::Nothing };
        self.run_scan(Some(source))
    }

    /// Every carrier the channel list knows, scanned in turn.
    fn start_satellite(&mut self, add_only: bool) -> Effect {
        self.setup.add_only = add_only;
        self.run_scan(None)
    }

    fn run_scan(&mut self, source: Option<LiveSource>) -> Effect {
        self.setup.adopted = false;
        self.setup.satellite = source.is_none();
        self.setup.stage = SetupStage::Running(Run { scan: true, ..Run::default() });
        self.setup.focus = 0;
        self.tkgs.updating = true;
        self.tkgs.error = None;
        let form = &self.setup.manual;
        Effect::Scan { source, add_only: self.setup.add_only, free_only: form.access == 1, scrambled_only: form.access == 2, keep: form.keep }
    }

    /// Back onto the menu, on the row that led away from it.
    fn setup_menu(&mut self, on: SetupRow) -> Effect {
        self.setup.stage = SetupStage::Menu;
        self.setup.focus = self.setup_rows().iter().position(|row| *row == on).unwrap_or(0);
        Effect::Paint
    }

    fn setup_key(&mut self, action: InputAction, at: Instant) -> Effect {
        match self.setup.stage {
            // A search: Back, or its one button, stops it.
            SetupStage::Running(_) => match action {
                InputAction::Back | InputAction::Ok => Effect::TkgsCancel,
                _ => Effect::Nothing,
            },
            SetupStage::Menu | SetupStage::Settings | SetupStage::Satellite | SetupStage::Manual => self.setup_list_key(action, at),
            _ => self.setup_card_key(action, at),
        }
    }

    fn setup_list_key(&mut self, action: InputAction, at: Instant) -> Effect {
        let rows = self.setup_rows();
        if self.setup.focus >= rows.len() {
            self.setup.focus = 0;
        }
        let row = rows[self.setup.focus];
        if let Some(digit) = action.digit() {
            return self.setup_digit(row, digit);
        }
        match action {
            InputAction::Up | InputAction::Down => {
                let focus = if action == InputAction::Up {
                    self.setup.focus.checked_sub(1)
                } else {
                    Some(self.setup.focus + 1).filter(|&f| f < rows.len())
                };
                let Some(focus) = focus else { return Effect::Nothing };
                self.setup.focus = focus;
                self.setup.manual.fresh = true;
                self.setup.confirm_clear = false;
                self.setup.cleared = None;
                Effect::Paint
            }
            InputAction::Back => match self.setup.stage {
                SetupStage::Settings => self.setup_menu(SetupRow::TkgsSettings),
                SetupStage::Manual => self.setup_menu(SetupRow::Manual),
                SetupStage::Satellite => self.setup_menu(SetupRow::Satellite),
                _ => self.leave_setup(at),
            },
            InputAction::Ok | InputAction::Left | InputAction::Right => self.setup_row_act(row, action),
            _ => Effect::Nothing,
        }
    }

    /// Ok on a row, or Left or Right on a row with a value to change.
    fn setup_row_act(&mut self, row: SetupRow, action: InputAction) -> Effect {
        let ok = action == InputAction::Ok;
        let forward = action != InputAction::Left;
        let form = &mut self.setup.manual;
        match row {
            SetupRow::Automatic | SetupRow::UpdateNow if ok => self.start_tkgs(),
            SetupRow::Satellite if ok => {
                self.setup.stage = SetupStage::Satellite;
                self.setup.focus = 0;
                Effect::Paint
            }
            SetupRow::Manual if ok => {
                self.open_manual();
                Effect::Paint
            }
            SetupRow::TkgsSettings if ok => {
                self.setup.stage = SetupStage::Settings;
                self.setup.focus = 0;
                Effect::Paint
            }
            SetupRow::TkgsMode => {
                self.tkgs.off = !self.tkgs.off;
                self.setup.focus = self.setup_rows().iter().position(|r| *r == SetupRow::TkgsMode).unwrap_or(0);
                Effect::TkgsMode(self.tkgs.off)
            }
            SetupRow::Ordering => {
                self.tkgs.hd_first = !self.tkgs.hd_first;
                Effect::TkgsOrdering(self.tkgs.hd_first)
            }
            SetupRow::Categories => {
                self.tkgs.categories_hidden = !self.tkgs.categories_hidden;
                Effect::TkgsCategories(!self.tkgs.categories_hidden)
            }
            // The transponders the list knows, one after another.
            SetupRow::Frequency if !ok => {
                let carriers = self.carriers();
                if carriers.is_empty() {
                    return Effect::Nothing;
                }
                let at = self.setup.manual.source().and_then(|source| carriers.iter().position(|c| *c == source));
                let next = match (at, forward) {
                    (Some(i), true) => (i + 1) % carriers.len(),
                    (Some(i), false) => (i + carriers.len() - 1) % carriers.len(),
                    (None, _) => 0,
                };
                self.setup.manual.set(&carriers[next]);
                self.setup.manual.fresh = true;
                Effect::Paint
            }
            SetupRow::Polarisation => {
                form.vertical = !form.vertical;
                Effect::Paint
            }
            SetupRow::Scrambled => {
                form.access = if forward { (form.access + 1) % 3 } else { (form.access + 2) % 3 };
                Effect::Paint
            }
            SetupRow::Keep => {
                let order = [LiveScanKeep::All, LiveScanKeep::Television, LiveScanKeep::Radio];
                let i = order.iter().position(|k| *k == form.keep).unwrap_or(0);
                form.keep = order[if forward { (i + 1) % 3 } else { (i + 2) % 3 }];
                Effect::Paint
            }
            SetupRow::Scan if ok && self.setup.stage == SetupStage::Satellite => self.start_satellite(false),
            SetupRow::ScanNew if ok => self.start_satellite(true),
            SetupRow::ClearList if ok && self.setup.confirm_clear => {
                self.setup.confirm_clear = false;
                Effect::ChannelsClear
            }
            SetupRow::ClearList if ok => {
                self.setup.confirm_clear = true;
                self.setup.cleared = None;
                Effect::Paint
            }
            SetupRow::Diseqc => {
                // Kapalı, A, B, C, D, round.
                let order = [None, Some(0), Some(1), Some(2), Some(3)];
                let at = order.iter().position(|p| *p == self.tkgs.diseqc_port).unwrap_or(0);
                let port = order[if forward { (at + 1) % 5 } else { (at + 4) % 5 }];
                self.tkgs.diseqc_port = port;
                Effect::Diseqc(port)
            }
            SetupRow::Scan if ok => self.start_scan(),
            _ => Effect::Nothing,
        }
    }

    /// A digit on the frequency or the rate: the first after the focus came
    /// there starts the number over; five digits at most.
    fn setup_digit(&mut self, row: SetupRow, digit: u8) -> Effect {
        let form = &mut self.setup.manual;
        let typed = match row {
            SetupRow::Frequency => &mut form.frequency,
            SetupRow::SymbolRate => &mut form.symbol_rate,
            _ => return Effect::Nothing,
        };
        if form.fresh {
            typed.clear();
            form.fresh = false;
        }
        if typed.len() >= 5 {
            return Effect::Nothing;
        }
        typed.push(char::from(b'0' + digit));
        Effect::Paint
    }

    /// The card a search ends on: its buttons.
    fn setup_card_key(&mut self, action: InputAction, at: Instant) -> Effect {
        let count = self.setup.actions().len();
        match action {
            InputAction::Up | InputAction::Left => {
                let Some(focus) = self.setup.focus.checked_sub(1) else { return Effect::Nothing };
                self.setup.focus = focus;
                Effect::Paint
            }
            InputAction::Down | InputAction::Right => {
                if self.setup.focus + 1 >= count {
                    return Effect::Nothing;
                }
                self.setup.focus += 1;
                Effect::Paint
            }
            InputAction::Back => match self.setup.stage {
                SetupStage::ScanDone { .. } | SetupStage::Failed { scan: true, .. } if self.setup.satellite => self.setup_menu(SetupRow::Satellite),
                SetupStage::ScanDone { .. } | SetupStage::Failed { scan: true, .. } => {
                    self.open_manual();
                    Effect::Paint
                }
                _ => self.setup_menu(SetupRow::Automatic),
            },
            InputAction::Ok => match (self.setup.stage.clone(), self.setup.focus) {
                (SetupStage::TkgsDone { .. }, 0) => self.leave_setup(at),
                (SetupStage::TkgsDone { .. }, _) => self.start_tkgs(),
                (SetupStage::ScanDone { first: Some(sid), .. }, 0) if self.index_of(sid).is_some() => {
                    self.tuning(sid, at);
                    Effect::Tune(sid)
                }
                (SetupStage::ScanDone { .. }, 0) => self.leave_setup(at),
                (SetupStage::ScanDone { .. }, _) if self.setup.satellite => self.start_satellite(self.setup.add_only),
                (SetupStage::ScanDone { .. }, _) => {
                    self.open_manual();
                    Effect::Paint
                }
                (SetupStage::Failed { scan: true, .. }, 0) if self.setup.satellite => self.start_satellite(self.setup.add_only),
                (SetupStage::Failed { scan: true, .. }, 0) => self.start_scan(),
                (SetupStage::Failed { scan: false, .. }, 0) => self.start_tkgs(),
                (SetupStage::Failed { scan: true, .. }, _) => {
                    self.open_manual();
                    Effect::Paint
                }
                (SetupStage::Failed { .. }, _) => self.setup_menu(SetupRow::Automatic),
                _ => Effect::Nothing,
            },
            _ => Effect::Nothing,
        }
    }

    /// Out of the setup: onto the channel playing, or the first one if
    /// none plays yet; with no channels at all, out of live television.
    fn leave_setup(&mut self, at: Instant) -> Effect {
        self.close_all();
        if self.current.is_some() {
            return Effect::Paint;
        }
        match self.starting_channel(None) {
            Some(sid) => {
                self.tuning(sid, at);
                Effect::Tune(sid)
            }
            None => Effect::Exit,
        }
    }

    /// Tuning to `sid` is about to be asked for: the banner says so at once,
    /// the overlays that chose it go.
    pub fn tuning(&mut self, sid: u16, at: Instant) {
        if self.current != Some(sid) {
            self.previous = self.current;
        }
        self.current = Some(sid);
        self.recent.retain(|&other| other != sid);
        self.recent.insert(0, sid);
        self.recent.truncate(RECENT + 1);
        self.problem = self.channel(sid).filter(|channel| channel.scrambled).map(|_| SCRAMBLED.to_string());
        self.layer = Layer::None;
        self.layer_until = None;
        self.banner_until = Some(at + BANNER_SHOW);
        self.banner_detail = false;
    }

    /// The tuner has no signal: Sony cancels the banner's timeout
    /// ("Canceling banner timeout", 2026-10-25) and the banner stays until
    /// the signal is back; `picture_arrived` then gives it its full time.
    pub fn signal_lost(&mut self) {
        self.no_signal = true;
        if self.current.is_some() {
            self.banner_until = Some(Instant::now() + Duration::from_secs(86_400));
        }
    }

    pub fn signal_back(&mut self, at: Instant) {
        self.no_signal = false;
        if self.banner_until.is_some() {
            self.banner_until = Some(at + BANNER_SHOW);
        }
    }

    /// The channel's picture is up. Sony holds the banner for the whole of
    /// the tuning (its timeout is cancelled until the picture comes) and lets
    /// it go a while after: the banner shown at the key stays for its full
    /// time from here.
    pub fn picture_arrived(&mut self, at: Instant) {
        if self.banner_until.is_some() {
            self.banner_until = Some(at + BANNER_SHOW);
        }
    }

    // ------------------------------------------------------------ the guide

    /// The programme on `sid` at `now`.
    pub fn event_at(&self, sid: u16, now: i64) -> Option<&LiveEvent> {
        self.guide.get(&sid)?.iter().find(|e| e.is_on_at(now))
    }

    pub fn has_any_guide(&self) -> bool {
        self.channels.iter().any(|c| self.guide.get(&c.service_id).is_some_and(|e| !e.is_empty()))
    }

    /// A row of the guide from `from` to `to`: the programmes, and gaps
    /// where nothing was said, end to end.
    pub fn entries(&self, sid: u16, from: i64, to: i64) -> Vec<Entry> {
        let mut events: Vec<&LiveEvent> = self
            .guide
            .get(&sid)
            .map(|events| events.iter().filter(|e| e.end() > from && e.start < to).collect())
            .unwrap_or_default();
        events.sort_by_key(|e| e.start);
        let mut out = Vec::new();
        let mut cursor = from;
        for event in events {
            if event.start > cursor {
                out.push(Entry { start: cursor, end: event.start, event: None });
            }
            if event.end() <= cursor {
                continue;
            }
            out.push(Entry { start: event.start.max(cursor), end: event.end().min(to), event: Some(event.clone()) });
            cursor = event.end();
        }
        if cursor < to {
            out.push(Entry { start: cursor, end: to, event: None });
        }
        out
    }

    fn row_entries(&self, row: usize) -> Vec<Entry> {
        let Some(channel) = self.channels.get(row) else { return Vec::new() };
        let earliest = self.guide_focus.earliest;
        self.entries(channel.service_id, earliest, earliest + GUIDE_HORIZON)
    }

    /// The entry the focus is on.
    pub fn focused_entry(&self) -> Option<Entry> {
        let at = self.guide_focus.at;
        self.row_entries(self.guide_focus.row).into_iter().find(|e| e.start <= at && at < e.end)
    }

    fn open_guide(&mut self, at: Instant, now: i64) {
        let earliest = floor_half_hour(now - GUIDE_LEAD);
        let row = self.current.and_then(|sid| self.index_of(sid)).unwrap_or(0);
        self.guide_focus = GuideFocus { row, top: 0, start: earliest, at: now, earliest };
        self.park_guide_row();
        self.open_layer(Layer::Guide, at);
        // The banner is not drawn over the guide.
        self.banner_until = None;
    }

    fn park_guide_row(&mut self) {
        let count = self.channels.len();
        let rows = GUIDE_ROWS.min(count);
        let g = &mut self.guide_focus;
        g.top = g.row.saturating_sub(GUIDE_SELECTION_ROW).min(count.saturating_sub(rows));
    }

    fn keep_focus_in_view(&mut self) {
        let g = &mut self.guide_focus;
        while g.at >= g.start + GUIDE_VIEW - GUIDE_RIGHT_MARGIN && g.start + HALF_HOUR < g.earliest + GUIDE_HORIZON {
            g.start += HALF_HOUR;
        }
        while g.at < g.start && g.start > g.earliest {
            g.start -= HALF_HOUR;
        }
    }

    fn guide_key(&mut self, action: InputAction, at: Instant, now: i64) -> Effect {
        match action {
            InputAction::Left | InputAction::Right => {
                let entries = self.row_entries(self.guide_focus.row);
                let Some(index) = entries.iter().position(|e| e.start <= self.guide_focus.at && self.guide_focus.at < e.end) else {
                    return Effect::Nothing;
                };
                let next = if action == InputAction::Left { index.checked_sub(1) } else { Some(index + 1) };
                let Some(entry) = next.and_then(|i| entries.get(i)) else { return Effect::Nothing };
                self.guide_focus.at = entry.start.max(self.guide_focus.earliest);
                if action == InputAction::Left && entry.start < self.guide_focus.start {
                    self.guide_focus.start = floor_half_hour(entry.start).max(self.guide_focus.earliest);
                }
                self.keep_focus_in_view();
                Effect::Paint
            }
            InputAction::Up | InputAction::Down | InputAction::ChannelUp | InputAction::ChannelDown => {
                let count = self.channels.len();
                if count == 0 {
                    return Effect::Nothing;
                }
                let up = matches!(action, InputAction::Up | InputAction::ChannelUp);
                let row = self.guide_focus.row as isize + if up { -1 } else { 1 };
                // Past either end it wraps, as the reference's grid does.
                self.guide_focus.row = row.rem_euclid(count as isize) as usize;
                // A time the focus had left behind in the view is taken back
                // to what the view shows.
                if self.guide_focus.at < self.guide_focus.start {
                    self.guide_focus.at = self.guide_focus.start;
                }
                self.park_guide_row();
                Effect::Paint
            }
            InputAction::Ok => {
                let Some(entry) = self.focused_entry() else { return Effect::Nothing };
                let sid = self.channels[self.guide_focus.row].service_id;
                // What is on now is tuned to; anything else is only read.
                if entry.start <= now && now < entry.end {
                    self.tuning(sid, at);
                    return Effect::Tune(sid);
                }
                Effect::Nothing
            }
            _ => Effect::Nothing,
        }
    }

    // ------------------------------------------------------------ the keypad

    fn open_keypad(&mut self, digit: u8, at: Instant) {
        self.keypad = Keypad::default();
        self.open_layer(Layer::Keypad, at);
        self.type_digit(digit);
    }

    fn type_digit(&mut self, digit: u8) {
        if self.keypad.typed.len() >= MAX_DIGITS {
            self.keypad.typed.clear();
        }
        self.keypad.typed.push(char::from(b'0' + digit));
        let typed = self.keypad.typed.trim_start_matches('0');
        let typed = if typed.is_empty() { "0" } else { typed };
        let mut exact = Vec::new();
        let mut begins = Vec::new();
        for (i, channel) in self.channels.iter().enumerate() {
            let number = channel.number.to_string();
            if number == typed {
                exact.push(i);
            } else if number.starts_with(typed) {
                begins.push(i);
            }
        }
        exact.extend(begins);
        exact.truncate(MAX_CANDIDATES);
        self.keypad.candidates = exact;
        self.keypad.selected = 0;
    }

    fn keypad_choice(&self) -> Option<u16> {
        self.keypad.candidates.get(self.keypad.selected).map(|&i| self.channels[i].service_id)
    }

    // ------------------------------------------------------------ the menu

    /// The menu's rows: a title and its cards.
    pub fn menu_rows(&self) -> Vec<(&'static str, Vec<Card>)> {
        let mut channels = vec![Card::Guide, Card::ChannelList, Card::Favorites];
        channels.extend(
            self.recent
                .iter()
                .filter(|&&sid| Some(sid) != self.current && self.channel(sid).is_some())
                .take(RECENT)
                .map(|&sid| Card::Recent(sid)),
        );
        vec![("Kanallar", channels), ("TV seçenekleri", vec![Card::Audio, Card::Subtitles, Card::Settings])]
    }

    fn menu_key(&mut self, action: InputAction, at: Instant, now: i64) -> Effect {
        let rows = self.menu_rows();
        match action {
            InputAction::Up | InputAction::Down => {
                let row = if action == InputAction::Up {
                    self.menu.row.checked_sub(1)
                } else {
                    Some(self.menu.row + 1).filter(|&r| r < rows.len())
                };
                let Some(row) = row else { return Effect::Nothing };
                self.menu = MenuFocus { row, col: 0 };
                Effect::Paint
            }
            InputAction::Left | InputAction::Right => {
                let cards = rows[self.menu.row].1.len();
                let col = if action == InputAction::Left {
                    self.menu.col.checked_sub(1)
                } else {
                    Some(self.menu.col + 1).filter(|&c| c < cards)
                };
                let Some(col) = col else { return Effect::Nothing };
                self.menu.col = col;
                Effect::Paint
            }
            InputAction::Ok => {
                let Some(&card) = rows.get(self.menu.row).and_then(|(_, cards)| cards.get(self.menu.col)) else {
                    return Effect::Nothing;
                };
                match card {
                    Card::Guide => self.open_guide(at, now),
                    Card::ChannelList => self.open_panel(Panel::Channels, at),
                    Card::Favorites => self.open_panel(Panel::Favorites, at),
                    Card::Recent(sid) => {
                        self.tuning(sid, at);
                        return Effect::Tune(sid);
                    }
                    Card::Audio => self.open_panel(Panel::Audio, at),
                    Card::Subtitles => self.open_panel(Panel::Subtitles, at),
                    Card::Settings => self.open_panel(Panel::Settings, at),
                }
                Effect::Paint
            }
            _ => Effect::Nothing,
        }
    }

    // ------------------------------------------------------------ side panels

    /// A side panel's rows: label, value, checked, enabled.
    pub fn panel_rows(&self, panel: Panel, now: i64) -> Vec<PanelRow> {
        let channel_row = |c: &LiveChannel, value: String| PanelRow {
            label: format!("{}  {}", c.number, c.name),
            value,
            checked: Some(c.service_id) == self.current,
            enabled: true,
        };
        let on_now = |sid: u16| {
            let event = self.event_at(sid, now).map(|e| e.title.clone()).unwrap_or_default();
            if self.channel(sid).is_some_and(|channel| channel.scrambled) {
                if event.is_empty() { "Şifreli".into() } else { format!("Şifreli · {event}") }
            } else { event }
        };
        match panel {
            Panel::Channels => {
                let grouped = self.grouped();
                let mut rows = vec![PanelRow {
                    label: "Grup".into(),
                    value: format!("{} · {}", self.group.title(), grouped.len()),
                    checked: false,
                    enabled: true,
                }];
                rows.extend(grouped.into_iter().map(|c| {
                    let mut row = channel_row(c, on_now(c.service_id));
                    if self.favorites.contains(&c.service_id) {
                        row.label = format!("{} ★", row.label);
                    }
                    row
                }));
                rows
            }
            Panel::Groups => self
                .groups()
                .into_iter()
                .map(|group| PanelRow {
                    value: self.channels.iter().filter(|c| group.holds(c)).count().to_string(),
                    checked: group == self.group,
                    label: group.title().to_string(),
                    enabled: true,
                })
                .collect(),
            Panel::Favorites => {
                let mut rows: Vec<PanelRow> = self
                    .channels
                    .iter()
                    .filter(|c| self.favorites.contains(&c.service_id))
                    .map(|c| channel_row(c, on_now(c.service_id)))
                    .collect();
                rows.push(PanelRow {
                    label: Panel::EditFavorites.title().into(),
                    value: String::new(),
                    checked: false,
                    enabled: !self.channels.is_empty(),
                });
                rows
            }
            Panel::EditFavorites => self
                .channels
                .iter()
                .map(|c| PanelRow {
                    label: format!("{}  {}", c.number, c.name),
                    value: String::new(),
                    checked: self.favorites.contains(&c.service_id),
                    enabled: true,
                })
                .collect(),
            Panel::Audio => self.tracks_rows(&self.audio, false),
            Panel::Subtitles => self.tracks_rows(&self.subtitles, true),
            Panel::Settings => {
                let describe = |source: &LiveSource| match source {
                    LiveSource::Satellite { frequency_khz, symbol_rate_ksps, .. } => {
                        format!("Uydu · {} MHz · {} kS/s", frequency_khz / 1000, symbol_rate_ksps)
                    }
                    LiveSource::Recording => "Kayıtlı yayın".to_string(),
                };
                let mut sources: Vec<&LiveSource> = Vec::new();
                for channel in &self.channels {
                    if !sources.contains(&&channel.source) {
                        sources.push(&channel.source);
                    }
                }
                let source = match sources.as_slice() {
                    [] => "Yok".to_string(),
                    [one] => describe(one),
                    many => format!("{} yayın", many.len()),
                };
                let tkgs = if self.tkgs.updating {
                    "Taranıyor…".to_string()
                } else if let Some(error) = &self.tkgs.error {
                    format!("Hata · {error}")
                } else if let Some(updated) = self.tkgs.updated {
                    format!("{} kanal · {} transponder · {}", self.tkgs.channels, self.tkgs.transponders, clock(updated))
                } else {
                    "Güncellenmedi".to_string()
                };
                vec![
                    PanelRow { label: "Kaynak".into(), value: source, checked: false, enabled: false },
                    PanelRow {
                        label: "Kanal sayısı".into(),
                        value: self.channels.len().to_string(),
                        checked: false,
                        enabled: false,
                    },
                    PanelRow { label: "Kanal kurulumu".into(), value: tkgs, checked: false, enabled: true },
                ]
            }
        }
    }

    fn tracks_rows(&self, tracks: &[TrackRow], with_off: bool) -> Vec<PanelRow> {
        let mut rows = Vec::new();
        if with_off {
            rows.push(PanelRow {
                label: "Kapalı".into(),
                value: String::new(),
                checked: !tracks.iter().any(|t| t.selected),
                enabled: true,
            });
        }
        rows.extend(tracks.iter().map(|t| PanelRow {
            label: t.label.clone(),
            value: t.detail.clone(),
            checked: t.selected,
            enabled: true,
        }));
        if rows.len() == usize::from(with_off) {
            rows.push(PanelRow { label: "Bu kanalda yok".into(), value: String::new(), checked: false, enabled: false });
        }
        rows
    }

    fn open_panel(&mut self, panel: Panel, at: Instant) {
        self.panel = PanelFocus::default();
        // The channel lists open on the channel on now; the list of a
        // group, below its group row.
        match panel {
            Panel::Channels => {
                if let Some(i) = self.current.and_then(|sid| self.grouped().iter().position(|c| c.service_id == sid)) {
                    self.panel.focus = i + 1;
                }
            }
            Panel::EditFavorites => {
                if let Some(i) = self.current.and_then(|sid| self.index_of(sid)) {
                    self.panel.focus = i;
                }
            }
            Panel::Groups => {
                if let Some(i) = self.groups().iter().position(|g| *g == self.group) {
                    self.panel.focus = i;
                }
            }
            _ => {}
        }
        self.open_layer(Layer::Panel(panel), at);
        self.banner_until = None;
        self.scroll_panel(panel);
    }

    fn scroll_panel(&mut self, panel: Panel) {
        let count = self.panel_rows(panel, 0).len();
        let p = &mut self.panel;
        if p.focus < p.top {
            p.top = p.focus;
        } else if p.focus >= p.top + PANEL_ROWS {
            p.top = p.focus + 1 - PANEL_ROWS;
        }
        p.top = p.top.min(count.saturating_sub(PANEL_ROWS));
    }

    fn panel_key(&mut self, panel: Panel, action: InputAction, at: Instant, now: i64) -> Effect {
        let rows = self.panel_rows(panel, now);
        match action {
            InputAction::Up | InputAction::Down => {
                let focus = if action == InputAction::Up {
                    self.panel.focus.checked_sub(1)
                } else {
                    Some(self.panel.focus + 1).filter(|&f| f < rows.len())
                };
                let Some(focus) = focus else { return Effect::Nothing };
                self.panel.focus = focus;
                self.scroll_panel(panel);
                Effect::Paint
            }
            InputAction::Ok => {
                let Some(row) = rows.get(self.panel.focus) else { return Effect::Nothing };
                if !row.enabled {
                    return Effect::Nothing;
                }
                match panel {
                    Panel::Channels => match self.panel.focus.checked_sub(1) {
                        None => {
                            self.open_panel(Panel::Groups, at);
                            Effect::Paint
                        }
                        Some(i) => {
                            let sid = self.grouped()[i].service_id;
                            self.tuning(sid, at);
                            Effect::Tune(sid)
                        }
                    },
                    Panel::Groups => {
                        if let Some(group) = self.groups().into_iter().nth(self.panel.focus) {
                            self.group = group;
                        }
                        self.open_panel(Panel::Channels, at);
                        Effect::Paint
                    }
                    Panel::Favorites => {
                        let favorites: Vec<u16> = self
                            .channels
                            .iter()
                            .filter(|c| self.favorites.contains(&c.service_id))
                            .map(|c| c.service_id)
                            .collect();
                        match favorites.get(self.panel.focus) {
                            Some(&sid) => {
                                self.tuning(sid, at);
                                Effect::Tune(sid)
                            }
                            None => {
                                self.open_panel(Panel::EditFavorites, at);
                                Effect::Paint
                            }
                        }
                    }
                    Panel::EditFavorites => {
                        let sid = self.channels[self.panel.focus].service_id;
                        if !self.favorites.remove(&sid) {
                            self.favorites.insert(sid);
                        }
                        Effect::Favorites
                    }
                    Panel::Audio => {
                        let id = self.audio[self.panel.focus].id;
                        Effect::Audio(id)
                    }
                    Panel::Subtitles => match self.panel.focus.checked_sub(1) {
                        None => Effect::Subtitle(-1),
                        Some(i) => Effect::Subtitle(self.subtitles[i].id),
                    },
                    Panel::Settings => match self.panel.focus {
                        2 => {
                            self.open_setup();
                            Effect::Paint
                        }
                        _ => Effect::Nothing,
                    },
                }
            }
            _ => Effect::Nothing,
        }
    }

    // ------------------------------------------------------------ keys

    fn open_layer(&mut self, layer: Layer, at: Instant) {
        self.layer = layer;
        self.extend_layer(at);
    }

    fn extend_layer(&mut self, at: Instant) {
        let life = match self.layer {
            Layer::None => {
                self.layer_until = None;
                return;
            }
            Layer::Keypad => KEYPAD_SHOW,
            Layer::Menu => MENU_SHOW,
            Layer::Guide => GUIDE_SHOW,
            Layer::Panel(_) => PANEL_SHOW,
            // The setup stays until it is left.
            Layer::Setup => {
                self.layer_until = None;
                return;
            }
        };
        self.layer_until = Some(at + life);
    }

    fn close_all(&mut self) {
        self.layer = Layer::None;
        self.layer_until = None;
        self.banner_until = None;
    }

    pub fn banner_shown(&self, at: Instant) -> bool {
        self.banner_until.is_some_and(|until| until > at)
    }

    /// Whether anything is over the picture.
    #[cfg(test)]
    pub fn overlaid(&self, at: Instant) -> bool {
        self.layer != Layer::None || self.banner_shown(at)
    }

    /// A key, while live television is on the panel. Home, Power, the
    /// volume and the menu of sheets are not this screen's: the caller keeps
    /// them.
    pub fn key(&mut self, action: InputAction, at: Instant, now: i64) -> Effect {
        // Back closes what is up before it means anything else; with nothing
        // up, it leaves.
        if self.layer == Layer::Setup {
            return self.setup_key(action, at);
        }
        if action == InputAction::Back {
            return match self.layer {
                Layer::Panel(Panel::Groups) => {
                    self.open_panel(Panel::Channels, at);
                    Effect::Paint
                }
                Layer::Panel(Panel::EditFavorites) => {
                    self.open_panel(Panel::Favorites, at);
                    Effect::Paint
                }
                Layer::None if self.banner_shown(at) => {
                    self.close_all();
                    Effect::Paint
                }
                Layer::None => Effect::Exit,
                _ => {
                    self.close_all();
                    Effect::Paint
                }
            };
        }
        let digit = action.digit();
        // Any key keeps the overlay it reaches up for its whole life again.
        if self.layer != Layer::None {
            self.extend_layer(at);
        }
        match self.layer {
            Layer::Keypad => {
                if let Some(digit) = digit {
                    self.type_digit(digit);
                    return Effect::Paint;
                }
                match action {
                    InputAction::Up | InputAction::Down => {
                        let count = self.keypad.candidates.len();
                        if count == 0 {
                            return Effect::Nothing;
                        }
                        let selected = self.keypad.selected as isize + if action == InputAction::Up { -1 } else { 1 };
                        self.keypad.selected = selected.clamp(0, count as isize - 1) as usize;
                        Effect::Paint
                    }
                    InputAction::Ok => match self.keypad_choice() {
                        Some(sid) => {
                            self.tuning(sid, at);
                            Effect::Tune(sid)
                        }
                        None => {
                            self.close_all();
                            Effect::Paint
                        }
                    },
                    _ => Effect::Nothing,
                }
            }
            // A digit over the menu goes to the number entry.
            Layer::Menu if digit.is_some() => {
                self.open_keypad(digit.unwrap_or(0), at);
                Effect::Paint
            }
            Layer::Menu => self.menu_key(action, at, now),
            Layer::Guide => match action {
                InputAction::Guide => {
                    self.close_all();
                    Effect::Paint
                }
                _ => self.guide_key(action, at, now),
            },
            Layer::Panel(panel) => self.panel_key(panel, action, at, now),
            Layer::Setup => self.setup_key(action, at),
            Layer::None => {
                if let Some(digit) = digit {
                    if self.channels.is_empty() {
                        return Effect::Nothing;
                    }
                    self.open_keypad(digit, at);
                    return Effect::Paint;
                }
                match action {
                    InputAction::Up | InputAction::ChannelUp | InputAction::Down | InputAction::ChannelDown => {
                        let up = matches!(action, InputAction::Up | InputAction::ChannelUp);
                        let Some(sid) = self.adjacent(if up { 1 } else { -1 }) else { return Effect::Nothing };
                        self.tuning(sid, at);
                        self.zap = Some((sid, at + ZAP_SETTLE));
                        Effect::Paint
                    }
                    InputAction::PreviousChannel => {
                        let Some(sid) = self.previous.filter(|sid| self.index_of(*sid).is_some()) else {
                            return Effect::Nothing;
                        };
                        self.tuning(sid, at);
                        Effect::Tune(sid)
                    }
                    InputAction::Ok => {
                        if self.current.is_some() {
                            self.banner_until = Some(at + BANNER_SHOW);
                            self.banner_detail = false;
                        }
                        self.menu = MenuFocus::default();
                        self.open_layer(Layer::Menu, at);
                        Effect::Paint
                    }
                    InputAction::Menu => {
                        self.menu = MenuFocus::default();
                        self.open_layer(Layer::Menu, at);
                        Effect::Paint
                    }
                    InputAction::Guide => {
                        if self.channels.is_empty() {
                            return Effect::Nothing;
                        }
                        self.open_guide(at, now);
                        Effect::Paint
                    }
                    InputAction::Info => {
                        if self.current.is_none() {
                            return Effect::Nothing;
                        }
                        // Info again over the large banner takes it away.
                        if self.banner_shown(at) && self.banner_detail {
                            self.banner_until = None;
                            self.banner_detail = false;
                        } else {
                            self.banner_until = Some(at + BANNER_SHOW);
                            self.banner_detail = true;
                        }
                        Effect::Paint
                    }
                    _ => Effect::Nothing,
                }
            }
        }
    }

    /// The overlays' lifetimes. The number entry's end tunes to what it
    /// had selected.
    pub fn tick(&mut self, at: Instant) -> Effect {
        if let Some((sid, due)) = self.zap
            && due <= at
        {
            self.zap = None;
            if self.current == Some(sid) {
                return Effect::Tune(sid);
            }
        }
        let mut effect = Effect::Nothing;
        if self.banner_until.is_some_and(|until| until <= at) {
            self.banner_until = None;
            effect = Effect::Paint;
        }
        if self.layer_until.is_some_and(|until| until <= at) {
            let keypad = self.layer == Layer::Keypad;
            self.layer = Layer::None;
            self.layer_until = None;
            effect = Effect::Paint;
            if keypad && let Some(sid) = self.keypad_choice() {
                self.tuning(sid, at);
                return Effect::Tune(sid);
            }
        }
        effect
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelRow {
    pub label: String,
    pub value: String,
    pub checked: bool,
    pub enabled: bool,
}

/// Whether the receiver's selection of the channel (`status.live.receiver.
/// selection`) says it is encrypted: nine in ten of a few hundred of its
/// packets scrambled. MediaBox has no conditional access, so such a channel
/// can never show a picture, whatever its SDT claims.
pub fn scrambled(selection: &serde_json::Value) -> bool {
    let count = |key: &str| selection.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
    let (all, scrambled) = (count("es_packets"), count("scrambled_packets"));
    all >= 500 && scrambled * 10 >= all * 9
}

/// What is said over such a channel.
/// What the reference application (Kanky on SK1) puts in the middle of the
/// black while its tuner locks again, measured 2026-10-24: "Sinyal Yok,
/// Kilitleniyor [Türksat 4A/5B/6A  11958/V/27500]...". The channel list is
/// Türksat's own (TKGS), so that is the satellite named.
pub fn no_signal(channel: &LiveChannel) -> String {
    match &channel.source {
        LiveSource::Satellite { frequency_khz, symbol_rate_ksps, polarisation } => format!(
            "Sinyal Yok, Kilitleniyor [Türksat 4A/5B/6A  {}/{}/{}]...",
            frequency_khz / 1000,
            match polarisation {
                mediabox_core::SatellitePolarisation::Horizontal => "H",
                mediabox_core::SatellitePolarisation::Vertical => "V",
            },
            symbol_rate_ksps
        ),
        LiveSource::Recording => "Sinyal Yok, Kilitleniyor...".to_string(),
    }
}

pub const SCRAMBLED: &str = "Bu kanal şu an şifreli yayınlanıyor; MediaBox şifreli yayın açamaz.";

pub fn utc_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// "19:00" in the box's own time zone.
pub fn clock(t: i64) -> String {
    let local = t + local_offset(t);
    let minutes = local.div_euclid(60);
    format!("{:02}:{:02}", minutes.div_euclid(60).rem_euclid(24), minutes.rem_euclid(60))
}

/// The local time zone's offset from UTC at `t`, from the system's own
/// `localtime_r`.
pub fn local_offset(t: i64) -> i64 {
    let time = t as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals; localtime_r is re-entrant.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff
}

/// "19:00 – 20:30".
pub fn span(event: &LiveEvent) -> String {
    format!("{} – {}", clock(event.start), clock(event.end()))
}

/// How far into `event` `now` is, 0..1.
pub fn progress(event: &LiveEvent, now: i64) -> f32 {
    if event.duration == 0 {
        return 0.0;
    }
    ((now - event.start) as f32 / event.duration as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(count: u32) -> Vec<LiveChannel> {
        (1..=count)
            .map(|number| LiveChannel {
                number,
                service_id: 10_799 + number as u16,
                name: format!("Kanal {number}"),
                provider: None,
                source: LiveSource::Recording,
                scrambled: false,
                radio: false,
                categories: Vec::new(),
            })
            .collect()
    }

    fn event(start: i64, minutes: u32, title: &str) -> LiveEvent {
        LiveEvent { start, duration: minutes * 60, title: title.into(), text: String::new() }
    }

    fn live(count: u32) -> Live {
        let mut live = Live::new();
        live.set_channels(channels(count));
        live
    }

    const NOW: i64 = 1_800_000_000 + 600; // ten past a half hour

    #[test]
    fn with_nothing_up_up_and_down_change_channel_and_wrap() {
        let at = Instant::now();
        let mut live = live(3);
        live.tuning(10_800, at);
        assert_eq!(live.key(InputAction::Up, at, NOW), Effect::Paint);
        assert_eq!(live.key(InputAction::ChannelUp, at, NOW), Effect::Paint);
        assert_eq!(live.key(InputAction::Up, at, NOW), Effect::Paint);
        assert_eq!(live.key(InputAction::Down, at, NOW), Effect::Paint);
        assert_eq!(live.tick(at), Effect::Nothing, "held: nothing tuned yet");
        assert_eq!(live.tick(at + ZAP_SETTLE), Effect::Tune(10_802), "one tune, to the last one");
        assert_eq!(live.tick(at + ZAP_SETTLE), Effect::Nothing);
        assert!(live.banner_shown(at), "a channel change shows the banner");
        assert_eq!(live.key(InputAction::PreviousChannel, at, NOW), Effect::Tune(10_800));
        assert_eq!(live.key(InputAction::PreviousChannel, at, NOW), Effect::Tune(10_802));
    }

    #[test]
    fn the_banner_goes_by_itself_after_eight_seconds() {
        let at = Instant::now();
        let mut live = live(2);
        live.tuning(10_800, at);
        assert_eq!(live.tick(at + Duration::from_millis(7900)), Effect::Nothing);
        assert!(live.banner_shown(at + Duration::from_millis(7900)));
        assert_eq!(live.tick(at + BANNER_SHOW), Effect::Paint);
        assert!(!live.overlaid(at + BANNER_SHOW));
    }

    #[test]
    fn back_closes_what_is_up_and_only_then_leaves() {
        let at = Instant::now();
        let mut live = live(2);
        live.tuning(10_800, at);
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Paint);
        assert_eq!(live.layer, Layer::Menu);
        assert!(live.banner_shown(at), "Ok shows the banner with the menu");
        assert_eq!(live.key(InputAction::Back, at, NOW), Effect::Paint);
        assert!(!live.overlaid(at), "Back takes the menu and the banner both");
        assert_eq!(live.key(InputAction::Back, at, NOW), Effect::Exit);
    }

    #[test]
    fn digits_select_the_exact_number_first_and_tune_after_two_seconds() {
        let at = Instant::now();
        let mut live = live(12);
        live.tuning(10_800, at);
        assert_eq!(live.key(InputAction::Digit1, at, NOW), Effect::Paint);
        assert_eq!(live.layer, Layer::Keypad);
        let numbers: Vec<u32> = live.keypad.candidates.iter().map(|&i| live.channels[i].number).collect();
        assert_eq!(numbers, [1, 10, 11, 12], "1 itself, then the numbers that begin with it");
        live.key(InputAction::Digit1, at, NOW);
        assert_eq!(live.keypad.typed, "11");
        assert_eq!(live.keypad_choice(), Some(10_810));
        // Any key restarts the two seconds.
        let later = at + Duration::from_millis(1500);
        live.key(InputAction::Down, later, NOW);
        assert_eq!(live.tick(later + Duration::from_millis(1900)), Effect::Nothing);
        assert_eq!(live.tick(later + KEYPAD_SHOW), Effect::Tune(10_810));
        assert_eq!(live.current, Some(10_810));
    }

    #[test]
    fn a_number_nothing_has_closes_the_entry_without_tuning() {
        let at = Instant::now();
        let mut live = live(3);
        live.tuning(10_800, at);
        live.key(InputAction::Digit9, at, NOW);
        assert!(live.keypad.candidates.is_empty());
        assert_eq!(live.tick(at + KEYPAD_SHOW), Effect::Paint);
        assert_eq!(live.current, Some(10_800));
        assert_eq!(live.layer, Layer::None);
    }

    #[test]
    fn a_sixth_digit_starts_the_number_over() {
        let at = Instant::now();
        let mut live = live(3);
        for _ in 0..MAX_DIGITS {
            live.key(InputAction::Digit2, at, NOW);
        }
        live.key(InputAction::Digit3, at, NOW);
        assert_eq!(live.keypad.typed, "3");
        assert_eq!(live.keypad_choice(), Some(10_802));
    }

    #[test]
    fn a_digit_over_the_menu_goes_to_the_number_entry() {
        let at = Instant::now();
        let mut live = live(3);
        live.key(InputAction::Menu, at, NOW);
        live.key(InputAction::Digit2, at, NOW);
        assert_eq!(live.layer, Layer::Keypad);
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Tune(10_801));
    }

    #[test]
    fn the_menu_goes_after_seven_seconds_of_no_key() {
        let at = Instant::now();
        let mut live = live(3);
        live.key(InputAction::Menu, at, NOW);
        let later = at + Duration::from_secs(5);
        live.key(InputAction::Right, later, NOW);
        assert_eq!(live.tick(at + MENU_SHOW), Effect::Nothing, "the key kept it up");
        assert_eq!(live.tick(later + MENU_SHOW), Effect::Paint);
        assert_eq!(live.layer, Layer::None);
    }

    #[test]
    fn the_menu_offers_the_guide_first_and_the_recent_channels_after() {
        let at = Instant::now();
        let mut live = live(4);
        live.tuning(10_800, at);
        live.tuning(10_802, at);
        let rows = live.menu_rows();
        assert_eq!(rows[0].1, [Card::Guide, Card::ChannelList, Card::Favorites, Card::Recent(10_800)]);
        assert_eq!(rows[1].0, "TV seçenekleri");
        live.key(InputAction::Ok, at, NOW);
        for _ in 0..3 {
            live.key(InputAction::Right, at, NOW);
        }
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Tune(10_800));
    }

    #[test]
    fn the_guide_opens_on_the_channel_on_now_at_the_half_hour_before_now() {
        let at = Instant::now();
        let mut live = live(9);
        live.tuning(10_805, at);
        live.guide.insert(10_805, vec![event(NOW - 600, 60, "Haber"), event(NOW + 3000, 30, "Hava")]);
        assert_eq!(live.key(InputAction::Guide, at, NOW), Effect::Paint);
        assert_eq!(live.layer, Layer::Guide);
        assert_eq!(live.guide_focus.start, floor_half_hour(NOW - 60));
        assert_eq!(live.guide_focus.row, 5);
        assert_eq!(live.guide_focus.top, 3, "the focused row parks third from the top");
        assert_eq!(live.focused_entry().unwrap().event.unwrap().title, "Haber");
        assert_eq!(live.key(InputAction::Right, at, NOW), Effect::Paint);
        assert_eq!(live.focused_entry().unwrap().event.unwrap().title, "Hava");
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Nothing, "a later programme is only read");
        live.key(InputAction::Left, at, NOW);
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Tune(10_805));
        assert_eq!(live.layer, Layer::None, "tuning from the guide closes it");
    }

    #[test]
    fn a_channel_with_no_guide_is_one_gap_and_nothing_is_made_up() {
        let mut live = live(2);
        let entries = live.entries(10_800, NOW, NOW + 3600);
        assert_eq!(entries, [Entry { start: NOW, end: NOW + 3600, event: None }]);
        live.guide.insert(10_801, vec![event(NOW + 600, 10, "A"), event(NOW + 1800, 10, "B")]);
        let entries = live.entries(10_801, NOW, NOW + 3600);
        let shape: Vec<(i64, bool)> = entries.iter().map(|e| (e.start - NOW, e.event.is_some())).collect();
        assert_eq!(shape, [(0, false), (600, true), (1200, false), (1800, true), (2400, false)]);
        assert!(!live.has_any_guide() || live.guide.contains_key(&10_801));
    }

    #[test]
    fn up_and_down_in_the_guide_keep_the_time_the_focus_is_at() {
        let at = Instant::now();
        let mut live = live(3);
        live.tuning(10_800, at);
        live.guide.insert(10_800, vec![event(NOW - 600, 30, "A0"), event(NOW + 1200, 60, "A1")]);
        live.guide.insert(10_801, vec![event(NOW - 1200, 120, "B0")]);
        live.key(InputAction::Guide, at, NOW);
        live.key(InputAction::Right, at, NOW);
        assert_eq!(live.focused_entry().unwrap().event.unwrap().title, "A1");
        live.key(InputAction::Down, at, NOW);
        assert_eq!(live.focused_entry().unwrap().event.unwrap().title, "B0");
        live.key(InputAction::Up, at, NOW);
        assert_eq!(live.focused_entry().unwrap().event.unwrap().title, "A1", "back on the same time");
    }

    #[test]
    fn the_guide_view_follows_the_focus_to_the_right() {
        let at = Instant::now();
        let mut live = live(1);
        let start = floor_half_hour(NOW - 60);
        live.guide.insert(10_800, (0..10).map(|i| event(start + i * 3600, 60, "x")).collect());
        live.key(InputAction::Guide, at, NOW);
        for _ in 0..3 {
            live.key(InputAction::Right, at, NOW);
        }
        let g = live.guide_focus;
        assert!(g.at >= g.start && g.at < g.start + GUIDE_VIEW - GUIDE_RIGHT_MARGIN, "{g:?}");
        for _ in 0..3 {
            live.key(InputAction::Left, at, NOW);
        }
        assert_eq!(live.guide_focus.start, start, "and back, never before the half hour before now");
    }

    #[test]
    fn favourites_are_toggled_in_their_editor_and_tuned_from_their_list() {
        let at = Instant::now();
        let mut live = live(3);
        live.tuning(10_800, at);
        live.open_panel(Panel::Favorites, at);
        // Nothing yet: the one row is the editor.
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Paint);
        assert_eq!(live.layer, Layer::Panel(Panel::EditFavorites));
        live.key(InputAction::Down, at, NOW);
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Favorites);
        assert!(live.favorites.contains(&10_801));
        assert_eq!(live.key(InputAction::Back, at, NOW), Effect::Paint);
        assert_eq!(live.layer, Layer::Panel(Panel::Favorites));
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Tune(10_801));
    }

    #[test]
    fn the_settings_offer_tkgs_update() {
        let live = live(1);
        let rows = live.panel_rows(Panel::Settings, NOW);
        assert!(rows.iter().any(|r| r.label == "Kanal kurulumu" && r.enabled));
    }

    #[test]
    fn subtitles_offer_off_and_the_players_own_tracks() {
        let at = Instant::now();
        let mut live = live(1);
        live.subtitles = vec![TrackRow { id: 3, label: "Türkçe".into(), detail: "dvb".into(), selected: false }];
        live.open_panel(Panel::Subtitles, at);
        assert!(live.panel_rows(Panel::Subtitles, NOW)[0].checked, "off is what is on");
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Subtitle(-1));
        live.key(InputAction::Down, at, NOW);
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Subtitle(3));
        live.audio.clear();
        assert_eq!(live.panel_rows(Panel::Audio, NOW)[0].label, "Bu kanalda yok");
    }

    #[test]
    fn with_no_channels_nothing_is_tuned_and_the_menu_still_opens() {
        let at = Instant::now();
        let mut live = live(0);
        assert_eq!(live.key(InputAction::Up, at, NOW), Effect::Nothing);
        assert_eq!(live.key(InputAction::Digit1, at, NOW), Effect::Nothing);
        assert_eq!(live.key(InputAction::Guide, at, NOW), Effect::Nothing);
        assert_eq!(live.key(InputAction::Ok, at, NOW), Effect::Paint);
        assert_eq!(live.layer, Layer::Menu);
        assert!(!live.banner_shown(at), "no channel, no banner");
    }

    #[test]
    fn a_channel_is_scrambled_only_when_nearly_all_of_it_is() {
        let selection = |all: u64, scrambled: u64| serde_json::json!({"es_packets": all, "scrambled_packets": scrambled});
        assert!(scrambled(&selection(86_000, 85_713)));
        assert!(!scrambled(&selection(86_000, 0)));
        assert!(!scrambled(&selection(100, 100)), "too few to say");
        assert!(!scrambled(&serde_json::json!({})), "an older receiver says nothing");
    }

    #[test]
    fn progress_is_how_far_into_the_programme_now_is() {
        let e = event(NOW, 60, "x");
        assert_eq!(progress(&e, NOW + 1800), 0.5);
        assert_eq!(progress(&e, NOW - 1), 0.0);
        assert_eq!(progress(&e, NOW + 7200), 1.0);
    }
}
