//! Searching from the sofa.
//!
//! The remote is the primary input and it has five keys, so the letters are on
//! the panel: a grid on the left, and on the right whatever the box has to
//! offer for what has been typed. A USB keyboard types into the same box and
//! does not change where the focus is, so a person who starts typing and then
//! reaches for the remote finds it where they left it.
//!
//! The rules are the reference's, read out of stremio-web and stremio-core
//! rather than guessed at:
//!
//! - typing asks no addon anything. It suggests, from the media core's local
//!   index, five titles at most, a quarter of a second after the last key;
//! - the search itself is made once, with Ara (or Enter on a keyboard, or by
//!   choosing a suggestion or an earlier search), and every searchable
//!   catalogue is asked at the same time;
//! - the answer is a row per catalogue, in the order the addons are installed;
//!   a catalogue that failed keeps its row and says so;
//! - every search made is remembered, newest first, and the last eight are
//!   offered while the box is empty.
//!
//! The version this replaced searched every addon a third of a second after
//! each letter. A remote types slower than that, so a four-letter word was
//! four full searches, each one waiting for the slowest catalogue.

use crate::keyboard::Cap;
use crate::model::Suggestion;
use crate::screens::shelves::Shelves;
use crate::state::{Item, Shelf};

/// stremio-web's search bar shows this many earlier searches.
pub const HISTORY_SHOWN: usize = 8;
/// And this many are kept, so the eight shown are the eight most recent.
const HISTORY_KEPT: usize = 50;

/// The same letters every other text field has, with Ara where Shift would
/// be; see `keyboard::layout_without_shift`.
fn layout() -> Vec<Vec<Cap>> {
    crate::keyboard::layout_without_shift()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The letters, in the panel under the search box while it is open.
    Keys,
    /// The suggestions and the earlier searches, beside the letters.
    List,
    /// The rows of results, as the board's shelves.
    Results,
    /// The search box itself, with its panel closed.
    Box,
    /// The places down the left.
    Places,
}

/// One line of the list beside the letters.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub text: String,
    /// "Film · 2021" for a suggestion; empty for an earlier search.
    pub detail: String,
    pub earlier: bool,
}

/// What pressing Ok did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Nothing,
    /// The query changed; suggestions are owed.
    Edited,
    /// A search is to be made now, for `query`.
    Search,
}

pub struct Search {
    pub query: String,
    pub pane: Pane,
    pub key_row: usize,
    pub key_col: usize,
    keys: Vec<Vec<Cap>>,

    /// The query the results are the answer to.
    pub searched: Option<String>,
    pub results: Shelves,
    pub searching: bool,
    pub note: String,
    /// Bumped on every search made, so an answer to one the viewer has since
    /// replaced is dropped rather than drawn.
    pub generation: u64,

    pub suggestions: Vec<Suggestion>,
    /// The query the suggestions are for.
    suggested_for: String,
    /// Bumped on every edit, for the same reason.
    pub suggest_generation: u64,

    /// Newest first.
    pub history: Vec<String>,
    /// Where the remote is in the list.
    pub list_index: usize,

    /// True while text is arriving from a real keyboard. Its Enter is the
    /// same key as the remote's Ok, and on a keyboard it means "search"; the
    /// first move of the remote hands Ok back to the grid.
    pub typing: bool,
    /// Whether the panel under the search box -- the letters, the suggestions
    /// and the earlier searches -- is open. The reference's search box drops
    /// its suggestions down under itself while it is typed into; on a
    /// television the letters go there too.
    pub editing: bool,
    /// The place the remote is on, down the left.
    pub place: usize,
}

