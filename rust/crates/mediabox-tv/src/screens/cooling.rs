//! The fan curve editor: the readings, the curve as a graph, a table of its
//! points, and the buttons under them -- driven by four arrows, Ok and Back,
//! and by the digits of a keyboard when one is plugged in.
//!
//! Three curves are kept apart, because they are three different facts:
//!
//! * the **active** curve is the one in the running device tree -- what the
//!   kernel's pwm-fan is doing now;
//! * the **saved** curve is the one written for the next boot, if any;
//! * the **draft** is the one on the screen, which nothing outside this
//!   process has seen until "Kaydet".
//!
//! Moving the focus never changes a value. Ok on a cell of the table opens it
//! for editing; up and down then step it, or digits are typed into it, and Ok
//! keeps it while Back puts it back. Every change goes through [`FanCurve`]'s
//! own editing methods, so the draft is always a curve the control plane would
//! accept: a value that would break a rule is refused where it is typed, with
//! the reason, rather than saved and rejected later.
//!
//! The graph draws the points joined by straight lines, which is what the
//! kernel is given: the line as one trip per whole degree (see
//! [`FanCurve::kernel_steps`]), so the drawing and the fan differ by less than
//! a degree. The board's own curve, before one of these is saved, is steps,
//! and is drawn as steps.

use mediabox_core::{
    FAN_CURVE_MAX_POINTS, FAN_CURVE_MIN_POINTS, FAN_PWM_MAX, FAN_TEMP_MAX_C, FanBoardFix, FanCurve,
    FanProfile, FanStatus, fan_pwm_allowed, fan_pwm_from_percent, fan_pwm_percent,
};

/// The temperature the graph's right edge stands for. A little past the last
/// temperature a point may have, so the throttling band is on the graph.
pub const GRAPH_MAX_C: f64 = 80.0;

/// A tick label this close to the current temperature is left out: the
/// temperature's own label sits on the axis there.
const TICK_CLEAR_C: f64 = 7.0;

/// The same for the duty axis, as a share of full.
const TICK_CLEAR_DUTY: f64 = 0.07;

/// The longest number a cell takes: 75 °C, 100 %.
const TYPED_MAX: usize = 3;

/// Where the focus is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The preset chips.
    Profile,
    /// A cell of the point table: the temperature, the duty, and on the
    /// selected row, add a point after it and remove it.
    Table,
    /// Save, undo, reset, the technical readings.
    Actions,
}

/// The buttons along the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Save,
    Undo,
    Reset,
    Advanced,
}

/// What a press asks the application to do beyond redrawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Nothing,
    /// The draft or the view changed; redraw.
    Changed,
    Save(FanCurve),
    Reset,
    /// Restart the appliance. Asked for from the question after a save, which
    /// is itself the confirmation.
    Restart,
}

/// What a move did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Moved,
    Unchanged,
    /// Left from the first thing of a row: back to the section list.
    Leave,
}

/// A cell open for editing.
#[derive(Debug, Clone)]
struct Edit {
    row: usize,
    /// 0 the temperature, 1 the duty.
    col: usize,
    /// The draft before the cell was opened, for Back.
    before: FanCurve,
    /// Digits typed so far. Empty when the value is being stepped.
    typed: String,
}

#[derive(Debug, Clone)]
pub struct Cooling {
    status: Option<FanStatus>,
    draft: Option<FanCurve>,
    /// The curve the draft is measured against for "unsaved": the saved one,
    /// or the active one when nothing is saved.
    base: Option<FanCurve>,
    zone: Zone,
    /// The table's row, which is also the selected point.
    row: usize,
    /// The table's column: 0 the temperature, 1 the duty, 2 add, 3 remove.
    col: usize,
    /// Which chip or button of the other zones.
    idx: usize,
    edit: Option<Edit>,
    advanced: bool,
    /// The question after a save: restart now, or later.
    prompt: bool,
    prompt_idx: usize,
    /// One sentence under the table: why a value was refused, what was done.
    note: String,
}

impl Default for Cooling {
    fn default() -> Self {
        Self {
            status: None,
            draft: None,
            base: None,
            zone: Zone::Table,
            row: 0,
            col: 0,
            idx: 0,
            edit: None,
            advanced: false,
            prompt: false,
            prompt_idx: 1,
            note: String::new(),
        }
    }
}

impl Cooling {
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------- the data

    /// Takes the daemon's latest answer. The draft is taken from it once and
    /// is the viewer's after that: a poll must not move a point somebody just
    /// stepped.
    pub fn load(&mut self, status: Option<FanStatus>) {
        self.status = status;
        if self.draft.is_none() {
            let start = self
                .status
                .as_ref()
                .filter(|status| status.available)
                .and_then(|status| status.configured.clone().or(status.curve.clone()));
            self.base = start.clone();
            self.draft = start;
        }
        self.settle();
    }

    pub fn available(&self) -> bool {
        self.status.as_ref().is_some_and(|status| status.available) && self.draft.is_some()
    }

    #[cfg(test)]
    pub fn draft(&self) -> Option<&FanCurve> {
        self.draft.as_ref()
    }

    /// Whether the draft differs from what is saved (or running, when nothing
    /// is saved).
    pub fn unsaved(&self) -> bool {
        self.draft.is_some() && self.draft != self.base
    }

    /// Whether a cell is open or the restart question is up: Back belongs to
    /// the editor then, not to the section list.
    #[cfg(test)]
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// The daemon saved the draft: it is the new baseline, and the restart
    /// question is asked.
    pub fn saved(&mut self, curve: FanCurve, pending: bool) {
        self.base = Some(curve.clone());
        self.draft = Some(curve);
        self.edit = None;
        self.ask(pending);
        self.settle();
    }

    /// The daemon removed the saved curve: the board's own is what the next
    /// boot runs, which on the boards this ships on is the vendor curve.
    pub fn reset_done(&mut self, pending: bool) {
        let own = FanCurve::board();
        self.base = Some(own.clone());
        self.draft = Some(own);
        self.edit = None;
        self.row = 0;
        self.ask(pending);
        self.settle();
    }

    fn ask(&mut self, pending: bool) {
        self.prompt = pending;
        // "Later" first: a stray Ok must not restart the appliance.
        self.prompt_idx = 1;
    }

    fn count(&self) -> usize {
        self.draft.as_ref().map_or(0, |d| d.points.len())
    }

    fn last_row(&self) -> usize {
        self.count().saturating_sub(1)
    }

    // ------------------------------------------------------------ the focus

    fn zones(&self) -> Vec<Zone> {
        if self.advanced {
            vec![Zone::Profile, Zone::Actions]
        } else {
            vec![Zone::Profile, Zone::Table, Zone::Actions]
        }
    }

    /// The preset chips: label, and whether it can be pressed. The custom
    /// chip says which curve this is and is never pressed: a curve becomes
    /// custom by being edited.
    fn chips(&self) -> Vec<(FanProfile, bool)> {
        vec![
            (FanProfile::Balanced, true),
            (FanProfile::Cool, true),
            (FanProfile::Custom, false),
        ]
    }

