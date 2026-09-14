//! Everything the box can show, as a grid rather than as a shelf.
//!
//! The home screen is a shelf of shelves and is made for browsing; this screen
//! is made for finding. Four sections across the top, a poster grid under them,
//! and the same structural focus model as everywhere else: a row, a column, and
//! a remembered position per section so moving between them and back lands
//! where it was left.

use crate::state::{Item, Shelf};

/// How many posters fit across the grid once the panel on the right has its
/// room. Fixed rather than measured: the focus model is a grid, and a grid that
/// reflowed would move a title out from under the remote.
pub const COLUMNS: usize = 4;

pub struct Section {
    pub title: String,
    pub note: String,
    pub items: Vec<Item>,
}

pub struct Library {
    pub sections: Vec<Section>,
    /// Which section is open.
    pub tab: usize,
    /// True while the remote is on the section strip rather than in the grid.
    pub on_tabs: bool,
    /// One remembered position per section.
    positions: Vec<usize>,
}

impl Library {
    pub fn new() -> Self {
        Self { sections: Vec::new(), tab: 0, on_tabs: true, positions: Vec::new() }
    }

    /// Built from what the home screen already has, so opening this screen
    /// costs nothing over the network. "Devam Et" is the account's unfinished
    /// titles; films and series are separated by the catalogue's own type;
    /// "Kitaplık" is what this appliance holds itself.
    pub fn build(&mut self, shelves: &[Shelf]) {
        let holding = self.focused().map(|item| item.id.clone());

        let mut sections = vec![
            Section { title: "Devam Et".into(), note: "Yarım kalanlar".into(), items: Vec::new() },
            Section { title: "Filmler".into(), note: String::new(), items: Vec::new() },
            Section { title: "Diziler".into(), note: String::new(), items: Vec::new() },
            Section { title: "Kitaplık".into(), note: "Bu cihazda".into(), items: Vec::new() },
        ];

        let mut seen: Vec<std::collections::HashSet<String>> =
            vec![std::collections::HashSet::new(); sections.len()];

        for shelf in shelves {
            for item in &shelf.items {
                let bucket = if item.progress > 0.0 && item.progress < 0.95 {
                    0
                } else if item.local {
                    3
                } else if item.kind == "series" {
                    2
                } else {
                    1
                };
                if seen[bucket].insert(item.id.clone()) {
                    sections[bucket].items.push(item.clone());
                }
                // A local title is also a film or a series, and a viewer looking
                // for one under "Filmler" should find it.
                if bucket == 3 {
                    let also = if item.kind == "series" { 2 } else { 1 };
                    if seen[also].insert(item.id.clone()) {
                        sections[also].items.push(item.clone());
                    }
                }
            }
        }

        for section in &mut sections {
            if section.note.is_empty() {
                section.note = format!("{} başlık", section.items.len());
            }
        }

        self.sections = sections;
        self.positions.resize(self.sections.len(), 0);

        // Empty sections are not landed on, and the remote keeps the title it
        // was on when this was rebuilt behind a refresh.
        if self.items().is_empty() {
            if let Some(next) = (0..self.sections.len()).find(|i| !self.sections[*i].items.is_empty())
            {
                self.tab = next;
            }
        }
        if let Some(id) = holding {
            if let Some(at) = self.items().iter().position(|item| item.id == id) {
                self.positions[self.tab] = at;
            }
        }
        self.clamp();
    }

    pub fn items(&self) -> &[Item] {
        self.sections.get(self.tab).map(|s| s.items.as_slice()).unwrap_or(&[])
    }

    pub fn index(&self) -> usize {
        self.positions.get(self.tab).copied().unwrap_or(0)
    }

    pub fn focused(&self) -> Option<&Item> {
        self.items().get(self.index())
    }

