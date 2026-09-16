//! Searching from the sofa.
//!
//! The remote is the primary input and it has five keys, so the letters are on
//! the panel: a grid on the left, results on the right, and one deterministic
//! rule for crossing between them. A USB keyboard types into the same box —
//! `text` below — and does not change where the focus is, so a person who
//! starts typing and then reaches for the remote finds it where they left it.

use crate::state::Item;

/// How many posters fit across the results pane at the design width. Fixed
/// rather than measured: the focus model is a grid and a grid that reflowed
/// would move a title out from under the remote when a row wrapped.
pub const COLUMNS: usize = 5;

/// What a key on the grid does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Letter(char),
    Space,
    Backspace,
    Clear,
}

impl Cap {
    pub fn label(self) -> String {
        match self {
            Cap::Letter(c) => upper(c),
            Cap::Space => "Boşluk".into(),
            Cap::Backspace => "Sil".into(),
            Cap::Clear => "Temizle".into(),
        }
    }

    /// Wide keys are drawn wide. The grid's columns are uniform, so a key that
    /// needs two of them says so here rather than in the interface.
    pub fn span(self) -> u32 {
        match self {
            Cap::Letter(_) => 1,
            Cap::Space => 3,
            Cap::Backspace => 2,
            Cap::Clear => 2,
        }
    }
}

/// Turkish uppercase, which is not Unicode's default.
///
/// The dotted and dotless i are different letters here, and `char::to_uppercase`
/// maps both to "I". On the letter grid that produced two keys that looked
/// identical and did different things — visible in the first snapshot of the
/// search screen, and unusable.
fn upper(c: char) -> String {
    match c {
        'i' => "İ".into(),
        'ı' => "I".into(),
        other => other.to_uppercase().to_string(),
    }
}

/// The alphabet in its own order, then the digits, then the three wide keys.
///
/// Seven columns on every row, including the last: the grid is laid out at a
/// fixed column width, and a row of eight put a key off the edge of the panel.
/// Turkish order rather than the English alphabet with the extra letters
/// appended, because this is a Turkish product and a person looking for Ç
/// expects it after C.
fn layout() -> Vec<Vec<Cap>> {
    let row = |s: &str| -> Vec<Cap> { s.chars().map(Cap::Letter).collect() };
    vec![
        row("abcçdef"),
        row("gğhıijk"),
        row("lmnoöpr"),
        row("sştuüvy"),
        row("zwxq012"),
        row("3456789"),
        vec![Cap::Space, Cap::Backspace, Cap::Clear],
    ]
}

/// How many columns the grid is laid out at. Every row is this wide.
pub const KEY_COLUMNS: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Keys,
    Results,
}

pub struct Search {
    pub query: String,
    pub pane: Pane,
    pub key_row: usize,
    pub key_col: usize,
    /// Which result the remote is on, as an index into `results`.
    pub index: usize,
    pub results: Vec<Item>,
    pub note: String,
    pub searching: bool,
    /// Bumped on every query change, so an answer for a query the viewer has
    /// already typed past is dropped rather than drawn.
    pub generation: u64,
    keys: Vec<Vec<Cap>>,
}