    fn can_add(&self, row: usize) -> bool {
        self.count() < FAN_CURVE_MAX_POINTS
            && self
                .draft
                .clone()
                .is_some_and(|mut d| d.insert_after(row).is_some())
    }

    fn can_remove(&self, row: usize) -> bool {
        self.count() > FAN_CURVE_MIN_POINTS && row < self.last_row()
    }

    /// Which cells of a row can take the focus: the temperature always, the
    /// duty except on the full-duty point, add and remove where they would
    /// do something.
    ///
    /// Add and remove are on the row rather than under the table: under it,
    /// reaching them from the remote walked the focus over every row below
    /// and the selection with it, so a point could only ever be added next
    /// to the last one.
    fn row_cells(&self, row: usize) -> [bool; 4] {
        [
            true,
            row < self.last_row(),
            self.can_add(row),
            self.can_remove(row),
        ]
    }

    /// Whether the saved curve is not what is running, with no restart
    /// pending to explain it: a boot that did not load it. Not drawn -- the
    /// daemon's warning says it under the table -- but the badge does not
    /// call the curve running.
    fn stale(&self) -> bool {
        self.status.as_ref().is_some_and(|status| {
            !status.pending_reboot
                && status.configured.as_ref().is_some_and(|configured| {
                    !status
                        .curve
                        .as_ref()
                        .is_some_and(|running| running.same_kernel(configured))
                })
        })
    }

    pub fn actions(&self) -> Vec<(Act, bool)> {
        let valid = self.draft.as_ref().is_some_and(|d| d.validate().is_ok());
        vec![
            (Act::Save, self.unsaved() && valid),
            (Act::Undo, self.unsaved()),
            (Act::Reset, true),
            (Act::Advanced, true),
        ]
    }

    /// Whether each thing of a zone can take the focus.
    fn enabled(&self, zone: Zone) -> Vec<bool> {
        match zone {
            Zone::Profile => self.chips().iter().map(|(_, on)| *on).collect(),
            Zone::Table => vec![true],
            Zone::Actions => self.actions().iter().map(|(_, on)| *on).collect(),
        }
    }

    fn reachable(&self, zone: Zone) -> bool {
        self.enabled(zone).iter().any(|on| *on)
    }

    fn nearest(&self, zone: Zone, want: usize) -> usize {
        let enabled = self.enabled(zone);
        (0..enabled.len())
            .filter(|&i| enabled[i])
            .min_by_key(|&i| (i as i32 - want as i32).abs())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn focus(&self) -> (Zone, usize, usize) {
        match self.zone {
            Zone::Table => (self.zone, self.row, self.col),
            _ => (self.zone, self.idx, 0),
        }
    }

    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.row
    }

    /// Entering from the section list: onto the selected point's temperature,
    /// where most visits are headed.
    pub fn enter(&mut self) {
        self.zone = if self.advanced {
            Zone::Actions
        } else {
            Zone::Table
        };
        self.col = 0;
        self.idx = self.nearest(self.zone, 0);
        self.note.clear();
        self.settle();
    }

    /// Keeps the focus on something that exists and can be pressed, after
    /// anything that changed the table or the buttons.
    fn settle(&mut self) {
        self.row = self.row.min(self.last_row());
        self.col = self.nearest_col(self.row, self.col);
        if !self.zones().contains(&self.zone) {
            self.zone = Zone::Actions;
        }
        if self.zone != Zone::Table {
            if self.reachable(self.zone) {
                self.idx = self.nearest(self.zone, self.idx);
            } else {
                self.zone = if self.advanced {
                    Zone::Actions
                } else {
                    Zone::Table
                };
                self.idx = self.nearest(self.zone, 0);
            }
        }
    }