    fn clamp(&mut self) {
        let len = self.items().len();
        if let Some(slot) = self.positions.get_mut(self.tab) {
            *slot = (*slot).min(len.saturating_sub(1));
        }
        if len == 0 {
            self.on_tabs = true;
        }
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        if self.on_tabs {
            if dy > 0 && !self.items().is_empty() {
                self.on_tabs = false;
                return true;
            }
            if dy != 0 {
                return false;
            }
            let next = (self.tab as i32 + dx).clamp(0, self.sections.len() as i32 - 1) as usize;
            if next == self.tab {
                return false;
            }
            self.tab = next;
            self.clamp();
            return true;
        }

        let items = self.items().len();
        if items == 0 {
            self.on_tabs = true;
            return true;
        }
        let index = self.index();
        let column = index % COLUMNS;

        if dy < 0 && index < COLUMNS {
            // Up from the first row goes to the section strip, which is the one
            // thing above the grid.
            self.on_tabs = true;
            return true;
        }
        let next = if dy == 0 {
            let row_start = index - column;
            let width = COLUMNS.min(items - row_start);
            row_start + (column as i32 + dx).clamp(0, width as i32 - 1) as usize
        } else {
            let candidate = index as i32 + dy * COLUMNS as i32;
            if candidate < 0 {
                return false;
            }
            let candidate = candidate as usize;
            // The last row is usually short; stepping down onto it lands on its
            // last title rather than nowhere.
            if candidate >= items {
                if dy > 0 && index / COLUMNS < (items - 1) / COLUMNS {
                    items - 1
                } else {
                    return false;
                }
            } else {
                candidate
            }
        };
        if next == index {
            return false;
        }
        self.positions[self.tab] = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Item, Shelf};

    fn shelf(items: Vec<Item>) -> Shelf {
        Shelf { title: "x".into(), source: String::new(), items }
    }

    fn film(id: &str) -> Item {
        Item::stub(id)
    }

    fn series(id: &str) -> Item {
        let mut item = Item::stub(id);
        item.kind = "series".into();
        item
    }

    #[test]
    fn a_title_lands_in_exactly_one_kind() {
        let mut library = Library::new();
        library.build(&[shelf(vec![film("a"), series("b")])]);
        assert_eq!(library.sections[1].items.len(), 1);
        assert_eq!(library.sections[2].items.len(), 1);
    }

    #[test]
    fn an_unfinished_title_is_the_first_section() {
        let mut item = film("a");
        item.progress = 0.4;
        let mut library = Library::new();
        library.build(&[shelf(vec![item])]);
        assert_eq!(library.sections[0].items.len(), 1);
        assert!(library.sections[1].items.is_empty());
    }

    #[test]
    fn the_same_title_twice_is_listed_once() {
        let mut library = Library::new();
        library.build(&[shelf(vec![film("a")]), shelf(vec![film("a")])]);
        assert_eq!(library.sections[1].items.len(), 1);
    }

    #[test]
    fn focus_starts_on_the_sections_and_down_enters_the_grid() {
        let mut library = Library::new();
        library.build(&[shelf(vec![film("a")])]);
        assert!(library.on_tabs);
        library.tab = 1;
        assert!(library.step(0, 1));
        assert!(!library.on_tabs);
        assert!(library.focused().is_some());
    }

    #[test]
    fn up_from_the_first_row_returns_to_the_sections() {
        let mut library = Library::new();
        library.build(&[shelf((0..9).map(|i| film(&format!("a{i}"))).collect())]);
        library.tab = 1;
        library.on_tabs = false;
        assert!(library.step(0, 1));
        assert!(library.step(0, -1));
        assert!(library.step(0, -1));
        assert!(library.on_tabs);
    }

    #[test]
    fn a_section_remembers_where_the_remote_was() {
        let mut library = Library::new();
        library.build(&[shelf((0..20).map(|i| film(&format!("a{i}"))).collect())]);
        library.tab = 1;
        library.on_tabs = false;
        library.step(1, 0);
        library.step(1, 0);
        let at = library.index();
        library.on_tabs = true;
        library.step(1, 0);
        library.step(-1, 0);
        assert_eq!(library.index(), at);
    }

    #[test]
    fn stepping_down_onto_a_short_last_row_lands_on_it() {
        // Two full rows and one short one, whatever COLUMNS happens to be.
        let count = COLUMNS * 2 + 1;
        let mut library = Library::new();
        library.build(&[shelf((0..count).map(|i| film(&format!("a{i}"))).collect())]);
        library.tab = 1;
        library.on_tabs = false;
        // On the last full row, above the short one.
        library.positions[1] = COLUMNS + 1;
        assert!(library.step(0, 1));
        assert_eq!(library.index(), count - 1);
    }
}