impl Search {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            pane: Pane::Keys,
            key_row: 0,
            key_col: 0,
            index: 0,
            results: Vec::new(),
            note: "Aramak için bir şeyler yazın".into(),
            searching: false,
            generation: 0,
            keys: layout(),
        }
    }

    pub fn keys(&self) -> &[Vec<Cap>] {
        &self.keys
    }

    pub fn focused_cap(&self) -> Option<Cap> {
        self.keys.get(self.key_row)?.get(self.key_col).copied()
    }

    pub fn focused_result(&self) -> Option<&Item> {
        self.results.get(self.index)
    }

    pub fn rows_of_results(&self) -> usize {
        self.results.len().div_ceil(COLUMNS)
    }

    /// Moves the remote. Returns whether anything changed.
    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        match self.pane {
            Pane::Keys => self.step_keys(dx, dy),
            Pane::Results => self.step_results(dx, dy),
        }
    }

    fn step_keys(&mut self, dx: i32, dy: i32) -> bool {
        // Right off the edge of the grid is how the results are reached. It is
        // the only way in, and Left from the first column is the only way back,
        // so crossing is never ambiguous.
        if dx > 0 {
            let width = self
                .keys
                .get(self.key_row)
                .map(|row| row.len())
                .unwrap_or(0);
            if self.key_col + 1 >= width {
                if self.results.is_empty() {
                    return false;
                }
                self.pane = Pane::Results;
                return true;
            }
            self.key_col += 1;
            return true;
        }
        if dx < 0 {
            if self.key_col == 0 {
                return false;
            }
            self.key_col -= 1;
            return true;
        }
        if dy != 0 {
            let next = self.key_row as i32 + dy;
            if next < 0 {
                return false;
            }
            // Down off the bottom of the grid is the second door to the
            // results. Reaching them by walking seven columns right is correct
            // and nobody does it.
            if next as usize >= self.keys.len() {
                if self.results.is_empty() {
                    return false;
                }
                self.pane = Pane::Results;
                return true;
            }
            self.key_row = next as usize;
            let width = self.keys[self.key_row].len();
            self.key_col = self.key_col.min(width.saturating_sub(1));
            return true;
        }
        false
    }

    fn step_results(&mut self, dx: i32, dy: i32) -> bool {
        if self.results.is_empty() {
            self.pane = Pane::Keys;
            return true;
        }
        let column = self.index % COLUMNS;

        if dx < 0 && column == 0 {
            self.pane = Pane::Keys;
            return true;
        }
        // Up from the first row goes back to the letters, which is the door
        // Down came through.
        if dy < 0 && self.index < COLUMNS {
            self.pane = Pane::Keys;
            self.key_row = self.keys.len() - 1;
            self.key_col = self.key_col.min(self.keys[self.key_row].len() - 1);
            return true;
        }
        let delta = dx + dy * COLUMNS as i32;
        if delta == 0 {
            return false;
        }
        // Left and right stay on their row; up and down move by a row. A grid
        // that wrapped at the edges would mean Right on the last column jumps
        // to the far side of the screen, which nobody expects.
        let next = if dy == 0 {
            let row_start = self.index - column;
            let width = COLUMNS.min(self.results.len() - row_start);
            let next_column = (column as i32 + dx).clamp(0, width as i32 - 1) as usize;
            row_start + next_column
        } else {
            let candidate = self.index as i32 + delta;
            if candidate < 0 || candidate as usize >= self.results.len() {
                return false;
            }
            candidate as usize
        };
        if next == self.index {
            return false;
        }
        self.index = next;
        true
    }

    /// Pressing Ok on the letter grid. Returns true when the query changed and
    /// a fresh search is owed.
    pub fn press(&mut self) -> bool {
        let Some(cap) = self.focused_cap() else {
            return false;
        };
        match cap {
            Cap::Letter(c) => self.push(c),
            Cap::Space => self.push(' '),
            Cap::Backspace => {
                if self.query.pop().is_none() {
                    return false;
                }
            }
            Cap::Clear => {
                if self.query.is_empty() {
                    return false;
                }
                self.query.clear();
            }
        }
        self.after_edit();
        true
    }

    /// A character from a real keyboard.
    pub fn typed(&mut self, c: char) -> bool {
        if c == '\u{8}' {
            if self.query.pop().is_none() {
                return false;
            }
        } else if c.is_control() {
            return false;
        } else {
            self.push(c);
        }
        self.after_edit();
        true
    }

    fn push(&mut self, c: char) {
        // A query longer than this is not a search, it is a paste. The cap also
        // bounds what goes over the socket to the media core.
        if self.query.chars().count() >= 64 {
            return;
        }
        self.query.push(c);
    }

    fn after_edit(&mut self) {
        self.generation += 1;
        self.index = 0;
        self.pane = Pane::Keys;
        if self.query.trim().is_empty() {
            self.results.clear();
            self.searching = false;
            self.note = "Aramak için bir şeyler yazın".into();
        } else {
            self.searching = true;
            self.note = "Aranıyor…".into();
        }
    }

    /// An answer, if it is still the answer to what is in the box.
    pub fn take(&mut self, generation: u64, results: Vec<Item>) {
        if generation != self.generation {
            return;
        }
        self.searching = false;
        self.note = match results.len() {
            0 => format!("“{}” için sonuç yok", self.query),
            n => format!("{n} sonuç"),
        };
        self.results = results;
        self.index = 0;
    }

    pub fn fail(&mut self, generation: u64, why: &str) {
        if generation != self.generation {
            return;
        }
        self.searching = false;
        self.results.clear();
        self.note = why.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_results(n: usize) -> Search {
        let mut search = Search::new();
        search.results = (0..n)
            .map(|i| crate::state::Item::stub(&format!("t{i}")))
            .collect();
        search
    }

    /// The grid is drawn at a fixed column width, so a row that is wider than
    /// the grid puts a key off the edge of the television.
    #[test]
    fn every_row_fits_the_grid() {
        let search = Search::new();
        for row in search.keys() {
            let span: u32 = row.iter().map(|cap| cap.span()).sum();
            assert_eq!(
                span as usize,
                KEY_COLUMNS,
                "{:?}",
                row.iter().map(|c| c.label()).collect::<Vec<_>>()
            );
        }
    }

    /// Two keys that look the same and do different things are two keys nobody
    /// can use.
    #[test]
    fn no_two_keys_carry_the_same_letter() {
        let search = Search::new();
        let mut seen = std::collections::HashSet::new();
        for row in search.keys() {
            for cap in row {
                assert!(seen.insert(cap.label()), "{} appears twice", cap.label());
            }
        }
    }

    #[test]
    fn the_turkish_i_is_two_letters() {
        assert_eq!(upper('i'), "İ");
        assert_eq!(upper('ı'), "I");
    }

    #[test]
    fn focus_starts_on_the_grid_and_is_never_lost() {
        let search = Search::new();
        assert_eq!(search.pane, Pane::Keys);
        assert!(search.focused_cap().is_some());
    }

    #[test]
    fn results_are_reached_from_the_right_edge_and_left_comes_back() {
        let mut search = with_results(12);
        for _ in 0..10 {
            search.step(1, 0);
        }
        assert_eq!(search.pane, Pane::Results);
        // Left, from the first column, and only from there.
        search.step(1, 0);
        assert_eq!(search.pane, Pane::Results);
        search.step(-1, 0);
        assert_eq!(search.pane, Pane::Results);
        search.index = 0;
        search.step(-1, 0);
        assert_eq!(search.pane, Pane::Keys);
    }

    /// Reaching the results by walking seven columns right is correct and
    /// nobody does it.
    #[test]
    fn down_off_the_bottom_of_the_letters_reaches_the_results() {
        let mut search = with_results(12);
        for _ in 0..12 {
            search.step(0, 1);
        }
        assert_eq!(search.pane, Pane::Results);
        // And up from the first row of results comes back.
        search.index = 2;
        assert!(search.step(0, -1));
        assert_eq!(search.pane, Pane::Keys);
        assert!(search.focused_cap().is_some());
    }

    #[test]
    fn an_empty_result_set_keeps_the_remote_on_the_letters() {
        let mut search = Search::new();
        for _ in 0..10 {
            search.step(1, 0);
        }
        assert_eq!(search.pane, Pane::Keys);
    }

    #[test]
    fn the_grid_never_steps_off_its_own_edges() {
        let mut search = Search::new();
        for _ in 0..20 {
            search.step(0, -1);
            search.step(-1, 0);
        }
        assert_eq!((search.key_row, search.key_col), (0, 0));
        for _ in 0..40 {
            search.step(0, 1);
        }
        assert!(search.key_row < search.keys.len());
        assert!(search.focused_cap().is_some());
    }

    #[test]
    fn a_row_move_stays_inside_the_results() {
        let mut search = with_results(7);
        search.pane = Pane::Results;
        search.index = 6;
        assert!(!search.step(0, 1));
        assert_eq!(search.index, 6);
        assert!(search.step(0, -1));
        assert_eq!(search.index, 1);
    }

    #[test]
    fn typing_supersedes_an_answer_already_in_flight() {
        let mut search = Search::new();
        search.typed('a');
        let stale = search.generation;
        search.typed('b');
        search.take(stale, vec![crate::state::Item::stub("old")]);
        assert!(search.results.is_empty());
        search.take(search.generation, vec![crate::state::Item::stub("new")]);
        assert_eq!(search.results.len(), 1);
    }

    #[test]
    fn backspace_on_an_empty_query_changes_nothing() {
        let mut search = Search::new();
        assert!(!search.typed('\u{8}'));
        assert_eq!(search.generation, 0);
    }
}