    fn nearest_col(&self, row: usize, want: usize) -> usize {
        let cells = self.row_cells(row);
        (0..cells.len())
            .filter(|&c| cells[c])
            .min_by_key(|&c| (c as i32 - want as i32).abs())
            .unwrap_or(0)
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> Nav {
        if !self.available() {
            return Nav::Unchanged;
        }
        if self.prompt {
            let want = (self.prompt_idx as i32 + dx.signum()).clamp(0, 1) as usize;
            if dx != 0 && want != self.prompt_idx {
                self.prompt_idx = want;
                return Nav::Moved;
            }
            return Nav::Unchanged;
        }
        if let Some(edit) = &self.edit {
            if dy == 0 {
                return Nav::Unchanged;
            }
            let (row, col) = (edit.row, edit.col);
            self.edit.as_mut().expect("open").typed.clear();
            let up = dy < 0;
            let draft = self.draft.as_mut().expect("available means a draft");
            let changed = if col == 0 {
                draft.step_temperature(row, up)
            } else {
                draft.step_pwm(row, up)
            };
            self.note = if changed {
                String::new()
            } else {
                self.limit_note(row, col)
            };
            return Nav::Moved;
        }

        let had_note = !self.note.is_empty();
        self.note.clear();
        let moved = if dy != 0 {
            self.step_vertical(dy.signum())
        } else if dx != 0 {
            match self.step_horizontal(dx.signum()) {
                Nav::Leave => return Nav::Leave,
                nav => nav == Nav::Moved,
            }
        } else {
            false
        };
        if moved || had_note {
            Nav::Moved
        } else {
            Nav::Unchanged
        }
    }

    fn step_vertical(&mut self, dy: i32) -> bool {
        if self.zone == Zone::Table {
            let row = self.row as i32 + dy;
            if (0..self.count() as i32).contains(&row) {
                self.row = row as usize;
                self.col = self.nearest_col(self.row, self.col);
                return true;
            }
        }
        let zones = self.zones();
        let mut at = zones.iter().position(|z| *z == self.zone).unwrap_or(0) as i32 + dy;
        while at >= 0 && (at as usize) < zones.len() {
            let zone = zones[at as usize];
            if self.reachable(zone) {
                self.zone = zone;
                if zone == Zone::Table {
                    self.row = if dy > 0 { 0 } else { self.last_row() };
                    self.col = self.nearest_col(self.row, 0);
                } else {
                    let want = match zone {
                        Zone::Profile => {
                            let profile = self.draft.as_ref().map(|d| d.profile);
                            self.chips()
                                .iter()
                                .position(|(p, _)| Some(*p) == profile)
                                .unwrap_or(0)
                        }
                        _ => 0,
                    };
                    self.idx = self.nearest(zone, want);
                }
                return true;
            }
            at += dy;
        }
        false
    }

    fn step_horizontal(&mut self, dx: i32) -> Nav {
        let enabled = if self.zone == Zone::Table {
            self.row_cells(self.row).to_vec()
        } else {
            self.enabled(self.zone)
        };
        let at = if self.zone == Zone::Table {
            self.col
        } else {
            self.idx
        };
        let found = if dx < 0 {
            (0..at).rev().find(|&i| enabled[i])
        } else {
            (at + 1..enabled.len()).find(|&i| enabled[i])
        };
        match found {
            Some(i) if self.zone == Zone::Table => {
                self.col = i;
                Nav::Moved
            }
            Some(i) => {
                self.idx = i;
                Nav::Moved
            }
            None if dx < 0 => Nav::Leave,
            None => Nav::Unchanged,
        }
    }

    // ------------------------------------------------------------ the keys

    pub fn press(&mut self) -> Press {
        if !self.available() {
            return Press::Nothing;
        }
        if self.prompt {
            self.prompt = false;
            return if self.prompt_idx == 0 {
                Press::Restart
            } else {
                self.note = "Yeni eğri bir sonraki açılışta etkin olacak.".into();
                Press::Changed
            };
        }
        if self.edit.is_some() {
            self.commit();
            return Press::Changed;
        }
        self.note.clear();
        let press = match self.zone {
            Zone::Profile => {
                let (profile, on) = self.chips()[self.idx];
                if !on {
                    return Press::Nothing;
                }
                self.draft = FanCurve::preset(profile);
                self.note = format!("{} yüklendi. Kaydedince uygulanır.", profile.label());
                Press::Changed
            }
            Zone::Table if self.col < 2 => {
                if !self.cell_editable(self.row, self.col) {
                    return Press::Nothing;
                }
                self.open(String::new());
                Press::Changed
            }
            Zone::Table => {
                let draft = self.draft.as_mut().expect("available means a draft");
                if self.col == 2 {
                    match draft.insert_after(self.row) {
                        // Onto the new point's temperature, ready to be typed.
                        Some(at) => {
                            self.row = at;
                            self.col = 0;
                            self.note = format!("Nokta #{} eklendi.", at + 1);
                            Press::Changed
                        }
                        None => Press::Nothing,
                    }
                } else if draft.remove(self.row) {
                    self.note = format!("Nokta #{} silindi.", self.row + 1);
                    Press::Changed
                } else {
                    Press::Nothing
                }
            }
            Zone::Actions => {
                let (act, on) = self.actions()[self.idx];
                if !on {
                    return Press::Nothing;
                }
                match act {
                    Act::Save => match self.draft.clone() {
                        Some(draft) if draft.validate().is_ok() => Press::Save(draft),
                        _ => Press::Nothing,
                    },
                    Act::Undo => {
                        self.draft = self.base.clone();
                        self.note = "Değişiklikler geri alındı.".into();
                        Press::Changed
                    }
                    Act::Reset => Press::Reset,
                    Act::Advanced => {
                        self.advanced = !self.advanced;
                        Press::Changed
                    }
                }
            }
        };
        self.settle();
        press
    }

    /// A character from a keyboard. Digits go into the cell being edited, or
    /// open the focused cell with that digit; everything else is not this
    /// screen's. Returns whether anything changed.
    pub fn typed(&mut self, c: char) -> bool {
        if !self.available() || self.prompt || !c.is_ascii_digit() {
            return false;
        }
        if let Some(edit) = self.edit.as_mut() {
            if edit.typed.len() < TYPED_MAX {
                edit.typed.push(c);
            }
            self.note.clear();
            return true;
        }
        if self.zone == Zone::Table && self.cell_editable(self.row, self.col) {
            self.note.clear();
            self.open(c.to_string());
            return true;
        }
        false
    }

    /// Backspace: a typed digit first, then the edit itself. Returns whether
    /// the editor used it; when it did not, it is Back.
    pub fn backspace(&mut self) -> bool {
        match self.edit.as_mut() {
            Some(edit) if !edit.typed.is_empty() => {
                edit.typed.pop();
                true
            }
            Some(_) => self.back(),
            None => false,
        }
    }

    /// Back: closes the question, or puts an open cell back as it was.
    /// Returns whether the editor used it; when it did not, the focus goes
    /// back to the section list.
    pub fn back(&mut self) -> bool {
        if self.prompt {
            self.prompt = false;
            self.note = "Yeni eğri bir sonraki açılışta etkin olacak.".into();
            return true;
        }
        if let Some(edit) = self.edit.take() {
            self.draft = Some(edit.before);
            self.note.clear();
            return true;
        }
        false
    }

    fn cell_editable(&self, row: usize, col: usize) -> bool {
        row < self.count() && (col == 0 || (col == 1 && row < self.last_row()))
    }

    fn open(&mut self, typed: String) {
        let before = self.draft.clone().expect("available means a draft");
        self.edit = Some(Edit {
            row: self.row,
            col: self.col,
            before,
            typed,
        });
    }

    /// Ok on an open cell: what was typed is set, or refused with the reason;
    /// what was stepped is kept.
    fn commit(&mut self) {
        let Some(edit) = self.edit.clone() else {
            return;
        };
        if edit.typed.is_empty() {
            self.edit = None;
            self.note.clear();
            return;
        }
        let value: u32 = edit.typed.parse().unwrap_or(u32::MAX);
        let draft = self.draft.as_mut().expect("available means a draft");
        let accepted = if edit.col == 0 {
            draft.set_temperature(edit.row, value)
        } else {
            fan_pwm_from_percent(value).is_some_and(|pwm| draft.set_pwm(edit.row, pwm))
        };
        if accepted {
            self.edit = None;
            self.note.clear();
        } else {
            self.note = if edit.col == 1
                && fan_pwm_from_percent(value).is_some_and(|pwm| !fan_pwm_allowed(pwm))
            {
                "%1–19 fanın durabildiği aralık: 0 ya da en az %20 yazın.".into()
            } else {
                self.limit_note(edit.row, edit.col)
            };
            if let Some(open) = self.edit.as_mut() {
                open.typed.clear();
            }
        }
    }

    /// The range a cell can take, in the words and units the table uses.
    fn limit_note(&self, row: usize, col: usize) -> String {
        let Some(draft) = &self.draft else {
            return String::new();
        };
        if col == 0 {
            let (low, high) = draft.temperature_bounds(row);
            let why = if row == self.last_row() && high == FAN_TEMP_MAX_C {
                " (kısma 75 °C'de başlar)"
            } else {
                " (komşu noktaların arası)"
            };
            format!("Sıcaklık {low}–{high} °C arasında olmalı{why}.")
        } else {
            let (low, high) = draft.pwm_bounds(row);
            format!(
                "PWM %{}–%{} arasında olmalı (komşu noktaların arası).",
                percent(low),
                percent(high)
            )
        }
    }

    // ------------------------------------------------------------ the text

    /// The technical readings, behind "Gelişmiş bilgiler". Not the main view:
    /// a person choosing a curve does not need the carrier's period.
    pub fn info(&self) -> Vec<(String, String)> {
        let Some(status) = &self.status else {
            return Vec::new();
        };
        let frequency = match status.pwm_frequency_hz {
            Some(hz) if hz >= 1000.0 => format!("{:.0} kHz", hz / 1000.0),
            Some(hz) => format!("{hz:.0} Hz"),
            None => "—".into(),
        };
        let fix = match status.board_fix {
            FanBoardFix::Active => "Etkin",
            FanBoardFix::Missing => "Gerekli ama etkin değil",
            FanBoardFix::NotNeeded => "Bu kartta gerekmiyor",
        };
        vec![
            ("Denetleyen".into(), "Çekirdek · pwm-fan".into()),
            (
                "Kart".into(),
                status.board.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "Ham PWM".into(),
                status
                    .pwm
                    .map_or_else(|| "—".into(), |pwm| format!("{pwm} / 255")),
            ),
            ("Kart düzeltmesi (50 Hz)".into(), fix.into()),
            ("PWM frekansı".into(), frequency),
            (
                "PWM periyodu".into(),
                status
                    .pwm_period_ns
                    .map_or_else(|| "—".into(), |ns| format!("{ns} ns")),
            ),
            (
                "Dönüş hızı (RPM)".into(),
                if status.rpm_available {
                    status
                        .rpm
                        .map_or_else(|| "Okunamadı".into(), |rpm| format!("{rpm} RPM"))
                } else {
                    "Ölçülemiyor".into()
                },
            ),
            ("Kısma sınırı".into(), "75 °C".into()),
            (
                "Çekirdek eşikleri".into(),
                status.curve.as_ref().map_or_else(
                    || "—".into(),
                    |curve| format!("{} eşik · 1 °C aralıkla", curve.kernel_steps().len()),
                ),
            ),
        ]
    }

    /// Everything the screen draws, composed from the three curves and the
    /// focus.
    pub fn view(&self) -> View {
        let Some(status) = self.status.clone().filter(|_| self.available()) else {
            let message = self
                .status
                .as_ref()
                .and_then(|status| status.error.clone())
                .unwrap_or_else(|| "Fan okunuyor…".into());
            return View {
                message,
                ..View::default()
            };
        };
        let draft = self.draft.clone().expect("available means a draft");
        let active = status.curve.clone();
        let now = status.temperature_c;
        let unsaved = self.unsaved();

        let stale = self.stale();
        let (state, state_tone) = if unsaved {
            ("Kaydedilmedi", "accent")
        } else if status.pending_reboot {
            ("Yeniden başlatma bekliyor", "warn")
        } else if stale {
            ("Kayıtlı", "")
        } else {
            ("Eğri etkin", "good")
        };

        // The graph: x from 0 to GRAPH_MAX_C, y from 0 to full duty, both as
        // fractions of the plot, so the steps, the points and the ticks share
        // one scale whatever shape the plot is.
        let x = |celsius: f64| (celsius / GRAPH_MAX_C).clamp(0.0, 1.0);
        let y = |pwm: u32| f64::from(pwm) / f64::from(FAN_PWM_MAX);
        let last = draft.points.len() - 1;
        let points = draft
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| GraphPoint {
                x: x(f64::from(point.temperature_c)),
                y: y(point.pwm),
                selected: !self.advanced && index == self.row,
                last: index == last,
            })
            .collect();
        // A change, drawn only where it is and only until it is applied:
        // an edit against the saved curve, or a saved curve not yet running
        // against the running one. Where the fan would do the same, and when
        // nothing has been changed, there is one line.
        let before = if unsaved {
            self.base.clone()
        } else if status.pending_reboot {
            active.clone()
        } else {
            None
        };
        let previous = before.map_or_else(Vec::new, |before| {
            draft
                .changed_ranges(&before, GRAPH_MAX_C as u32)
                .into_iter()
                .flat_map(|(from, to)| {
                    pieces(&before.polyline_within(GRAPH_MAX_C, f64::from(from), f64::from(to)))
                })
                .collect()
        });
        let now_duty = status.pwm.map(y);
        let x_ticks = (0..=8)
            .map(|i| {
                let c = f64::from(i * 10);
                Tick {
                    at: x(c),
                    label: format!("{}°", i * 10),
                    shown: now.is_none_or(|now| (c - now).abs() >= TICK_CLEAR_C),
                }
            })
            .collect();
        let y_ticks = (0..=4)
            .map(|i| {
                let at = f64::from(i) * 0.25;
                Tick {
                    at,
                    label: format!("%{}", i * 25),
                    shown: now_duty.is_none_or(|now| (at - now).abs() >= TICK_CLEAR_DUTY),
                }
            })
            .collect();

        let edit = self.edit.as_ref();
        let rows = draft
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let in_table = self.zone == Zone::Table && !self.prompt;
                let focus_col = if in_table && index == self.row {
                    self.col as i32
                } else {
                    -1
                };
                let edit_col = edit.filter(|e| e.row == index).map_or(-1, |e| e.col as i32);
                let typed = |col: usize| {
                    edit.filter(|e| e.row == index && e.col == col && !e.typed.is_empty())
                        .map(|e| e.typed.clone())
                };
                RowView {
                    number: format!("{}", index + 1),
                    temperature: typed(0).map_or_else(
                        || format!("{} °C", point.temperature_c),
                        |t| format!("{t}▏°C"),
                    ),
                    duty: typed(1)
                        .map_or_else(|| format!("%{}", percent(point.pwm)), |t| format!("%{t}▏")),
                    raw: if index == last {
                        "sabit".into()
                    } else if point.pwm == 0 {
                        "kapalı".into()
                    } else {
                        point.pwm.to_string()
                    },
                    locked: index == last,
                    selected: !self.advanced && index == self.row,
                    can_add: self.can_add(index),
                    can_remove: self.can_remove(index),
                    focus_col,
                    edit_col,
                }
            })
            .collect();

        let chips = self
            .chips()
            .iter()
            .enumerate()
            .map(|(i, (profile, on))| Chip {
                label: profile.label().into(),
                enabled: *on,
                on: draft.profile == *profile,
                focused: !self.prompt && self.zone == Zone::Profile && self.idx == i,
            })
            .collect();
        let button = |zone: Zone, i: usize, label: &str, enabled: bool| Chip {
            label: label.into(),
            enabled,
            on: false,
            focused: !self.prompt && self.zone == zone && self.idx == i,
        };
        let actions = self
            .actions()
            .iter()
            .enumerate()
            .map(|(i, (act, on))| {
                let label = match act {
                    Act::Save => "Kaydet",
                    Act::Undo => "Geri al",
                    Act::Reset => "Varsayılana dön",
                    Act::Advanced if self.advanced => "Eğriye dön",
                    Act::Advanced => "Gelişmiş bilgiler",
                };
                button(Zone::Actions, i, label, *on)
            })
            .collect();

        let callout = if self.advanced {
            String::new()
        } else {
            let point = draft.points[self.row];
            format!(
                "#{} · {} °C · %{}",
                self.row + 1,
                point.temperature_c,
                percent(point.pwm)
            )
        };

        let note = if !self.note.is_empty() {
            self.note.clone()
        } else if let Some(edit) = edit {
            if edit.col == 0 {
                "Rakamla yazın ya da ▲▼ 1 °C · OK onayla · Geri vazgeç".into()
            } else {
                "Yüzde yazın ya da ▲▼ 5 PWM · 1–19 % atlanır · OK onayla".into()
            }
        } else if let Some(warning) = status.warning.clone() {
            warning
        } else if self.zone == Zone::Table && self.col == 2 {
            "Bu noktayla sonraki arasına yeni nokta ekler.".into()
        } else if self.zone == Zone::Table && self.col == 3 {
            "Bu noktayı siler.".into()
        } else {
            "Son nokta güvenlik için %100'de sabit.".into()
        };

        let legend = if unsaved {
            "Taslak"
        } else if previous.is_empty() {
            "Eğri"
        } else {
            "Kayıtlı"
        };
        View {
            legend: legend.into(),
            available: true,
            message: String::new(),
            temperature: now.map_or_else(|| "—".into(), |c| format!("{c:.1} °C")),
            duty: status
                .pwm
                .map_or_else(|| "—".into(), |pwm| format!("%{}", percent(pwm))),
            duty_raw: status
                .pwm
                .map_or_else(String::new, |pwm| format!("{pwm}/255")),
            curve: format!("{} nokta", draft.points.len()),
            profile: draft.profile.label().into(),
            state: state.into(),
            state_tone: state_tone.into(),
            chips,
            line: line(&draft),
            previous_line: previous,
            points,
            x_ticks,
            y_ticks,
            now_x: now.map(x),
            now_y: now_duty,
            now_temperature: now.map_or_else(String::new, |c| format!("{c:.1}°")),
            now_duty: status
                .pwm
                .map_or_else(String::new, |pwm| format!("%{}", percent(pwm))),
            trip_x: x(f64::from(FAN_TEMP_MAX_C)),
            callout,
            callout_editing: edit.is_some(),
            rows,
            count: format!("{} / {FAN_CURVE_MAX_POINTS}", draft.points.len()),
            actions,
            note,
            advanced: self.advanced,
            info: self.info(),
            prompt: self.prompt,
            prompt_focus: self.prompt_idx,
        }
    }
}