impl Search {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            pane: Pane::Keys,
            key_row: 0,
            key_col: 0,
            keys: layout(),
            searched: None,
            results: Shelves::new(),
            searching: false,
            note: String::new(),
            generation: 0,
            suggestions: Vec::new(),
            suggested_for: String::new(),
            suggest_generation: 0,
            history: Vec::new(),
            list_index: 0,
            typing: false,
            editing: true,
            place: 0,
        }
    }

    /// Opens the panel under the search box, on the letters.
    pub fn open_editing(&mut self) {
        self.editing = true;
        self.pane = Pane::Keys;
    }

    /// Closes it, onto the results if there are any and the box otherwise.
    pub fn close_editing(&mut self) {
        self.editing = false;
        self.pane = if self.showing_results() && !self.results.is_empty() {
            Pane::Results
        } else {
            Pane::Box
        };
    }

    pub fn keys(&self) -> &[Vec<Cap>] {
        &self.keys
    }

    pub fn focused_cap(&self) -> Option<Cap> {
        self.keys.get(self.key_row)?.get(self.key_col).copied()
    }

    pub fn focused_result(&self) -> Option<&Item> {
        if self.pane != Pane::Results {
            return None;
        }
        self.results.focused()
    }

    /// Whether the right-hand side is showing results rather than the list.
    /// It is, for as long as the box still says what was searched for.
    pub fn showing_results(&self) -> bool {
        self.searched
            .as_deref()
            .is_some_and(|searched| searched == self.query.trim())
    }

    /// The list beside the letters: what the index suggests for the query,
    /// then the earlier searches -- only those, while the box is empty.
    pub fn entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        let query = self.query.trim();
        if !query.is_empty() && self.suggested_for == query {
            for suggestion in &self.suggestions {
                entries.push(Entry {
                    text: suggestion.name.clone(),
                    detail: suggestion.detail(),
                    earlier: false,
                });
            }
        }
        for earlier in self.history.iter().take(HISTORY_SHOWN) {
            entries.push(Entry {
                text: earlier.clone(),
                detail: String::new(),
                earlier: true,
            });
        }
        entries
    }

    fn enter_side(&mut self) -> bool {
        let count = self.entries().len();
        if count == 0 {
            return false;
        }
        self.pane = Pane::List;
        self.list_index = self.list_index.min(count - 1);
        true
    }

    /// Moves the remote. Returns whether anything changed.
    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        self.typing = false;
        match self.pane {
            Pane::Keys => self.step_keys(dx, dy),
            Pane::List => self.step_list(dx, dy),
            Pane::Results => self.step_results(dx, dy),
            Pane::Box => {
                if dy > 0 && self.showing_results() && !self.results.is_empty() {
                    self.pane = Pane::Results;
                    return true;
                }
                if dx < 0 {
                    self.pane = Pane::Places;
                    self.place = 0;
                    return true;
                }
                false
            }
            Pane::Places => {
                if dx > 0 {
                    self.pane = Pane::Box;
                    return true;
                }
                let next = (self.place as i32 + dy).clamp(0, 3) as usize;
                let moved = next != self.place;
                self.place = next;
                moved
            }
        }
    }

    fn step_keys(&mut self, dx: i32, dy: i32) -> bool {
        // Right off the edge of the grid is the way across. Left from the
        // first column of whatever is on the right is the only way back, so
        // crossing is never ambiguous.
        if dx > 0 {
            let width = self
                .keys
                .get(self.key_row)
                .map(|row| row.len())
                .unwrap_or(0);
            if self.key_col + 1 >= width {
                return self.enter_side();
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
            if next < 0 || next as usize >= self.keys.len() {
                return false;
            }
            // A column is a place on the panel, not an index into a row: the
            // bottom row has wide keys, and keeping the index would jump the
            // remote sideways every time it went down into it.
            let column = column_of(&self.keys[self.key_row], self.key_col);
            self.key_row = next as usize;
            self.key_col = key_at(&self.keys[self.key_row], column);
            return true;
        }
        false
    }

    fn step_list(&mut self, dx: i32, dy: i32) -> bool {
        let count = self.entries().len();
        if count == 0 || dx < 0 {
            self.pane = Pane::Keys;
            return true;
        }
        if dy < 0 && self.list_index > 0 {
            self.list_index -= 1;
            return true;
        }
        if dy > 0 && self.list_index + 1 < count {
            self.list_index += 1;
            return true;
        }
        false
    }

    fn step_results(&mut self, dx: i32, dy: i32) -> bool {
        if self.results.is_empty() {
            self.pane = Pane::Box;
            return true;
        }
        if dx < 0 && self.results.column() == 0 {
            self.pane = Pane::Places;
            self.place = 0;
            return true;
        }
        if dx != 0 {
            return self.results.step_column(dx);
        }
        if dy != 0 {
            if self.results.step_row(dy) {
                return true;
            }
            if dy < 0 {
                // Up off the first row is the search box, as on the board.
                self.pane = Pane::Box;
                return true;
            }
        }
        false
    }

    /// Ok on the letter grid.
    pub fn press(&mut self) -> Press {
        let Some(cap) = self.focused_cap() else {
            return Press::Nothing;
        };
        match cap {
            Cap::Letter(c) => self.push(c),
            Cap::Space => self.push(' '),
            // Unreachable: this grid is built without Shift, and
            // `the_grid_here_has_no_shift` holds it that way.
            Cap::Shift => return Press::Nothing,
            Cap::Backspace => {
                if self.query.pop().is_none() {
                    return Press::Nothing;
                }
            }
            Cap::Clear => {
                if self.query.is_empty() {
                    return Press::Nothing;
                }
                self.query.clear();
            }
            Cap::Search => return self.search(),
        }
        self.after_edit();
        Press::Edited
    }

    /// A character from a real keyboard. Enter arrives as Ok, not here; see
    /// `typing`.
    pub fn typed(&mut self, c: char) -> Press {
        if c == '\u{8}' {
            if self.query.pop().is_none() {
                return Press::Nothing;
            }
        } else if c.is_control() {
            return Press::Nothing;
        } else {
            self.push(c);
        }
        self.typing = true;
        self.after_edit();
        Press::Edited
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
        self.suggest_generation += 1;
        self.list_index = 0;
        // Typing opens the panel, wherever the remote was.
        self.editing = true;
        if self.pane != Pane::Keys {
            self.pane = Pane::Keys;
        }
    }

    /// Ok in the list: that text is the query, and it is searched for.
    pub fn choose_entry(&mut self) -> Press {
        let Some(entry) = self.entries().into_iter().nth(self.list_index) else {
            return Press::Nothing;
        };
        self.query = entry.text;
        self.suggest_generation += 1;
        self.search()
    }

    /// Makes the search for what is in the box.
    pub fn search(&mut self) -> Press {
        let query = self.query.trim().to_string();
        if query.is_empty() {
            return Press::Nothing;
        }
        self.query = query.clone();
        self.generation += 1;
        self.searched = Some(query.clone());
        self.searching = true;
        self.note = "Aranıyor…".into();
        self.results.clear();
        // The panel closes onto the box; the results take the remote when
        // they land.
        self.editing = false;
        self.pane = Pane::Box;
        self.remember(&query);
        Press::Search
    }

    fn remember(&mut self, query: &str) {
        self.history.retain(|earlier| !earlier.eq_ignore_ascii_case(query));
        self.history.insert(0, query.to_string());
        self.history.truncate(HISTORY_KEPT);
    }

    /// The answer, if it is still the answer to the search on the panel.
    pub fn take(&mut self, generation: u64, rows: Vec<Shelf>) {
        if generation != self.generation {
            return;
        }
        self.searching = false;
        self.results.set(rows);
        if !self.editing && self.pane == Pane::Box && !self.results.is_empty() {
            self.pane = Pane::Results;
        }
        self.note = if self.results.is_empty() {
            match self.results.len() {
                0 => format!("“{}” için sonuç yok", self.searched.as_deref().unwrap_or("")),
                _ => "Arama kataloglarından yanıt gelmedi".into(),
            }
        } else {
            String::new()
        };
    }

    pub fn fail(&mut self, generation: u64, why: &str) {
        if generation != self.generation {
            return;
        }
        self.searching = false;
        self.results.clear();
        self.note = why.to_string();
    }

    /// Suggestions, if they are still for what is in the box.
    pub fn take_suggestions(&mut self, generation: u64, query: &str, found: Vec<Suggestion>) {
        if generation != self.suggest_generation || query != self.query.trim() {
            return;
        }
        self.suggestions = found;
        self.suggested_for = query.to_string();
        let count = self.entries().len();
        if self.pane == Pane::List && self.list_index >= count {
            self.list_index = count.saturating_sub(1);
        }
    }
}

