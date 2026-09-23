//! The display settings: the resolution, its refresh, and the colour mode at
//! it -- driven by four arrows, Ok and Back.
//!
//! Two settings are kept apart, because they are two different facts:
//!
//! * the **kept** one, which the daemon has written down for this display --
//!   or the one on trial, while the daemon waits to hear whether to keep it;
//! * the **draft**, which is what the screen shows as chosen and nothing
//!   outside this process has seen until "Uygula".
//!
//! Moving the focus never changes a value. Ok chooses the thing under the
//! focus into the draft; a cell that cannot be sent says why instead. Nothing
//! is decided here: every mode, every colour cell and every reason comes from
//! the offer the platform computed by the HDMI rules
//! (`mediabox_platform::output`), and the daemon checks a setting again before
//! it goes on trial.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mediabox_core::{
    ColorFormat, ColorMode, ColourCell, OUTPUT_TRIAL_SECONDS, OutputModeOffer, OutputOffer,
    OutputSetting, OutputStatus, ResolutionChoice,
};

/// Where the focus is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// `Otomatik`, then one row per size.
    Resolutions,
    /// The refresh rates of the size under the focus.
    Rates,
    /// `Otomatik`, then the format × depth cells of the draft's mode.
    Colours,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Apply,
    Undo,
    Auto,
    Edid,
}

const ACTS: [Act; 4] = [Act::Apply, Act::Undo, Act::Auto, Act::Edid];

/// What a press asks the application to do beyond redrawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Nothing,
    Changed,
    /// Put this on the wire, on trial.
    Try(ResolutionChoice, Option<ColorMode>),
    Keep,
    Revert,
}