/// A duty as the whole percent the table and the graph print.
fn percent(pwm: u32) -> u32 {
    fan_pwm_percent(pwm).round() as u32
}

/// The curve as a graph draws it: [`FanCurve::polyline`] as straight pieces,
/// in fractions of the plot.
///
/// Fractions, not path commands: the plot is several times wider than it is
/// tall, and a path in a square view box is fitted into it with its aspect
/// kept -- measured on the panel, the curve came out squeezed into the middle
/// fifth while the points sat where they belong. Pieces are placed by the
/// same two functions as the points, whatever shape the plot is.
pub fn line(curve: &FanCurve) -> Vec<Segment> {
    pieces(&curve.polyline(GRAPH_MAX_C))
}

/// Corners in °C and duty, as straight pieces in fractions of the plot.
fn pieces(corners: &[(f64, f64)]) -> Vec<Segment> {
    let x = |celsius: f64| (celsius / GRAPH_MAX_C).clamp(0.0, 1.0);
    let y = |pwm: f64| pwm / f64::from(FAN_PWM_MAX);
    corners
        .windows(2)
        .map(|pair| Segment {
            x0: x(pair[0].0),
            y0: y(pair[0].1),
            x1: x(pair[1].0),
            y1: y(pair[1].1),
        })
        .collect()
}