/// Where a key starts, counted in grid columns.
fn column_of(row: &[Cap], index: usize) -> usize {
    row.iter().take(index).map(|cap| cap.span() as usize).sum()
}

/// The key covering a grid column.
fn key_at(row: &[Cap], column: usize) -> usize {
    let mut start = 0;
    for (index, cap) in row.iter().enumerate() {
        let span = cap.span() as usize;
        if column < start + span {
            return index;
        }
        start += span;
    }
    row.len().saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::shelves::tests::shelf;

    fn with_results(rows: usize, per_row: usize) -> Search {
        let mut search = Search::new();
        search.query = "dune".into();
        assert_eq!(search.search(), Press::Search);
        let generation = search.generation;
        search.take(
            generation,
            (0..rows).map(|r| shelf(&format!("r{r}"), per_row)).collect(),
        );
        search
    }

    fn suggestion(name: &str) -> Suggestion {
        Suggestion {
            id: format!("tt-{name}"),
            kind: "movie".into(),
            name: name.into(),
            release_info: Some("2021".into()),
        }
    }

    fn press_key(search: &mut Search, wanted: Cap) -> Press {
        for (row, keys) in search.keys.clone().iter().enumerate() {
            if let Some(col) = keys.iter().position(|cap| *cap == wanted) {
                search.key_row = row;
                search.key_col = col;
                return search.press();
            }
        }
        panic!("no {wanted:?} key");
    }

    /// A key that does nothing is a key the remote has to walk past.
    #[test]
    fn the_grid_here_has_no_shift_and_has_ara() {
        let caps: Vec<Cap> = Search::new().keys().iter().flatten().copied().collect();
        assert!(!caps.contains(&Cap::Shift));
        assert!(caps.contains(&Cap::Search));
    }

    /// The grid is drawn at a fixed column width, so a row that is wider than
    /// the grid puts a key off the edge of the television.
    #[test]
    fn every_row_fits_the_grid() {
        for row in Search::new().keys() {
            let span: u32 = row.iter().map(|cap| cap.span()).sum();
            assert_eq!(span as usize, crate::keyboard::COLUMNS);
        }
    }

    /// Two keys that look the same and do different things are two keys nobody
    /// can use.
    #[test]
    fn no_two_keys_carry_the_same_letter() {
        let mut seen = std::collections::HashSet::new();
        for cap in Search::new().keys().iter().flatten() {
            assert!(seen.insert(cap.label(false)), "{} twice", cap.label(false));
        }
    }

    /// The fault this screen was rewritten for: every letter was a search.
    #[test]
    fn typing_only_edits_and_ara_searches_once() {
        let mut search = Search::new();
        for letter in "dune".chars() {
            assert_eq!(press_key(&mut search, Cap::Letter(letter)), Press::Edited);
        }
        assert_eq!(search.generation, 0, "no search was made while typing");
        assert!(search.suggest_generation >= 4);
        assert_eq!(press_key(&mut search, Cap::Search), Press::Search);
        assert_eq!(search.generation, 1);
        assert_eq!(search.searched.as_deref(), Some("dune"));
        assert!(search.searching);
    }

    #[test]
    fn ara_on_an_empty_box_searches_for_nothing() {
        let mut search = Search::new();
        assert_eq!(press_key(&mut search, Cap::Search), Press::Nothing);
        search.query = "   ".into();
        assert_eq!(press_key(&mut search, Cap::Search), Press::Nothing);
        assert_eq!(search.generation, 0);
    }

    #[test]
    fn an_answer_to_an_earlier_search_is_dropped() {
        let mut search = Search::new();
        search.query = "dun".into();
        search.search();
        let stale = search.generation;
        search.query = "dune".into();
        search.search();
        search.take(stale, vec![shelf("old", 3)]);
        assert!(search.results.is_empty());
        assert!(search.searching);
        search.take(search.generation, vec![shelf("new", 3)]);
        assert_eq!(search.results.focused().unwrap().id, "new-0");
    }

    #[test]
    fn suggestions_for_what_is_no_longer_typed_are_dropped() {
        let mut search = Search::new();
        search.typed('d');
        let early = search.suggest_generation;
        search.typed('u');
        search.take_suggestions(early, "d", vec![suggestion("Dogma")]);
        assert!(search.entries().is_empty());
        search.take_suggestions(search.suggest_generation, "du", vec![suggestion("Dune")]);
        assert_eq!(search.entries()[0].text, "Dune");
    }

    #[test]
    fn choosing_a_suggestion_searches_for_it() {
        let mut search = Search::new();
        search.typed('d');
        search.take_suggestions(search.suggest_generation, "d", vec![suggestion("Dune")]);
        for _ in 0..10 {
            search.step(1, 0);
        }
        assert_eq!(search.pane, Pane::List);
        assert_eq!(search.choose_entry(), Press::Search);
        assert_eq!(search.searched.as_deref(), Some("Dune"));
        assert_eq!(search.history, vec!["Dune".to_string()]);
    }

    #[test]
    fn earlier_searches_are_newest_first_without_repeats() {
        let mut search = Search::new();
        for query in ["dune", "oppenheimer", "Dune"] {
            search.query = query.into();
            search.search();
        }
        assert_eq!(search.history, vec!["Dune".to_string(), "oppenheimer".to_string()]);
        search.query.clear();
        let shown: Vec<String> = search.entries().into_iter().map(|e| e.text).collect();
        assert_eq!(shown, search.history);
    }

    #[test]
    fn a_keyboard_edit_after_a_search_shows_the_list_again() {
        let mut search = with_results(2, 5);
        assert!(search.showing_results());
        search.typed('x');
        assert!(!search.showing_results());
        search.typed('\u{8}');
        assert!(search.showing_results(), "the box says what was searched again");
    }

    /// The reference's search page: the results are the board's shelves under
    /// the search box, and the letters are in a panel that closes when the
    /// search is made.
    #[test]
    fn a_search_closes_the_panel_and_the_results_take_the_remote() {
        let search = with_results(2, 12);
        assert!(!search.editing);
        assert_eq!(search.pane, Pane::Results);
    }

    #[test]
    fn up_off_the_results_is_the_box_and_ok_there_opens_the_letters() {
        let mut search = with_results(2, 12);
        assert!(search.step(0, -1));
        assert_eq!(search.pane, Pane::Box);
        search.open_editing();
        assert_eq!((search.pane, search.editing), (Pane::Keys, true));
        search.close_editing();
        assert_eq!(search.pane, Pane::Results, "Back closes it onto the results");
    }

    #[test]
    fn left_off_the_first_poster_is_the_places() {
        let mut search = with_results(2, 12);
        assert!(search.step(-1, 0));
        assert_eq!(search.pane, Pane::Places);
        assert!(search.step(1, 0));
        assert_eq!(search.pane, Pane::Box);
    }

    #[test]
    fn each_result_row_keeps_its_own_column() {
        let mut search = with_results(2, 12);
        search.pane = Pane::Results;
        search.step(1, 0);
        search.step(0, 1);
        assert_eq!(search.results.column(), 0);
        search.step(0, -1);
        assert_eq!(search.results.column(), 1);
    }

    #[test]
    fn with_nothing_on_the_right_the_remote_stays_on_the_letters() {
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
        assert!(search.focused_cap().is_some());
    }

    /// Down into the bottom row lands on the key under the remote, not on
    /// whichever key has the same index.
    #[test]
    fn down_into_the_wide_keys_stays_in_the_same_place() {
        let mut search = Search::new();
        search.key_row = search.keys.len() - 2;
        search.key_col = 9;
        search.step(0, 1);
        assert_eq!(search.focused_cap(), Some(Cap::Search));
        search.step(0, -1);
        assert_eq!(search.key_col, 8, "back up into the column Ara starts in");
    }

    #[test]
    fn keyboard_enter_is_a_search_and_a_remote_move_ends_that() {
        let mut search = Search::new();
        search.typed('a');
        assert!(search.typing);
        search.step(1, 0);
        assert!(!search.typing);
    }

    #[test]
    fn backspace_on_an_empty_query_changes_nothing() {
        let mut search = Search::new();
        assert_eq!(search.typed('\u{8}'), Press::Nothing);
        assert_eq!(press_key(&mut search, Cap::Backspace), Press::Nothing);
    }
}