/// What a move did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Moved,
    Unchanged,
    /// Left from the resolutions: back to the section list.
    Leave,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Sheet {
    None,
    /// "Bu görüntü kalsın mı?", until `deadline`. 0 keeps, 1 goes back.
    Confirm { deadline: Instant, focus: usize },
    Edid,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Draft {
    resolution: ResolutionChoice,
    colours: BTreeMap<String, ColorMode>,
}

impl From<&OutputSetting> for Draft {
    fn from(setting: &OutputSetting) -> Self {
        Draft {
            resolution: setting.resolution,
            colours: setting.colours.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Output {
    status: Option<OutputStatus>,
    draft: Option<Draft>,
    base: Option<Draft>,
    zone: Zone,
    /// Before the actions were reached, to go back up to.
    above: Zone,
    /// 0 is `Otomatik`; a size is its group index plus one.
    res: usize,
    /// The group whose rates are shown: the one under the focus, or the
    /// draft's.
    open: usize,
    rate: usize,
    /// 0 is `Otomatik`; a cell is its index plus one.
    cell: usize,
    act: usize,
    sheet: Sheet,
    note: String,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            status: None,
            draft: None,
            base: None,
            zone: Zone::Resolutions,
            above: Zone::Resolutions,
            res: 0,
            open: 0,
            rate: 0,
            cell: 0,
            act: 0,
            sheet: Sheet::None,
            note: String::new(),
        }
    }
}

impl Output {
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------- the data

    /// Takes the daemon's latest answer. The draft is taken from it once, and
    /// again after a trial ends; in between it is the viewer's, so a poll
    /// never undoes a choice somebody just made.
    pub fn load(&mut self, status: Option<OutputStatus>) {
        self.status = status.filter(|status| status.offer.is_some());
        let Some(status) = &self.status else { return };
        let kept = status
            .trial
            .as_ref()
            .map(|trial| Draft::from(&trial.setting))
            .unwrap_or_else(|| Draft::from(&status.setting));
        if self.draft.is_none() {
            self.draft = Some(kept.clone());
        }
        self.base = Some(kept);
        // A trial the screen has not heard about -- started from the web or
        // `mediaboxctl` -- is asked about here too.
        match (&status.trial, &self.sheet) {
            (Some(trial), Sheet::None) => self.ask(trial.seconds_left),
            (None, Sheet::Confirm { .. }) => self.sheet = Sheet::None,
            _ => {}
        }
        self.settle();
    }

    /// The daemon put a setting on trial: ask whether to keep it.
    pub fn trial(&mut self, seconds: u32) {
        self.ask(seconds);
    }

    fn ask(&mut self, seconds: u32) {
        self.sheet = Sheet::Confirm {
            deadline: Instant::now() + Duration::from_secs(seconds.into()),
            // "Koru" first, as the reference does: the question is only seen
            // by somebody whose display shows it.
            focus: 0,
        };
    }

    /// The trial is over, kept or not: the draft is what the daemon says next.
    pub fn trial_ended(&mut self, note: &str) {
        self.sheet = Sheet::None;
        self.draft = None;
        self.note = note.to_string();
    }

    pub fn available(&self) -> bool {
        self.status.is_some() && self.draft.is_some()
    }

    pub fn asking(&self) -> bool {
        matches!(self.sheet, Sheet::Confirm { .. })
    }

    fn offer(&self) -> Option<&OutputOffer> {
        self.status.as_ref()?.offer.as_ref()
    }

    fn draft(&self) -> Draft {
        self.draft.clone().unwrap_or_default()
    }

    /// The mode the draft resolves to on this display.
    fn draft_mode(&self) -> Option<&OutputModeOffer> {
        self.offer()?.resolve(self.draft.as_ref()?.resolution)
    }

    /// The draft's colour choice at its own mode, `None` for `Auto`.
    fn draft_colour(&self) -> Option<ColorMode> {
        let mode = self.draft_mode()?;
        self.draft.as_ref()?.colours.get(&mode.label).copied()
    }

    /// Whether the draft would change what the daemon has: another mode, or
    /// another colour at the draft's mode.
    pub fn unsaved(&self) -> bool {
        let (Some(draft), Some(base)) = (&self.draft, &self.base) else {
            return false;
        };
        let label = self.draft_mode().map(|mode| mode.label.clone()).unwrap_or_default();
        draft.resolution != base.resolution || draft.colours.get(&label) != base.colours.get(&label)
    }

    fn groups(&self) -> usize {
        self.offer().map_or(0, |offer| offer.groups.len())
    }

    fn open_modes(&self) -> &[OutputModeOffer] {
        self.offer()
            .and_then(|offer| offer.groups.get(self.open))
            .map_or(&[], |group| group.modes.as_slice())
    }

    fn cells(&self) -> &[ColourCell] {
        self.draft_mode().map_or(&[], |mode| mode.cells.as_slice())
    }

    /// Where a cell sits in the grid: its row (the format) and its column
    /// (the depth). 4:2:2 is one cell across both columns: HDMI carries it
    /// in a twelve-bit container whatever the depth (HDMI 1.3 s6.5).
    fn place(cell: &ColourCell) -> (usize, usize, bool) {
        let row = match cell.mode.format {
            ColorFormat::Rgb => 0,
            ColorFormat::Ycbcr444 => 1,
            ColorFormat::Ycbcr422 => 2,
            ColorFormat::Ycbcr420 => 3,
        };
        if cell.mode.format == ColorFormat::Ycbcr422 {
            (row, 0, true)
        } else {
            (row, usize::from(cell.mode.bits > 8), false)
        }
    }

    /// The group a resolution choice falls in.
    fn group_of(&self, choice: ResolutionChoice) -> Option<usize> {
        let offer = self.offer()?;
        let mode = offer.resolve(choice)?;
        offer
            .groups
            .iter()
            .position(|group| group.width == mode.width && group.height == mode.height)
    }

    /// Keeps every index inside what is there now.
    fn settle(&mut self) {
        self.res = self.res.min(self.groups());
        self.open = self.open.min(self.groups().saturating_sub(1));
        self.rate = self.rate.min(self.open_modes().len().saturating_sub(1));
        self.cell = self.cell.min(self.cells().len());
        self.act = self.act.min(ACTS.len() - 1);
    }

    #[cfg(test)]
    pub fn focus(&self) -> (Zone, usize, usize, usize) {
        (self.zone, self.res, self.rate, self.cell)
    }

    #[cfg(test)]
    pub fn chosen(&self) -> (ResolutionChoice, Option<ColorMode>) {
        (self.draft().resolution, self.draft_colour())
    }

    /// The remote comes in from the section list: onto the draft's size.
    pub fn enter(&mut self) {
        self.zone = Zone::Resolutions;
        let draft = self.draft();
        match draft.resolution {
            ResolutionChoice::Auto => {
                self.res = 0;
                self.open = self.group_of(ResolutionChoice::Auto).unwrap_or(0);
            }
            choice => {
                let group = self.group_of(choice).unwrap_or(0);
                self.res = group + 1;
                self.open = group;
            }
        }
        self.rate = self.rate_of_draft();
        self.settle();
    }

    fn rate_of_draft(&self) -> usize {
        let choice = self.draft().resolution;
        self.open_modes()
            .iter()
            .position(|mode| mode.is(choice))
            .unwrap_or(0)
    }

    // ------------------------------------------------------------ moving

    pub fn step(&mut self, dx: i32, dy: i32) -> Nav {
        let before = (self.zone, self.res, self.rate, self.cell, self.act, self.open);
        match &mut self.sheet {
            Sheet::Confirm { focus, .. } => {
                if dx != 0 {
                    *focus = if dx > 0 { 1 } else { 0 };
                    return Nav::Moved;
                }
                return Nav::Unchanged;
            }
            Sheet::Edid => return Nav::Unchanged,
            Sheet::None => {}
        }
        self.note.clear();
        match self.zone {
            Zone::Resolutions => {
                if dx < 0 {
                    return Nav::Leave;
                }
                if dx > 0 {
                    self.zone = if self.res == 0 { Zone::Colours } else { Zone::Rates };
                    if self.zone == Zone::Rates {
                        self.rate = self.rate_of_draft();
                    }
                    self.cell = 0;
                } else if dy > 0 && self.res == self.groups() {
                    self.down_to_actions();
                } else if dy != 0 {
                    self.res = (self.res as i32 + dy).clamp(0, self.groups() as i32) as usize;
                    // The rates beside follow the size under the focus.
                    self.open = match self.res {
                        0 => self.group_of(ResolutionChoice::Auto).unwrap_or(0),
                        at => at - 1,
                    };
                    self.rate = self.rate_of_draft();
                }
            }
            Zone::Rates => {
                let count = self.open_modes().len();
                if dx < 0 {
                    self.zone = Zone::Resolutions;
                } else if dx > 0 {
                    self.zone = Zone::Colours;
                    self.cell = 0;
                } else if dy > 0 && self.rate + 1 >= count {
                    self.down_to_actions();
                } else if dy != 0 {
                    self.rate = (self.rate as i32 + dy).clamp(0, count as i32 - 1) as usize;
                }
            }
            Zone::Colours => self.step_colours(dx, dy),
            Zone::Actions => {
                if dy < 0 {
                    self.zone = self.above;
                } else if dx != 0 {
                    self.act = (self.act as i32 + dx).clamp(0, ACTS.len() as i32 - 1) as usize;
                }
            }
        }
        self.settle();
        if before == (self.zone, self.res, self.rate, self.cell, self.act, self.open) {
            Nav::Unchanged
        } else {
            Nav::Moved
        }
    }

    fn down_to_actions(&mut self) {
        self.above = self.zone;
        self.zone = Zone::Actions;
        // Onto "Uygula" when there is something to apply.
        self.act = 0;
    }

    fn step_colours(&mut self, dx: i32, dy: i32) {
        let cells: Vec<(usize, usize, bool)> = self.cells().iter().map(Self::place).collect();
        if self.cell == 0 {
            if dx < 0 {
                self.zone = if self.draft().resolution == ResolutionChoice::Auto {
                    Zone::Resolutions
                } else {
                    Zone::Rates
                };
            } else if dy > 0 {
                if cells.is_empty() {
                    self.down_to_actions();
                } else {
                    self.cell = 1;
                }
            }
            return;
        }
        let (row, col, _) = cells[self.cell - 1];
        let find = |row: usize, col: usize| {
            cells
                .iter()
                .position(|(r, c, span)| *r == row && (*c == col || *span))
                .map(|index| index + 1)
        };
        if dx != 0 {
            let target = col as i32 + dx;
            match find(row, target.max(0) as usize).filter(|at| target >= 0 && *at != self.cell) {
                Some(at) => self.cell = at,
                None if dx < 0 => {
                    self.zone = if self.draft().resolution == ResolutionChoice::Auto {
                        Zone::Resolutions
                    } else {
                        Zone::Rates
                    };
                }
                None => {}
            }
            return;
        }
        if dy != 0 {
            let rows: Vec<usize> = {
                let mut rows: Vec<usize> = cells.iter().map(|(r, _, _)| *r).collect();
                rows.dedup();
                rows
            };
            let at = rows.iter().position(|r| *r == row).unwrap_or(0) as i32 + dy;
            if at < 0 {
                self.cell = 0;
            } else if at as usize >= rows.len() {
                self.down_to_actions();
            } else if let Some(cell) = find(rows[at as usize], col) {
                self.cell = cell;
            }
        }
    }

    // ------------------------------------------------------------ pressing

    pub fn press(&mut self) -> Press {
        match self.sheet.clone() {
            Sheet::Confirm { focus, .. } => {
                return if focus == 0 { Press::Keep } else { Press::Revert };
            }
            Sheet::Edid => {
                self.sheet = Sheet::None;
                return Press::Changed;
            }
            Sheet::None => {}
        }
        self.note.clear();
        match self.zone {
            Zone::Resolutions => {
                if self.res == 0 {
                    if let Some(draft) = self.draft.as_mut() {
                        draft.resolution = ResolutionChoice::Auto;
                    }
                    self.zone = Zone::Colours;
                    self.cell = 0;
                } else {
                    self.open = self.res - 1;
                    self.zone = Zone::Rates;
                    self.rate = self.rate_of_draft();
                }
                Press::Changed
            }
            Zone::Rates => {
                let Some(mode) = self.open_modes().get(self.rate).cloned() else {
                    return Press::Nothing;
                };
                if mode.allowed().next().is_none() {
                    self.note = format!("{}: bu bağlantıda hiçbir renk biçimi sığmıyor.", mode.label);
                    return Press::Changed;
                }
                if let Some(draft) = self.draft.as_mut() {
                    draft.resolution = mode.choice();
                }
                self.zone = Zone::Colours;
                self.cell = 0;
                Press::Changed
            }
            Zone::Colours => {
                let Some(mode) = self.draft_mode().cloned() else {
                    return Press::Nothing;
                };
                if self.cell == 0 {
                    if let Some(draft) = self.draft.as_mut() {
                        draft.colours.remove(&mode.label);
                    }
                    return Press::Changed;
                }
                let cell = mode.cells[self.cell - 1].clone();
                match cell.refused {
                    Some(refusal) => self.note = refusal.text(cell.mode.format),
                    None => {
                        if let Some(draft) = self.draft.as_mut() {
                            draft.colours.insert(mode.label.clone(), cell.mode);
                        }
                    }
                }
                Press::Changed
            }
            Zone::Actions => match ACTS[self.act] {
                Act::Apply if self.unsaved() => {
                    Press::Try(self.draft().resolution, self.draft_colour())
                }
                Act::Apply => Press::Nothing,
                Act::Undo => {
                    if !self.unsaved() {
                        return Press::Nothing;
                    }
                    self.draft = self.base.clone();
                    self.note = "Taslak bırakıldı.".into();
                    Press::Changed
                }
                Act::Auto => {
                    self.draft = Some(Draft::default());
                    Press::Changed
                }
                Act::Edid => {
                    self.sheet = Sheet::Edid;
                    Press::Changed
                }
            },
        }
    }

    /// Back. `None` when there is nothing for it to do here, so it leaves.
    pub fn back(&mut self) -> Option<Press> {
        match self.sheet {
            Sheet::Confirm { .. } => Some(Press::Revert),
            Sheet::Edid => {
                self.sheet = Sheet::None;
                Some(Press::Changed)
            }
            Sheet::None if self.zone == Zone::Actions => {
                self.zone = self.above;
                Some(Press::Changed)
            }
            Sheet::None => None,
        }
    }

    // ------------------------------------------------------------ the view

    pub fn view(&self) -> View {
        let (Some(status), Some(offer)) = (self.status.as_ref(), self.offer()) else {
            return View {
                message: self
                    .status
                    .as_ref()
                    .and_then(|status| status.error.clone())
                    .unwrap_or_else(|| "Ekran okunuyor…".into()),
                ..View::default()
            };
        };
        let draft = self.draft();
        let focus = |zone: Zone| self.sheet == Sheet::None && self.zone == zone;
        let unsaved = self.unsaved();
        let wire = status.wire.clone().unwrap_or_default();
        let wire_mode = offer.mode(&wire.mode);
        let link = &offer.link;
        let ceiling = if link.max_character_rate_khz > 0 {
            link.max_character_rate_khz.min(link.source_max_khz)
        } else {
            link.source_max_khz
        };
        let load = match (wire_mode, wire.colour) {
            (Some(mode), Some(colour)) => colour.character_rate_khz(mode.pixel_clock_khz),
            _ => 0,
        };

        let (state, state_tone) = if self.asking() {
            ("Onay bekliyor".to_string(), "accent")
        } else if unsaved {
            ("Uygulanmadı".to_string(), "accent")
        } else if wire_mode.is_some_and(|mode| mode.hdr10()) {
            ("Etkin · HDR hazır".to_string(), "good")
        } else {
            ("Etkin · HDR yok".to_string(), "good")
        };

        // Sizes.
        let draft_group = self.group_of(draft.resolution);
        let wire_group = wire_mode.and_then(|mode| {
            offer
                .groups
                .iter()
                .position(|group| group.width == mode.width && group.height == mode.height)
        });
        let auto_mode = offer.mode(&offer.auto);
        let mut resolutions = vec![RowView {
            title: "Otomatik".into(),
            sub: auto_mode.map(label).unwrap_or_default(),
            selected: draft.resolution == ResolutionChoice::Auto,
            open: false,
            focused: focus(Zone::Resolutions) && self.res == 0,
            now: status.setting.resolution == ResolutionChoice::Auto && status.trial.is_none(),
            hdr: false,
            divider: false,
            refused: false,
        }];
        let mut divided = false;
        for (index, group) in offer.groups.iter().enumerate() {
            let fastest = group
                .modes
                .iter()
                .filter(|mode| mode.allowed().next().is_some())
                .map(|mode| mode.refresh_mhz)
                .max();
            resolutions.push(RowView {
                title: format!("{}×{}", group.width, group.height),
                sub: match (group.name.as_str(), fastest) {
                    ("", Some(hz)) => format!("{} Hz'e kadar", hz_text(hz)),
                    (name, Some(hz)) => format!("{name} · {} Hz'e kadar", hz_text(hz)),
                    (name, None) => format!("{name} · sığmıyor"),
                },
                selected: draft.resolution != ResolutionChoice::Auto && draft_group == Some(index),
                open: self.open == index && !(focus(Zone::Resolutions) && self.res == index + 1),
                focused: focus(Zone::Resolutions) && self.res == index + 1,
                now: wire_group == Some(index) && status.setting.resolution != ResolutionChoice::Auto,
                hdr: group.modes.iter().any(OutputModeOffer::hdr10),
                divider: group.computer && !divided,
                refused: fastest.is_none(),
            });
            divided |= group.computer;
        }

        // Rates of the size under the focus.
        let rates = self
            .open_modes()
            .iter()
            .enumerate()
            .map(|(index, mode)| RowView {
                title: format!("{} Hz{}", hz_text(mode.refresh_mhz), if mode.interlaced { " i" } else { "" }),
                sub: if mode.preferred { "Ekranın tercihi".into() } else { String::new() },
                selected: mode.is(draft.resolution),
                open: false,
                focused: focus(Zone::Rates) && self.rate == index,
                now: mode.label == wire.mode,
                hdr: mode.hdr10(),
                divider: false,
                refused: mode.allowed().next().is_none(),
            })
            .collect();

        // Colours of the draft's mode.
        let mode = self.draft_mode();
        let chosen = self.draft_colour();
        let cells = mode
            .map(|mode| {
                mode.cells
                    .iter()
                    .enumerate()
                    .map(|(index, cell)| {
                        let (row, col, span) = Self::place(cell);
                        let on_wire = mode.label == wire.mode && wire.colour == Some(cell.mode);
                        CellView {
                            row,
                            col,
                            span,
                            state: match (cell.refused, chosen == Some(cell.mode), on_wire) {
                                (Some(_), _, _) => "Olmaz",
                                (None, true, _) => "Seçili",
                                (None, false, true) => "Şu an",
                                (None, false, false) => "Seçilebilir",
                            }
                            .into(),
                            mhz: format!("{} MHz", cell.rate_khz / 1000),
                            fill: (f64::from(cell.rate_khz) / f64::from(link.source_max_khz)).min(1.0),
                            tick: f64::from(ceiling) / f64::from(link.source_max_khz),
                            ok: cell.refused.is_none(),
                            selected: chosen == Some(cell.mode),
                            focused: focus(Zone::Colours) && self.cell == index + 1,
                            hdr: cell.refused.is_none() && cell.mode.carries_hdr(),
                            now: on_wire,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let reason = self.reason(mode, ceiling);

        let actions = ACTS
            .iter()
            .enumerate()
            .map(|(index, act)| ActionView {
                label: match act {
                    Act::Apply => "Uygula",
                    Act::Undo => "Geri al",
                    Act::Auto => "Otomatiğe dön",
                    Act::Edid => "EDID bilgileri",
                }
                .into(),
                enabled: match act {
                    Act::Apply | Act::Undo => unsaved,
                    Act::Auto => draft != Draft::default(),
                    Act::Edid => true,
                },
                focused: focus(Zone::Actions) && self.act == index,
            })
            .collect();

        let note = if !self.note.is_empty() {
            self.note.clone()
        } else if let Some(error) = crate::platform::output_error() {
            error
        } else if unsaved {
            format!(
                "Taslak ekrana gönderilmedi. Uygula'ya basınca {OUTPUT_TRIAL_SECONDS} saniye içinde onay istenir."
            )
        } else {
            "Seçim bu ekran için saklanır; başka bir ekran Otomatik ile açılır.".into()
        };

        let (sheet, seconds, confirm_focus) = match &self.sheet {
            Sheet::None => (0, 0, 0),
            Sheet::Confirm { deadline, focus } => (
                1,
                deadline.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32,
                *focus,
            ),
            Sheet::Edid => (2, 0, 0),
        };
        let (trial_line, previous_line) = status
            .trial
            .as_ref()
            .map(|trial| {
                let describe = |setting: &OutputSetting| {
                    offer
                        .resolve(setting.resolution)
                        .map(|mode| {
                            format!(
                                "{} · {}",
                                label(mode),
                                offer
                                    .colour(setting, mode)
                                    .map(colour_text)
                                    .unwrap_or_default()
                            )
                        })
                        .unwrap_or_default()
                };
                (describe(&trial.setting), describe(&trial.previous))
            })
            .unwrap_or_default();

        View {
            available: true,
            message: String::new(),
            sink: format!(
                "{} · {} · {} MHz",
                offer.sink_name.as_deref().unwrap_or("Ekran"),
                offer.connector,
                link.max_character_rate_khz / 1000
            ),
            wire_size: wire_mode
                .map(|mode| format!("{}×{}", mode.width, mode.height))
                .unwrap_or_else(|| "—".into()),
            wire_rate: wire_mode
                .map(|mode| format!("{} Hz{}", hz_text(mode.refresh_mhz), if mode.interlaced { " i" } else { "" }))
                .unwrap_or_default(),
            wire_format: wire.colour.map(format_short).unwrap_or_else(|| "—".into()),
            wire_bits: wire.colour.map(|mode| format!("{} bit", mode.bits)).unwrap_or_default(),
            load: if load > 0 { format!("{}", load / 1000) } else { "—".into() },
            load_max: format!("/ {} MHz", ceiling / 1000),
            load_fill: if ceiling > 0 { (f64::from(load) / f64::from(ceiling)).min(1.0) } else { 0.0 },
            state,
            state_tone: state_tone.into(),
            resolution_focus: resolutions
                .iter()
                .position(|row| row.focused)
                .or_else(|| resolutions.iter().position(|row| row.selected))
                .unwrap_or(0),
            divider_at: resolutions
                .iter()
                .position(|row| row.divider)
                .map_or(-1, |at| at as i32),
            resolutions,
            resolution_count: format!(
                "{} boyut · {} mod",
                offer.groups.len(),
                offer.modes().count()
            ),
            rate_title: offer
                .groups
                .get(self.open)
                .map(|group| format!("{}×{}", group.width, group.height))
                .unwrap_or_default(),
            rates,
            colour_title: mode.map(label).unwrap_or_default(),
            auto_colour: format!(
                "SDR: {} · HDR: {}",
                mode.and_then(|mode| mode.auto_sdr).map(colour_text).unwrap_or_else(|| "—".into()),
                mode.and_then(|mode| mode.auto_hdr).map(colour_text).unwrap_or_else(|| "yok".into())
            ),
            auto_colour_selected: chosen.is_none(),
            auto_colour_focused: focus(Zone::Colours) && self.cell == 0,
            cells,
            reason,
            actions,
            note,
            sheet,
            seconds,
            fraction: f64::from(seconds) / f64::from(OUTPUT_TRIAL_SECONDS),
            trial: trial_line,
            previous: previous_line,
            confirm_focus,
            edid: edid_rows(offer),
        }
    }

    /// One sentence about what is under the focus.
    fn reason(&self, mode: Option<&OutputModeOffer>, ceiling: u32) -> String {
        let Some(offer) = self.offer() else {
            return String::new();
        };
        match self.zone {
            Zone::Colours if self.cell > 0 => {
                let Some(cell) = mode.and_then(|mode| mode.cells.get(self.cell - 1)) else {
                    return String::new();
                };
                let name = colour_text(cell.mode);
                match cell.refused {
                    Some(refusal) => format!("{name}: seçilemez. {}", refusal.text(cell.mode.format)),
                    None => format!(
                        "{name}: gereken {} MHz, bu bağlantının {} MHz tavanına sığar.{}",
                        cell.rate_khz / 1000,
                        ceiling / 1000,
                        if cell.mode.carries_hdr() {
                            " HDR10 taşır."
                        } else {
                            " HDR10 için 10 bit gerekir."
                        }
                    ),
                }
            }
            Zone::Colours => "Otomatik renk: SDR'de önce RGB 8 bit, sığmazsa 4:2:0 8 bit. HDR film \
                 oynarken 10 bit taşıyan ilk biçim: RGB, 4:4:4, 4:2:2, 4:2:0 sırasıyla."
                .into(),
            Zone::Rates => match self.open_modes().get(self.rate) {
                Some(mode) if mode.allowed().next().is_none() => {
                    format!("{}: bu bağlantıda hiçbir renk biçimi sığmıyor.", label(mode))
                }
                Some(mode) => format!(
                    "{}: {} renk seçeneği. {}",
                    label(mode),
                    mode.allowed().count(),
                    if mode.hdr10() {
                        "HDR10 taşınabilir."
                    } else {
                        "HDR10 taşınamaz: bu modda 10 bit sığmıyor."
                    }
                ),
                None => String::new(),
            },
            Zone::Resolutions if self.res == 0 => format!(
                "Otomatik: ekranın tercih ettiği en-boy oranında en büyük mod, bağlantının taşıdığı \
                 en yüksek hızda. Bu ekranda: {}.",
                offer.mode(&offer.auto).map(label).unwrap_or_default()
            ),
            Zone::Resolutions => offer
                .groups
                .get(self.res - 1)
                .map(|group| {
                    format!(
                        "{}×{}: {} yenileme hızı. Ok ile hızları seçin.",
                        group.width,
                        group.height,
                        group.modes.len()
                    )
                })
                .unwrap_or_default(),
            Zone::Actions => String::new(),
        }
    }
}

/// `3840×2160 · 59.94 Hz`
fn label(mode: &OutputModeOffer) -> String {
    format!(
        "{}×{} · {} Hz{}",
        mode.width,
        mode.height,
        hz_text(mode.refresh_mhz),
        if mode.interlaced { " (geçmeli)" } else { "" }
    )
}

fn hz_text(refresh_mhz: u32) -> String {
    let hz = f64::from(refresh_mhz) / 1000.0;
    if (hz - hz.round()).abs() < 0.005 {
        format!("{}", hz.round() as u32)
    } else {
        format!("{hz:.3}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn format_short(mode: ColorMode) -> String {
    match mode.format {
        ColorFormat::Rgb => "RGB",
        ColorFormat::Ycbcr444 => "4:4:4",
        ColorFormat::Ycbcr422 => "4:2:2",
        ColorFormat::Ycbcr420 => "4:2:0",
    }
    .into()
}

/// `YCbCr 4:2:2 10 bit`
fn colour_text(mode: ColorMode) -> String {
    match mode.format {
        ColorFormat::Rgb => format!("RGB {} bit", mode.bits),
        _ => format!("YCbCr {} {} bit", format_short(mode), mode.bits),
    }
}

fn edid_rows(offer: &OutputOffer) -> Vec<(String, String)> {
    let link = &offer.link;
    let depths = |bits: &[u8]| {
        if bits.is_empty() {
            "yok".to_string()
        } else {
            bits.iter()
                .map(|bits| format!("{bits} bit"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let vics = |vics: &[u8]| {
        vics.iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    vec![
        ("Ekran adı".into(), offer.sink_name.clone().unwrap_or_else(|| "—".into())),
        ("Bağlayıcı".into(), offer.connector.clone()),
        (
            "Bu girişin tavanı".into(),
            if link.max_character_rate_khz > 0 {
                format!("{} MHz · {}", link.max_character_rate_khz / 1000, link.declared_by)
            } else {
                "bildirilmedi".into()
            },
        ),
        ("HDMI".into(), if link.is_hdmi { "evet".into() } else { "hayır (DVI)".into() }),
        (
            "YCbCr".into(),
            match (link.ycbcr444, link.ycbcr422) {
                (true, true) => "4:4:4, 4:2:2".into(),
                (true, false) => "4:4:4".into(),
                (false, true) => "4:2:2".into(),
                (false, false) => "yok".into(),
            },
        ),
        ("Derin renk (RGB)".into(), depths(&link.rgb_deep)),
        (
            "4:2:0 modları (VIC)".into(),
            match (link.y420_only.is_empty(), link.y420_also.is_empty()) {
                (true, true) => "yok".into(),
                (false, _) => format!("{} · yalnız 4:2:0", vics(&link.y420_only)),
                (true, false) => format!("{} · 4:2:0 da olur", vics(&link.y420_also)),
            },
        ),
        ("4:2:0 derin renk".into(), depths(&link.ycbcr420_deep)),
        (
            "HDR aktarımı".into(),
            match (link.st2084, link.hlg) {
                (true, true) => "SMPTE ST 2084 (HDR10), HLG".into(),
                (true, false) => "SMPTE ST 2084 (HDR10)".into(),
                (false, true) => "HLG".into(),
                (false, false) => "yok".into(),
            },
        ),
        ("Kimlik (checksum)".into(), offer.sink.clone()),
        (
            "Kaynak (bu kart)".into(),
            format!("{} MHz · {} bit · RGB, 4:4:4, 4:2:2, 4:2:0", link.source_max_khz / 1000, link.source_max_bits),
        ),
    ]
}

#[derive(Debug, Clone, Default)]
pub struct RowView {
    pub title: String,
    pub sub: String,
    pub selected: bool,
    pub open: bool,
    pub focused: bool,
    pub now: bool,
    pub hdr: bool,
    /// A "Bilgisayar modları" heading goes above this row.
    pub divider: bool,
    pub refused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CellView {
    pub row: usize,
    pub col: usize,
    pub span: bool,
    pub state: String,
    pub mhz: String,
    /// The rate as a share of what the source sends at most, and where the
    /// link's ceiling falls on the same scale.
    pub fill: f64,
    pub tick: f64,
    pub ok: bool,
    pub selected: bool,
    pub focused: bool,
    pub hdr: bool,
    pub now: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ActionView {
    pub label: String,
    pub enabled: bool,
    pub focused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct View {
    pub available: bool,
    pub message: String,
    pub sink: String,
    pub wire_size: String,
    pub wire_rate: String,
    pub wire_format: String,
    pub wire_bits: String,
    pub load: String,
    pub load_max: String,
    pub load_fill: f64,
    pub state: String,
    pub state_tone: String,
    pub resolutions: Vec<RowView>,
    pub resolution_focus: usize,
    pub divider_at: i32,
    pub resolution_count: String,
    pub rate_title: String,
    pub rates: Vec<RowView>,
    pub colour_title: String,
    pub auto_colour: String,
    pub auto_colour_selected: bool,
    pub auto_colour_focused: bool,
    pub cells: Vec<CellView>,
    pub reason: String,
    pub actions: Vec<ActionView>,
    pub note: String,
    /// 0 none, 1 the confirmation, 2 the EDID sheet.
    pub sheet: i32,
    pub seconds: u32,
    pub fraction: f64,
    pub trial: String,
    pub previous: String,
    pub confirm_focus: usize,
    pub edid: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::{OutputTrial, OutputWire};
    use mediabox_platform::video::{RK3588_HDMI, Timing};

    /// The Sony's 300 MHz input, as the Plus read it (see
    /// mediabox-platform/tests/output.rs for the whole of it).
    const SONY_300: &str = "\
00ffffffffffff004dd903f301010101011a0103809051780a0dc9a05747982712484c2108008180a9c0714f\
b3000101010101010101023a801871382d40582c45009f295300001e011d007251d01e206e2855009f295300\
001e000000fc00534f4e5920545620202a30300a000000fd00303e0e461e000a20202020202001d302034df0\
575d5e5f621f101405130420223c3e1216030711150206012c0d7f071507503d07bc570600830f00006e030c\
004000b83c2f008001020304e200f9e305ff01e50e60616566e3060d01011d8018711c1620582c25009f2953\
00009e0000000000000000000000000000000000000000000000000000000000000000d7";

    fn edid() -> Vec<u8> {
        (0..SONY_300.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&SONY_300[at..at + 2], 16).unwrap())
            .collect()
    }

    fn status(setting: OutputSetting) -> OutputStatus {
        let timings = [
            Timing::new(1920, 1080, 148_500, 2200, 1125, false, true),
            Timing::new(3840, 2160, 594_000, 4400, 2250, false, false),
            Timing::new(3840, 2160, 297_000, 4400, 2250, false, false),
            Timing::new(3840, 2160, 297_000, 5500, 2250, false, false),
            Timing::new(1280, 720, 74_250, 1650, 750, false, false),
        ];
        let offer = mediabox_platform::output::offer(&timings, &edid(), "HDMI-A-2", Some("SONY TV".into()), &RK3588_HDMI)
            .expect("an EDID");
        OutputStatus {
            wire: Some(OutputWire {
                mode: "3840x2160p60".into(),
                colour: Some(ColorMode::new(ColorFormat::Ycbcr420, 8)),
                bus_format: Some("UYYVYY8_0_5X24".into()),
                hdr: false,
            }),
            setting: OutputSetting { sink: offer.sink.clone(), ..setting },
            offer: Some(offer),
            trial: None,
            error: None,
        }
    }

    fn screen() -> Output {
        let mut screen = Output::new();
        screen.load(Some(status(OutputSetting::default())));
        screen.enter();
        screen
    }

    fn thirty() -> ResolutionChoice {
        ResolutionChoice::Fixed { width: 3840, height: 2160, refresh_mhz: 30_000, interlaced: false }
    }

    #[test]
    fn auto_comes_first_and_the_sizes_are_grouped() {
        let screen = screen();
        let view = screen.view();
        let titles: Vec<&str> = view.resolutions.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, vec!["Otomatik", "3840×2160", "1920×1080", "1280×720"]);
        assert_eq!(view.resolutions[0].sub, "3840×2160 · 60 Hz");
        assert!(view.resolutions[0].selected && view.resolutions[0].focused);
        assert_eq!(screen.focus().0, Zone::Resolutions);
    }

    #[test]
    fn moving_never_changes_what_is_chosen() {
        let mut screen = screen();
        let before = screen.chosen();
        for (dx, dy) in [(0, 1), (0, 1), (1, 0), (0, 1), (1, 0), (0, 1), (1, 0), (0, 1), (-1, 0), (0, -1)] {
            screen.step(dx, dy);
            assert_eq!(screen.chosen(), before);
        }
        assert!(!screen.unsaved());
    }

    #[test]
    fn a_size_opens_its_rates_and_a_rate_is_chosen_into_the_draft() {
        let mut screen = screen();
        screen.step(0, 1); // 3840x2160
        assert_eq!(screen.view().rates.len(), 3);
        assert_eq!(screen.press(), Press::Changed); // into its rates
        assert_eq!(screen.focus().0, Zone::Rates);
        screen.step(0, 1); // 30 Hz
        assert_eq!(screen.press(), Press::Changed);
        assert_eq!(screen.chosen(), (thirty(), None));
        assert!(screen.unsaved());
        // And the colours are now 4K30's: RGB fits there.
        let view = screen.view();
        assert_eq!(view.colour_title, "3840×2160 · 30 Hz");
        assert!(view.cells.iter().any(|cell| cell.ok && cell.state == "Seçilebilir"));
    }

    #[test]
    fn a_cell_that_cannot_be_sent_says_why_and_is_not_chosen() {
        let mut screen = screen();
        screen.step(1, 0); // Otomatik has no rates: straight to the colours
        assert_eq!(screen.focus().0, Zone::Colours);
        screen.step(0, 1); // RGB 8 at 4K60 on 300 MHz
        assert!(screen.view().reason.contains("4:2:0"), "{}", screen.view().reason);
        screen.press();
        assert_eq!(screen.chosen().1, None);
        assert!(!screen.unsaved());
    }

    #[test]
    fn apply_asks_for_a_trial_of_the_draft() {
        let mut screen = screen();
        screen.step(0, 1);
        screen.press();
        screen.step(0, 1);
        screen.press(); // 4K30
        screen.step(0, 1); // RGB 8
        screen.step(0, 1); // 4:4:4 8
        screen.step(0, 1); // 4:2:2
        screen.press();
        let colour = Some(ColorMode::new(ColorFormat::Ycbcr422, 10));
        assert_eq!(screen.chosen(), (thirty(), colour));
        // Down past the last row to the buttons; "Uygula" is first.
        for _ in 0..4 {
            screen.step(0, 1);
        }
        assert_eq!(screen.focus().0, Zone::Actions);
        assert_eq!(screen.press(), Press::Try(thirty(), colour));
    }

    #[test]
    fn a_trial_is_asked_about_and_back_takes_it_back() {
        let mut screen = screen();
        screen.trial(OUTPUT_TRIAL_SECONDS);
        assert!(screen.asking());
        assert_eq!(screen.view().sheet, 1);
        assert_eq!(screen.press(), Press::Keep, "Koru under the focus");
        screen.step(1, 0);
        assert_eq!(screen.press(), Press::Revert);
        assert_eq!(screen.back(), Some(Press::Revert));
        screen.trial_ended("Önceki moda dönüldü.");
        assert!(!screen.asking());
    }

    #[test]
    fn a_trial_started_elsewhere_is_asked_about_here_too() {
        let mut screen = Output::new();
        let mut started = status(OutputSetting::default());
        started.trial = Some(OutputTrial {
            setting: OutputSetting { resolution: thirty(), ..started.setting.clone() },
            previous: started.setting.clone(),
            seconds_left: 12,
        });
        screen.load(Some(started));
        assert!(screen.asking());
        assert_eq!(screen.view().seconds, 12);
    }

    #[test]
    fn a_poll_does_not_undo_a_choice_being_made() {
        let mut screen = screen();
        screen.step(0, 1);
        screen.press();
        screen.step(0, 1);
        screen.press();
        screen.load(Some(status(OutputSetting::default())));
        assert_eq!(screen.chosen().0, thirty());
    }

    #[test]
    fn the_wire_is_shown_as_the_kernel_reports_it() {
        let view = screen().view();
        assert_eq!(view.wire_size, "3840×2160");
        assert_eq!(view.wire_rate, "60 Hz");
        assert_eq!(view.wire_format, "4:2:0");
        assert_eq!(view.load, "297");
        assert_eq!(view.load_max, "/ 300 MHz");
    }
}