/// One straight piece of the curve.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Segment {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphPoint {
    pub x: f64,
    pub y: f64,
    pub selected: bool,
    /// The full-duty point every curve ends on.
    pub last: bool,
}

/// An axis label, and whether it is drawn: one right under the current
/// reading's own label is not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tick {
    pub at: f64,
    pub label: String,
    pub shown: bool,
}

/// A chip or a button.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chip {
    pub label: String,
    pub enabled: bool,
    /// For a preset chip: the draft is this curve.
    pub on: bool,
    pub focused: bool,
}

/// One row of the point table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowView {
    pub number: String,
    pub temperature: String,
    pub duty: String,
    /// The duty the kernel is given, or why there is none to give.
    pub raw: String,
    pub locked: bool,
    pub selected: bool,
    /// Add after this point, remove this point: shown on the selected row.
    pub can_add: bool,
    pub can_remove: bool,
    /// Which cell has the focus, -1 for none: 0 temperature, 1 duty, 2 add,
    /// 3 remove.
    pub focus_col: i32,
    /// Which cell is open, -1 for none.
    pub edit_col: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub available: bool,
    pub message: String,
    pub temperature: String,
    pub duty: String,
    pub duty_raw: String,
    pub curve: String,
    pub profile: String,
    pub state: String,
    /// "good", "accent" or "warn".
    pub state_tone: String,
    pub chips: Vec<Chip>,
    pub line: Vec<Segment>,
    /// The curve before a change that is not applied yet, only where the
    /// change is.
    pub previous_line: Vec<Segment>,
    /// What the draft's line is called in the legend.
    pub legend: String,
    pub points: Vec<GraphPoint>,
    pub x_ticks: Vec<Tick>,
    pub y_ticks: Vec<Tick>,
    pub now_x: Option<f64>,
    pub now_y: Option<f64>,
    pub now_temperature: String,
    pub now_duty: String,
    pub trip_x: f64,
    /// The selected point's label, the one text inside the plot.
    pub callout: String,
    pub callout_editing: bool,
    pub rows: Vec<RowView>,
    pub count: String,
    pub actions: Vec<Chip>,
    pub note: String,
    pub advanced: bool,
    pub info: Vec<(String, String)>,
    pub prompt: bool,
    pub prompt_focus: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediabox_core::FanPoint;

    fn status(active: FanCurve, configured: Option<FanCurve>, pending: bool) -> FanStatus {
        FanStatus {
            available: true,
            board: Some("Orange Pi 5 Plus".into()),
            temperature_c: Some(52.3),
            pwm: Some(80),
            pwm_percent: Some(31.4),
            pwm_enable: Some(1),
            pwm_period_ns: Some(20_000_000),
            pwm_frequency_hz: Some(50.0),
            rpm_available: false,
            rpm: None,
            control_backend: "kernel-pwm-fan".into(),
            curve: Some(active),
            configured,
            pending_reboot: pending,
            board_fix: FanBoardFix::Active,
            boot_config_ready: true,
            warning: None,
            error: None,
        }
    }

    fn editor() -> Cooling {
        let mut cooling = Cooling::new();
        cooling.load(Some(status(FanCurve::balanced(), None, false)));
        cooling.enter();
        cooling
    }

    fn points(cooling: &Cooling) -> Vec<FanPoint> {
        cooling.draft().unwrap().points.clone()
    }

    fn to_zone(cooling: &mut Cooling, zone: Zone) {
        for _ in 0..30 {
            if cooling.focus().0 == zone {
                return;
            }
            cooling.step(0, 1);
        }
        for _ in 0..30 {
            if cooling.focus().0 == zone {
                return;
            }
            cooling.step(0, -1);
        }
        panic!("never reached {zone:?}");
    }

    fn to_row(cooling: &mut Cooling, row: usize) {
        to_zone(cooling, Zone::Table);
        while cooling.selected() > row {
            cooling.step(0, -1);
        }
        while cooling.selected() < row {
            cooling.step(0, 1);
        }
    }

    fn type_in(cooling: &mut Cooling, text: &str) {
        for c in text.chars() {
            assert!(cooling.typed(c), "{c} not taken");
        }
    }

    /// Walking the whole editor, every cell and button, changes nothing.
    #[test]
    fn navigation_alone_never_changes_a_value() {
        let mut cooling = editor();
        let before = cooling.draft().cloned();
        for _ in 0..20 {
            for _ in 0..5 {
                cooling.step(1, 0);
            }
            cooling.step(0, 1);
        }
        for _ in 0..20 {
            cooling.step(0, -1);
        }
        assert_eq!(cooling.draft().cloned(), before);
        assert!(!cooling.unsaved());
    }

    #[test]
    fn it_opens_on_the_selected_point_and_left_leaves() {
        let mut cooling = editor();
        assert_eq!(cooling.focus(), (Zone::Table, 0, 0));
        assert_eq!(cooling.step(1, 0), Nav::Moved);
        assert_eq!(cooling.step(-1, 0), Nav::Moved);
        assert_eq!(cooling.step(-1, 0), Nav::Leave);
        to_zone(&mut cooling, Zone::Actions);
        assert_eq!(cooling.step(-1, 0), Nav::Leave);
    }

    /// Ok opens a cell, the arrows step it, Ok keeps it and Back puts it back.
    #[test]
    fn a_cell_is_opened_stepped_kept_or_put_back() {
        let mut cooling = editor();
        assert_eq!(cooling.press(), Press::Changed);
        assert!(cooling.editing());
        assert_eq!(cooling.step(0, 1), Nav::Moved);
        assert_eq!(points(&cooling)[0], FanPoint::new(49, 50));
        // Sideways does nothing while a cell is open.
        assert_eq!(cooling.step(1, 0), Nav::Unchanged);
        assert_eq!(cooling.press(), Press::Changed);
        assert!(!cooling.editing());
        assert_eq!(cooling.draft().unwrap().profile, FanProfile::Custom);
        assert!(cooling.unsaved());

        // The duty cell, stepped up, then Back.
        cooling.step(1, 0);
        cooling.press();
        cooling.step(0, -1);
        assert_eq!(points(&cooling)[0], FanPoint::new(49, 55));
        assert!(cooling.back());
        assert_eq!(points(&cooling)[0], FanPoint::new(49, 50));
        assert!(!cooling.editing());
    }

    /// From a keyboard: digits open the focused cell, Ok sets them.
    #[test]
    fn digits_are_typed_into_the_focused_cell() {
        let mut cooling = editor();
        to_row(&mut cooling, 1);
        type_in(&mut cooling, "57");
        assert!(cooling.editing());
        assert!(cooling.view().rows[1].temperature.starts_with("57"));
        // Not set until Ok.
        assert_eq!(points(&cooling)[1].temperature_c, 55);
        cooling.press();
        assert_eq!(points(&cooling)[1].temperature_c, 57);
        assert!(!cooling.editing());

        // The duty cell takes a percent: 55 % is 140.
        cooling.step(1, 0);
        type_in(&mut cooling, "55");
        cooling.press();
        assert_eq!(points(&cooling)[1].pwm, 140);
        assert_eq!(cooling.view().rows[1].duty, "%55");
        assert_eq!(cooling.draft().unwrap().validate(), Ok(()));
    }

    /// A typed value that breaks a rule is refused where it is typed, with
    /// the range, and the cell stays open for another try.
    #[test]
    fn a_typed_value_out_of_range_is_refused_with_the_reason() {
        let mut cooling = editor();
        to_row(&mut cooling, 1);
        type_in(&mut cooling, "62");
        cooling.press();
        assert!(cooling.editing());
        assert_eq!(points(&cooling)[1].temperature_c, 55);
        let note = cooling.view().note;
        assert!(note.contains("51–59 °C"), "{note}");

        // The last point cannot go past 75 °C.
        cooling.back();
        to_row(&mut cooling, 4);
        type_in(&mut cooling, "80");
        cooling.press();
        assert!(cooling.view().note.contains("kısma"));
        assert_eq!(points(&cooling)[4].temperature_c, 70);
    }

    /// 1–19 % is the gap a fan can stall in: refused; 0 is off and allowed.
    #[test]
    fn a_typed_duty_in_the_stall_gap_is_refused_and_zero_is_off() {
        let mut cooling = editor();
        cooling.step(1, 0);
        type_in(&mut cooling, "15");
        cooling.press();
        assert!(cooling.view().note.contains("%1–19"));
        assert_eq!(points(&cooling)[0].pwm, 50);
        type_in(&mut cooling, "0");
        cooling.press();
        assert_eq!(points(&cooling)[0].pwm, 0);
        assert_eq!(cooling.view().rows[0].raw, "kapalı");
        // More than 100 is refused too, not wrapped.
        type_in(&mut cooling, "250");
        cooling.press();
        assert!(cooling.editing());
        assert_eq!(points(&cooling)[0].pwm, 0);
    }

    #[test]
    fn backspace_takes_a_digit_then_the_edit() {
        let mut cooling = editor();
        type_in(&mut cooling, "45");
        assert!(cooling.backspace());
        assert!(cooling.view().rows[0].temperature.starts_with("4▏"));
        assert!(cooling.backspace());
        assert!(cooling.editing());
        assert!(cooling.backspace());
        assert!(!cooling.editing());
        assert!(!cooling.unsaved());
        // With nothing open it is Back's.
        assert!(!cooling.backspace());
    }

    /// Letters and marks are not this screen's; nor are digits off the table.
    #[test]
    fn only_digits_on_the_table_are_taken() {
        let mut cooling = editor();
        assert!(!cooling.typed('a'));
        assert!(!cooling.typed('-'));
        cooling.step(1, 0);
        cooling.step(1, 0);
        assert_eq!(cooling.focus(), (Zone::Table, 0, 2));
        assert!(!cooling.typed('5'));
        to_zone(&mut cooling, Zone::Actions);
        assert!(!cooling.typed('5'));
        assert!(!cooling.unsaved());
    }

    /// The last point is full duty: its duty cell cannot be focused or typed.
    #[test]
    fn the_last_point_s_duty_is_locked() {
        let mut cooling = editor();
        to_row(&mut cooling, 4);
        assert_eq!(cooling.focus(), (Zone::Table, 4, 0));
        assert!(!cooling.typed('x'));
        // From the duty column of the row above, down lands on temperature.
        cooling.step(0, -1);
        cooling.step(1, 0);
        cooling.step(0, 1);
        assert_eq!(cooling.focus(), (Zone::Table, 4, 0));
        let view = cooling.view();
        assert!(view.rows[4].locked);
        assert_eq!(view.rows[4].raw, "sabit");
        assert_eq!(view.rows[4].duty, "%100");
    }

    #[test]
    fn a_preset_loads_and_an_edit_makes_it_custom() {
        let mut cooling = editor();
        to_zone(&mut cooling, Zone::Profile);
        // Focus lands on the chip of the curve in the draft.
        assert_eq!(cooling.focus(), (Zone::Profile, 0, 0));
        cooling.step(1, 0);
        assert_eq!(cooling.press(), Press::Changed);
        assert_eq!(cooling.draft().cloned(), FanCurve::preset(FanProfile::Cool));
        // The custom chip is a label, not a button.
        cooling.step(1, 0);
        assert_eq!(cooling.focus(), (Zone::Profile, 1, 0));
        to_row(&mut cooling, 1);
        type_in(&mut cooling, "46");
        cooling.press();
        assert_eq!(cooling.draft().unwrap().profile, FanProfile::Custom);
        assert!(cooling.view().chips[2].on);
    }

    /// Down to 0 °C, typed.
    #[test]
    fn a_curve_can_be_started_at_zero_degrees() {
        let mut cooling = editor();
        type_in(&mut cooling, "0");
        cooling.press();
        assert_eq!(points(&cooling)[0].temperature_c, 0);
        assert_eq!(cooling.draft().unwrap().validate(), Ok(()));
    }

    /// Add and remove are on the row: after any point, not only the last.
    #[test]
    fn points_are_added_after_the_selected_one_and_removed() {
        let mut cooling = editor();
        to_row(&mut cooling, 1);
        cooling.step(1, 0);
        cooling.step(1, 0);
        assert_eq!(cooling.focus(), (Zone::Table, 1, 2));
        assert_eq!(cooling.press(), Press::Changed);
        // Between 55 and 60 °C, and the focus is on its temperature.
        assert_eq!(points(&cooling).len(), 6);
        assert_eq!(points(&cooling)[2].temperature_c, 57);
        assert_eq!(cooling.focus(), (Zone::Table, 2, 0));
        type_in(&mut cooling, "58");
        cooling.press();
        assert_eq!(points(&cooling)[2].temperature_c, 58);

        // Remove it again from its own row.
        for _ in 0..3 {
            cooling.step(1, 0);
        }
        assert_eq!(cooling.focus(), (Zone::Table, 2, 3));
        assert_eq!(cooling.press(), Press::Changed);
        assert_eq!(points(&cooling).len(), 5);
        assert_eq!(cooling.draft().unwrap().validate(), Ok(()));
    }

    /// The full-duty point cannot be removed: every curve ends on it. From
    /// its row, add puts a point before it.
    #[test]
    fn the_full_duty_point_cannot_be_removed() {
        let mut cooling = editor();
        to_row(&mut cooling, 4);
        let view = cooling.view();
        assert!(!view.rows[4].can_remove);
        assert!(view.rows[4].can_add);
        // Right from the temperature skips the locked duty onto add.
        assert_eq!(cooling.step(1, 0), Nav::Moved);
        assert_eq!(cooling.focus(), (Zone::Table, 4, 2));
        assert_eq!(cooling.step(1, 0), Nav::Unchanged);
        cooling.press();
        assert_eq!(points(&cooling)[4].temperature_c, 67);
        assert_eq!(points(&cooling)[5], FanPoint::new(70, 255));
    }

    #[test]
    fn undo_puts_the_saved_curve_back() {
        let mut cooling = editor();
        type_in(&mut cooling, "40");
        cooling.press();
        assert!(cooling.unsaved());
        to_zone(&mut cooling, Zone::Actions);
        cooling.step(1, 0);
        assert_eq!(cooling.press(), Press::Changed);
        assert!(!cooling.unsaved());
        assert_eq!(cooling.draft().cloned(), Some(FanCurve::balanced()));
    }

    /// Save is offered only for a change; after it, the restart question
    /// comes up with "later" under the focus.
    #[test]
    fn saving_hands_over_the_draft_and_then_asks_about_restarting() {
        let mut cooling = editor();
        to_zone(&mut cooling, Zone::Actions);
        assert!(!cooling.view().actions[0].enabled);
        to_row(&mut cooling, 0);
        type_in(&mut cooling, "45");
        cooling.press();
        let draft = cooling.draft().cloned().unwrap();
        to_zone(&mut cooling, Zone::Actions);
        assert_eq!(cooling.focus(), (Zone::Actions, 0, 0));
        assert_eq!(cooling.press(), Press::Save(draft.clone()));
        cooling.saved(draft.clone(), true);
        assert!(!cooling.unsaved());
        let view = cooling.view();
        assert!(view.prompt);
        assert_eq!(view.prompt_focus, 1);
        // The question holds the focus; digits are not taken under it.
        assert!(!cooling.typed('5'));
        assert_eq!(cooling.step(0, 1), Nav::Unchanged);
        cooling.step(-1, 0);
        assert_eq!(cooling.press(), Press::Restart);
        assert!(!cooling.view().prompt);

        // Later: the question goes, nothing restarts.
        cooling.saved(draft, true);
        assert_eq!(cooling.press(), Press::Changed);
        assert!(!cooling.view().prompt);
        assert!(cooling.view().note.contains("sonraki açılış"));
    }

    /// Active, saved and draft are three things, and the screen says which.
    #[test]
    fn active_pending_and_draft_are_told_apart() {
        let cool = FanCurve::preset(FanProfile::Cool).unwrap();
        let mut cooling = Cooling::new();
        cooling.load(Some(status(FanCurve::balanced(), Some(cool.clone()), true)));
        // The draft starts from what is saved, not what is running.
        assert_eq!(cooling.draft().cloned(), Some(cool));
        let view = cooling.view();
        assert_eq!(view.state, "Yeniden başlatma bekliyor");
        assert_eq!(view.state_tone, "warn");
        assert_eq!(view.profile, "Serin");
        // Nothing unsaved, but the saved curve is not applied until the
        // restart: where it differs from the running one, that is drawn.
        assert!(!view.previous_line.is_empty());
        assert_eq!(view.legend, "Kayıtlı");

        cooling.enter();
        type_in(&mut cooling, "38");
        cooling.press();
        let view = cooling.view();
        assert_eq!(view.state, "Kaydedilmedi");
        assert_eq!(view.state_tone, "accent");
        assert!(!view.previous_line.is_empty());
    }

    /// With nothing changed there is one line, whatever the running tree
    /// holds: a saved curve that did not load is the daemon's warning under
    /// the table, not a second curve, and the badge does not call it running.
    #[test]
    fn with_nothing_changed_there_is_one_line() {
        let cool = FanCurve::preset(FanProfile::Cool).unwrap();
        let mut cooling = Cooling::new();
        let mut not_loaded = status(FanCurve::board(), Some(cool.clone()), false);
        not_loaded.warning = Some("kaydedilen fan eğrisi bu açılışta etkin değil".into());
        cooling.load(Some(not_loaded));
        cooling.enter();
        let view = cooling.view();
        assert!(view.previous_line.is_empty());
        assert_eq!(view.legend, "Eğri");
        assert_ne!(view.state, "Eğri etkin");
        assert!(view.note.contains("etkin değil"));
        assert!(!view.actions[0].enabled);

        let mut cooling = Cooling::new();
        cooling.load(Some(status(cool.clone(), Some(cool), false)));
        let view = cooling.view();
        assert_eq!(view.state, "Eğri etkin");
        assert!(view.previous_line.is_empty());
    }

    /// An edit is drawn against the saved curve only where it changes it.
    #[test]
    fn an_edit_is_drawn_only_where_it_changes_the_curve() {
        let mut cooling = editor();
        to_row(&mut cooling, 1);
        cooling.step(1, 0);
        // Balanced's second point, 100 → 105.
        cooling.press();
        cooling.step(0, -1);
        cooling.press();
        assert_eq!(points(&cooling)[1], FanPoint::new(55, 105));
        let view = cooling.view();
        assert!(!view.previous_line.is_empty());
        // Only around the moved point, never out to the edges.
        let from = view.previous_line.first().unwrap().x0 * 80.0;
        let to = view.previous_line.last().unwrap().x1 * 80.0;
        assert!(
            (50.0..55.0).contains(&from) && (55.0..=60.0).contains(&to),
            "{from}..{to}"
        );
        // Put back: one line again.
        cooling.press();
        cooling.step(0, 1);
        cooling.press();
        assert_eq!(points(&cooling)[1], FanPoint::new(55, 100));
        assert!(cooling.view().previous_line.is_empty());
    }

    /// Back to the board's own curve: its steps, under its own name.
    #[test]
    fn the_board_s_own_curve_comes_back_after_a_reset() {
        let mut cooling = editor();
        cooling.reset_done(true);
        assert_eq!(cooling.draft().cloned(), Some(FanCurve::board()));
        let view = cooling.view();
        assert_eq!(view.profile, "Kartın eğrisi");
        assert!(view.chips.iter().all(|chip| !chip.on));
    }

    /// A poll does not undo an edit.
    #[test]
    fn a_poll_does_not_move_the_draft() {
        let mut cooling = editor();
        type_in(&mut cooling, "44");
        cooling.press();
        let edited = cooling.draft().cloned();
        cooling.load(Some(status(FanCurve::balanced(), None, false)));
        assert_eq!(cooling.draft().cloned(), edited);
    }

    /// The graph is the points joined by lines; the line, the points and the
    /// ticks share one scale; the selected point is the table's row.
    #[test]
    fn the_graph_joins_the_points_on_one_scale() {
        let mut cooling = editor();
        to_row(&mut cooling, 1);
        let view = cooling.view();
        let line = &view.line;
        // Off to 50 °C, up to the first point, four lines, full to the edge.
        assert_eq!(line.len(), 7);
        assert_eq!(
            (line[0].x0, line[0].y0, line[0].x1, line[0].y1),
            (0.0, 0.0, 50.0 / 80.0, 0.0)
        );
        assert_eq!(line[1].x0, line[1].x1);
        assert_eq!(line[6].x1, 1.0);
        assert_eq!(line[6].y1, 1.0);
        // Every point is a corner of the line, at the same place.
        for (point, piece) in view.points.iter().zip(&line[1..]) {
            assert_eq!((point.x, point.y), (piece.x1, piece.y1));
        }
        // And the pieces join.
        for pair in line.windows(2) {
            assert_eq!((pair[0].x1, pair[0].y1), (pair[1].x0, pair[1].y0));
        }
        assert!(view.points[1].selected);
        assert!(view.points[4].last);
        assert_eq!(view.callout, "#2 · 55 °C · %39");
        assert_eq!(view.now_x, Some(52.3 / 80.0));
        assert_eq!(view.now_y, Some(80.0 / 255.0));
        assert_eq!(view.now_temperature, "52.3°");
        assert_eq!(view.now_duty, "%31");
        let tick = |ticks: &[Tick], at: f64| ticks.iter().find(|t| t.at == at).unwrap().shown;
        assert!(view.x_ticks[0].at == 0.0 && view.x_ticks[8].at == 1.0);
        assert!(tick(&view.x_ticks, 20.0 / 80.0));
    }

    /// The labels on the axes do not sit on each other: a tick under the
    /// current temperature's or duty's own label is left out.
    #[test]
    fn ticks_under_the_current_readings_are_left_out() {
        let view = editor().view();
        // 52.3 °C: 50° is within 7° of it, 40° and 60° are not.
        let shown: Vec<&str> = view
            .x_ticks
            .iter()
            .filter(|t| t.shown)
            .map(|t| t.label.as_str())
            .collect();
        assert_eq!(
            shown,
            ["0°", "10°", "20°", "30°", "40°", "60°", "70°", "80°"]
        );
        // 80/255 is 31 %: the 25 % tick is under it.
        let shown: Vec<&str> = view
            .y_ticks
            .iter()
            .filter(|t| t.shown)
            .map(|t| t.label.as_str())
            .collect();
        assert_eq!(shown, ["%0", "%50", "%75", "%100"]);
    }

    /// Ten points: the table holds them all and add is no longer offered.
    #[test]
    fn ten_points_fill_the_table() {
        let mut cooling = Cooling::new();
        let points: Vec<FanPoint> = (0..10)
            .map(|i| FanPoint::new(i * 8, if i == 9 { 255 } else { 50 + i * 20 }))
            .collect();
        let ten = FanCurve {
            profile: FanProfile::Custom,
            points,
        };
        cooling.load(Some(status(ten, None, false)));
        cooling.enter();
        let view = cooling.view();
        assert_eq!(view.rows.len(), 10);
        assert_eq!(view.count, "10 / 10");
        assert!(view.rows.iter().all(|row| !row.can_add));
        to_row(&mut cooling, 9);
        assert_eq!(cooling.selected(), 9);
    }

    /// The technical readings are behind a switch, and PWM is never called a
    /// speed anywhere on the screen.
    #[test]
    fn technical_readings_are_behind_a_switch_and_no_duty_is_a_speed() {
        let mut cooling = editor();
        assert!(!cooling.view().advanced);
        to_zone(&mut cooling, Zone::Actions);
        cooling.step(1, 0);
        cooling.step(1, 0);
        cooling.step(1, 0);
        assert_eq!(cooling.press(), Press::Changed);
        let view = cooling.view();
        assert!(view.advanced);
        assert_eq!(view.actions[3].label, "Eğriye dön");
        // No table while the readings are up.
        assert_eq!(cooling.step(0, -1), Nav::Moved);
        assert_eq!(cooling.focus().0, Zone::Profile);
        let info = cooling.info();
        assert!(
            info.iter()
                .any(|(label, value)| label == "PWM frekansı" && value == "50 Hz")
        );
        assert!(
            info.iter()
                .any(|(label, value)| label == "Dönüş hızı (RPM)" && value == "Ölçülemiyor")
        );
        assert!(
            info.iter()
                .any(|(label, value)| label.contains("50 Hz") && value == "Etkin")
        );
        let view = cooling.view();
        for text in [&view.duty, &view.note, &view.callout]
            .into_iter()
            .chain(view.info.iter().flat_map(|(a, b)| [a, b]))
        {
            assert!(!text.contains("Fan hızı"), "{text}");
        }
    }

    #[test]
    fn a_board_with_no_fan_shows_why_and_offers_nothing() {
        let mut cooling = Cooling::new();
        cooling.load(Some(FanStatus {
            available: false,
            error: Some("bu kartta pwm-fan denetimli bir fan bulunamadı".into()),
            ..FanStatus::default()
        }));
        assert!(!cooling.available());
        assert!(!cooling.typed('5'));
        let view = cooling.view();
        assert!(!view.available);
        assert!(view.message.contains("pwm-fan"));
    }
}
