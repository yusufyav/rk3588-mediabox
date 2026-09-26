//! The account's library, as a grid rather than as a shelf.
//!
//! The reference's library, read out of stremio-core's `LibraryWithFilters`:
//! the titles in the account's library -- not removed, not temporary -- shown
//! all together or by type, in one of six orders:
//!
//! - last watched first (the default), by name, by name backwards, most
//!   watched first, watched first, not watched first.
//!
//! Two more sections are this television's own: "Devam Et", in the order
//! every Stremio client shows it, and the titles this appliance holds itself.
//!
//! Beside the grid is the focused title's record and what can be done with
//! it: open it, mark a film watched, take a title out of the library, or take
//! it out of "Devam Et". The focus model is the structural one: a row of
//! sections with the order at its end, the grid, and the actions, with a
//! remembered position per section.

use std::cmp::Ordering;

use crate::state::Item;

/// How many posters fit across the grid once the panel on the right has its
/// room. Fixed rather than measured: the focus model is a grid, and a grid that
/// reflowed would move a title out from under the remote.
pub const COLUMNS: usize = 4;

/// stremio-core's `Sort`, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    LastWatched,
    Name,
    NameReverse,
    TimesWatched,
    Watched,
    NotWatched,
}

pub const SORTS: [Sort; 6] = [
    Sort::LastWatched,
    Sort::Name,
    Sort::NameReverse,
    Sort::TimesWatched,
    Sort::Watched,
    Sort::NotWatched,
];

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::LastWatched => "Son izlenen",
            Sort::Name => "A–Z",
            Sort::NameReverse => "Z–A",
            Sort::TimesWatched => "En çok izlenen",
            Sort::Watched => "İzlenenler önce",
            Sort::NotWatched => "İzlenmeyenler önce",
        }
    }

    /// `Sort::sort_items`. Timestamps are the account's ISO 8601 strings, which
    /// order the same way as the times they are.
    fn order(self, a: &Item, b: &Item) -> Ordering {
        let last = |item: &Item| item.record.as_ref().and_then(|r| r.last_watched.clone());
        let added = |item: &Item| item.record.as_ref().and_then(|r| r.added.clone());
        let times = |item: &Item| item.record.as_ref().map(|r| r.times_watched).unwrap_or(0);
        let watched = |item: &Item| times(item) > 0;
        match self {
            Sort::LastWatched => last(b).cmp(&last(a)),
            Sort::TimesWatched => times(b).cmp(&times(a)),
            Sort::Watched => watched(b)
                .cmp(&watched(a))
                .then(last(b).cmp(&last(a)))
                .then(added(b).cmp(&added(a))),
            Sort::NotWatched => watched(a)
                .cmp(&watched(b))
                .then(last(a).cmp(&last(b)))
                .then(added(a).cmp(&added(b))),
            Sort::Name => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            Sort::NameReverse => b.title.to_lowercase().cmp(&a.title.to_lowercase()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// The account's library, of one type or of all of them.
    Account(Option<&'static str>),
    /// "Devam Et", as the media core ordered it.
    Continuing,
    /// This appliance's own titles.
    Local,
}

pub struct Section {
    pub title: String,
    pub note: String,
    pub kind: SectionKind,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Tabs,
    Grid,
    Actions,
}

/// What can be done with the focused title, from the panel beside the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Open,
    MarkWatched,
    MarkUnwatched,
    RemoveFromLibrary,
    RemoveFromContinuing,
}

impl Act {
    pub fn label(self) -> &'static str {
        match self {
            Act::Open => "Ayrıntılar",
            Act::MarkWatched => "İzlendi olarak işaretle",
            Act::MarkUnwatched => "İzlenmedi olarak işaretle",
            Act::RemoveFromLibrary => "Kütüphaneden çıkar",
            Act::RemoveFromContinuing => "Devam Et'ten çıkar",
        }
    }
}

pub struct Library {
    pub sections: Vec<Section>,
    /// Which section is open.
    pub tab: usize,
    pub zone: Zone,
    /// Where the remote is on the tab row: a section, or the order at its end.
    pub tab_focus: usize,
    pub sort: Sort,
    /// The order's list, when it is down: the highlighted one.
    pub sort_open: Option<usize>,
    pub action: usize,
    /// One remembered position per section.
    positions: Vec<usize>,
}

impl Library {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            tab: 0,
            zone: Zone::Tabs,
            tab_focus: 0,
            sort: Sort::LastWatched,
            sort_open: None,
            action: 0,
            positions: Vec::new(),
        }
    }

    /// Built from the account's library, its "Devam Et", and this appliance's
    /// own titles, as the media core listed them. Nothing is asked for over
    /// the network to open this screen.
    pub fn build(&mut self, account: &[Item], continuing: &[Item], local: &[Item]) {
        let holding = self.focused().map(|item| item.id.clone());
        let showing = self.sections.get(self.tab).map(|s| s.kind);

        let mut kinds: Vec<&'static str> = Vec::new();
        for item in account {
            let kind: &'static str = match item.kind.as_str() {
                "movie" => "movie",
                "series" => "series",
                "channel" => "channel",
                "tv" => "tv",
                _ => "other",
            };
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        kinds.sort_by_key(|kind| match *kind {
            "movie" => 0,
            "series" => 1,
            "channel" => 2,
            "tv" => 3,
            _ => 4,
        });

        let mut sections = vec![Section {
            title: "Tümü".into(),
            note: String::new(),
            kind: SectionKind::Account(None),
            items: Vec::new(),
        }];
        for kind in kinds {
            let title = match kind {
                "movie" => "Filmler",
                "series" => "Diziler",
                "channel" => "Kanallar",
                "tv" => "TV",
                _ => "Diğer",
            };
            sections.push(Section {
                title: title.into(),
                note: String::new(),
                kind: SectionKind::Account(Some(kind)),
                items: Vec::new(),
            });
        }
        sections.push(Section {
            title: "Devam Et".into(),
            note: String::new(),
            kind: SectionKind::Continuing,
            items: continuing.to_vec(),
        });
        sections.push(Section {
            title: "Bu cihazda".into(),
            note: String::new(),
            kind: SectionKind::Local,
            items: local.to_vec(),
        });
        for section in &mut sections {
            if let SectionKind::Account(kind) = section.kind {
                section.items = account
                    .iter()
                    .filter(|item| match kind {
                        None => true,
                        Some("other") => {
                            !matches!(item.kind.as_str(), "movie" | "series" | "channel" | "tv")
                        }
                        Some(kind) => item.kind == kind,
                    })
                    .cloned()
                    .collect();
            }
        }

        self.sections = sections;
        self.apply_sort();
        self.positions.resize(self.sections.len(), 0);

        // Back on the section it was showing, if it is still there.
        self.tab = showing
            .and_then(|kind| self.sections.iter().position(|s| s.kind == kind))
            .unwrap_or(0);
        if let Some(id) = holding {
            if let Some(at) = self.items().iter().position(|item| item.id == id) {
                self.positions[self.tab] = at;
            }
        }
        self.clamp();
    }

    fn apply_sort(&mut self) {
        let sort = self.sort;
        for section in &mut self.sections {
            if matches!(section.kind, SectionKind::Account(_)) {
                section.items.sort_by(|a, b| sort.order(a, b));
            }
            section.note = format!("{} başlık", section.items.len());
        }
    }

    pub fn items(&self) -> &[Item] {
        self.sections
            .get(self.tab)
            .map(|s| s.items.as_slice())
            .unwrap_or(&[])
    }

    pub fn index(&self) -> usize {
        self.positions.get(self.tab).copied().unwrap_or(0)
    }

    pub fn focused(&self) -> Option<&Item> {
        if self.zone == Zone::Tabs {
            return None;
        }
        self.items().get(self.index())
    }

    /// Whether the open section is the account's library, where the order
    /// applies.
    pub fn sortable(&self) -> bool {
        self.sections
            .get(self.tab)
            .is_some_and(|s| matches!(s.kind, SectionKind::Account(_)))
    }

    /// The actions for the focused title.
    pub fn actions(&self) -> Vec<Act> {
        let Some(item) = self.focused() else {
            return Vec::new();
        };
        let mut acts = vec![Act::Open];
        if item.local {
            return acts;
        }
        let record = item.record.as_ref();
        if item.kind != "series" {
            if record.is_some_and(|r| r.times_watched > 0) {
                acts.push(Act::MarkUnwatched);
            } else {
                acts.push(Act::MarkWatched);
            }
        }
        if record.is_some_and(|r| r.in_library) {
            acts.push(Act::RemoveFromLibrary);
        }
        if item.continuing {
            acts.push(Act::RemoveFromContinuing);
        }
        acts
    }

    pub fn focused_act(&self) -> Option<Act> {
        self.actions().get(self.action).copied()
    }

    fn clamp(&mut self) {
        let len = self.items().len();
        if let Some(slot) = self.positions.get_mut(self.tab) {
            *slot = (*slot).min(len.saturating_sub(1));
        }
        if len == 0 && self.zone != Zone::Tabs {
            self.zone = Zone::Tabs;
            self.tab_focus = self.tab;
        }
        self.action = self.action.min(self.actions().len().saturating_sub(1));
    }

    /// The tab row's stops: the sections, then the order.
    fn tab_stops(&self) -> usize {
        self.sections.len() + 1
    }

    pub fn on_sort(&self) -> bool {
        self.zone == Zone::Tabs && self.tab_focus == self.sections.len()
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        if let Some(open) = self.sort_open {
            let next = (open as i32 + dy).clamp(0, SORTS.len() as i32 - 1) as usize;
            self.sort_open = Some(next);
            return next != open;
        }
        match self.zone {
            Zone::Tabs => {
                if dy > 0 && !self.items().is_empty() {
                    self.zone = Zone::Grid;
                    return true;
                }
                if dy != 0 {
                    return false;
                }
                let next =
                    (self.tab_focus as i32 + dx).clamp(0, self.tab_stops() as i32 - 1) as usize;
                if next == self.tab_focus {
                    return false;
                }
                self.tab_focus = next;
                // Walking the sections opens them; the order is only a stop.
                if next < self.sections.len() {
                    self.tab = next;
                    self.clamp();
                }
                true
            }
            Zone::Actions => {
                if dx < 0 {
                    self.zone = Zone::Grid;
                    return true;
                }
                let count = self.actions().len();
                let next = (self.action as i32 + dy).clamp(0, count as i32 - 1) as usize;
                let moved = next != self.action;
                self.action = next;
                moved
            }
            Zone::Grid => self.step_grid(dx, dy),
        }
    }

    fn step_grid(&mut self, dx: i32, dy: i32) -> bool {
        let items = self.items().len();
        if items == 0 {
            self.zone = Zone::Tabs;
            return true;
        }
        let index = self.index();
        let column = index % COLUMNS;

        if dy < 0 && index < COLUMNS {
            // Up from the first row goes to the section strip, which is the one
            // thing above the grid.
            self.zone = Zone::Tabs;
            self.tab_focus = self.tab;
            return true;
        }
        let row_start = index - column;
        let width = COLUMNS.min(items - row_start);
        if dx > 0 && column + 1 >= width {
            // Right off the end of a row is the panel beside the grid.
            self.zone = Zone::Actions;
            self.action = 0;
            return true;
        }
        let next = if dy == 0 {
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
        self.action = 0;
        true
    }

    /// Ok on the order opens its list, on the order in use. It can be chosen
    /// from any section and applies to the account's; the chip is dimmed on
    /// the sections it does not order.
    pub fn open_sort(&mut self) -> bool {
        if !self.on_sort() {
            return false;
        }
        self.sort_open = SORTS.iter().position(|s| *s == self.sort);
        self.sort_open.is_some()
    }

    /// Ok in the list: that order, and the list goes up.
    pub fn pick_sort(&mut self) -> bool {
        let Some(open) = self.sort_open.take() else {
            return false;
        };
        self.sort = SORTS[open];
        self.apply_sort();
        // A new order starts at the top: the first title is the point of it.
        self.positions.iter_mut().for_each(|p| *p = 0);
        true
    }

    pub fn close_sort(&mut self) -> bool {
        self.sort_open.take().is_some()
    }

    /// Takes a title out of a section at once, rather than waiting for the
    /// account to be read again: the change has been made, and the grid
    /// should say so. The remote stays where it was, on the next title.
    pub fn drop_from(&mut self, id: &str, library: bool, continuing: bool) {
        for section in &mut self.sections {
            let affected = match section.kind {
                SectionKind::Account(_) => library,
                SectionKind::Continuing => continuing,
                SectionKind::Local => false,
            };
            if affected {
                section.items.retain(|item| item.id != id);
            }
        }
        if continuing {
            for section in &mut self.sections {
                for item in &mut section.items {
                    if item.id == id {
                        item.continuing = false;
                        item.progress = 0.0;
                    }
                }
            }
        }
        self.apply_sort();
        self.clamp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WatchState;

    fn record(last: &str, times: u64, added: &str) -> WatchState {
        WatchState {
            time_offset: None,
            duration: None,
            last_watched: Some(last.into()),
            times_watched: times,
            added: Some(added.into()),
            in_library: true,
        }
    }

    fn film(id: &str) -> Item {
        let mut item = Item::stub(id);
        item.record = Some(record("2026-01-01T00:00:00.000Z", 0, "2025-01-01T00:00:00.000Z"));
        item
    }

    fn series(id: &str) -> Item {
        let mut item = film(id);
        item.kind = "series".into();
        item
    }

    fn built(account: Vec<Item>) -> Library {
        let mut library = Library::new();
        library.build(&account, &[], &[]);
        library
    }

    #[test]
    fn the_sections_are_all_then_each_type_then_devam_et_and_this_device() {
        let library = built(vec![series("b"), film("a")]);
        let titles: Vec<&str> = library.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Tümü", "Filmler", "Diziler", "Devam Et", "Bu cihazda"]);
        assert_eq!(library.sections[0].items.len(), 2);
        assert_eq!(library.sections[1].items.len(), 1);
    }

    #[test]
    fn the_default_order_is_last_watched_first() {
        let mut old = film("old");
        old.record = Some(record("2025-01-01T00:00:00.000Z", 1, "2024-01-01T00:00:00.000Z"));
        let mut new = film("new");
        new.record = Some(record("2026-09-01T00:00:00.000Z", 0, "2024-01-01T00:00:00.000Z"));
        let library = built(vec![old, new]);
        assert_eq!(library.sections[0].items[0].id, "new");
    }

    #[test]
    fn each_order_is_the_references() {
        let mut a = film("Alpha");
        a.title = "Alpha".into();
        a.record = Some(record("2026-01-01T00:00:00.000Z", 3, "2024-01-01T00:00:00.000Z"));
        let mut b = film("beta");
        b.title = "beta".into();
        b.record = Some(record("2026-02-01T00:00:00.000Z", 0, "2024-02-01T00:00:00.000Z"));
        let mut library = built(vec![a, b]);
        let order = |library: &Library| -> Vec<String> {
            library.sections[0].items.iter().map(|i| i.id.clone()).collect()
        };
        for (sort, expected) in [
            (Sort::LastWatched, ["beta", "Alpha"]),
            (Sort::Name, ["Alpha", "beta"]),
            (Sort::NameReverse, ["beta", "Alpha"]),
            (Sort::TimesWatched, ["Alpha", "beta"]),
            (Sort::Watched, ["Alpha", "beta"]),
            (Sort::NotWatched, ["beta", "Alpha"]),
        ] {
            library.sort = sort;
            library.apply_sort();
            assert_eq!(order(&library), expected, "{sort:?}");
        }
    }

    #[test]
    fn the_order_is_chosen_from_its_list_at_the_end_of_the_tabs() {
        let mut library = built(vec![film("a"), film("b")]);
        for _ in 0..10 {
            library.step(1, 0);
        }
        assert!(library.on_sort());
        assert!(library.open_sort());
        library.step(0, 1);
        assert!(library.pick_sort());
        assert_eq!(library.sort, Sort::Name);
    }

    #[test]
    fn right_off_the_end_of_a_row_is_the_actions_and_left_comes_back() {
        let mut library = built((0..6).map(|i| film(&format!("a{i}"))).collect());
        library.step(0, 1);
        for _ in 0..COLUMNS {
            library.step(1, 0);
        }
        assert_eq!(library.zone, Zone::Actions);
        assert_eq!(library.focused_act(), Some(Act::Open));
        assert!(library.step(-1, 0));
        assert_eq!(library.zone, Zone::Grid);
    }

    #[test]
    fn a_film_can_be_marked_watched_and_taken_out_of_the_library() {
        let library = {
            let mut l = built(vec![film("a")]);
            l.zone = Zone::Grid;
            l
        };
        assert_eq!(library.actions(), vec![Act::Open, Act::MarkWatched, Act::RemoveFromLibrary]);
    }

    #[test]
    fn a_title_on_devam_et_can_be_taken_off_it() {
        let mut watching = film("w");
        watching.continuing = true;
        let mut library = Library::new();
        library.build(&[], &[watching], &[]);
        library.tab = 1;
        library.zone = Zone::Grid;
        assert!(library.actions().contains(&Act::RemoveFromContinuing));
        library.drop_from("w", false, true);
        assert!(library.items().is_empty());
        assert_eq!(library.zone, Zone::Tabs, "an emptied section leaves nothing to stand on");
    }

    #[test]
    fn removing_keeps_the_remote_on_the_next_title() {
        let mut library = built((0..3).map(|i| film(&format!("a{i}"))).collect());
        library.zone = Zone::Grid;
        library.positions[0] = 1;
        let removed = library.focused().unwrap().id.clone();
        library.drop_from(&removed, true, false);
        assert_eq!(library.items().len(), 2);
        assert_eq!(library.index(), 1);
        assert_ne!(library.focused().unwrap().id, removed);
    }

    #[test]
    fn a_rebuild_keeps_the_section_and_the_title() {
        let items: Vec<Item> = (0..9).map(|i| film(&format!("a{i}"))).collect();
        let mut library = built(items.clone());
        library.tab = 1;
        library.zone = Zone::Grid;
        library.positions[1] = 4;
        let id = library.focused().unwrap().id.clone();
        library.build(&items, &[], &[]);
        assert_eq!(library.tab, 1);
        assert_eq!(library.focused().unwrap().id, id);
    }

    #[test]
    fn stepping_down_onto_a_short_last_row_lands_on_it() {
        let count = COLUMNS * 2 + 1;
        let mut library = built((0..count).map(|i| film(&format!("a{i}"))).collect());
        library.zone = Zone::Grid;
        library.positions[0] = COLUMNS + 1;
        assert!(library.step(0, 1));
        assert_eq!(library.index(), count - 1);
    }

    #[test]
    fn a_title_on_this_device_can_only_be_opened() {
        let mut local = Item::stub("l");
        local.local = true;
        let mut library = Library::new();
        library.build(&[], &[], &[local]);
        library.tab = 2;
        library.zone = Zone::Grid;
        assert_eq!(library.actions(), vec![Act::Open]);
    }
}
