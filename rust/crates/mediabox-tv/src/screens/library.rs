//! The account's library, as a grid rather than as a shelf.
//!
//! The reference's library, read out of stremio-core's `LibraryWithFilters`:
//! the titles in the account's library -- not removed, not temporary -- shown
//! all together or by type, in one of six orders:
//!
//! - last watched first (the default), by name, by name backwards, most
//!   watched first, watched first, not watched first.
//!
//! Laid out as the reference's library page: the board's column of places and
//! search box, then a type dropdown and the orders as chips, and a grid of
//! posters under them, nine across. Two more choices in the dropdown are this
//! television's own: "İzlemeye devam edin", in the order every Stremio client
//! shows it, and the titles this appliance holds itself.
//!
//! Ok on a poster is the reference's menu for it -- its details, "Vazgeç" off
//! "İzlemeye devam edin", watched or not, "Kaldır" from the library -- since a
//! remote has no second button to open it with. The focus model is the
//! structural one, with a remembered position per section.

use std::cmp::Ordering;

use crate::state::Item;

/// The reference's grid at its 1920-pixel layout: nine across. Fixed rather
/// than measured: the focus model is a grid, and a grid that reflowed would
/// move a title out from under the remote.
pub const COLUMNS: usize = 9;

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
            // The reference's SORT_ strings, capitalised as its chips are.
            Sort::LastWatched => "Son İzlenen",
            Sort::Name => "A-Z",
            Sort::NameReverse => "Z-A",
            Sort::TimesWatched => "En Çok İzlenen",
            Sort::Watched => "İzlenen",
            Sort::NotWatched => "İzlenmeyen",
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
    /// "İzlemeye devam edin", as the media core ordered it.
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
    Places,
    Search,
    /// The type dropdown and the order chips.
    Filters,
    Grid,
    /// The focused poster's menu.
    Menu,
}

/// What can be done with the focused title, from its menu, in the
/// reference's order.
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
            Act::RemoveFromContinuing => "Vazgeç",
            Act::MarkWatched => "İzlendi olarak işaretle",
            Act::MarkUnwatched => "İzlenmedi olarak işaretle",
            Act::RemoveFromLibrary => "Kaldır",
        }
    }
}

pub struct Library {
    pub sections: Vec<Section>,
    /// Which section is open.
    pub tab: usize,
    pub zone: Zone,
    /// Where the remote is on the filters: 0 the dropdown, then the chips.
    pub tab_focus: usize,
    pub sort: Sort,
    /// The dropdown's list, when it is down: the highlighted section.
    pub type_open: Option<usize>,
    pub action: usize,
    /// The place the remote is on, down the left; 2 is the library.
    pub place: usize,
    returns_to: Zone,
    /// One remembered position per section.
    positions: Vec<usize>,
}

impl Library {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            tab: 0,
            zone: Zone::Filters,
            tab_focus: 0,
            sort: Sort::LastWatched,
            type_open: None,
            action: 0,
            place: 2,
            returns_to: Zone::Grid,
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
            sections.push(Section {
                title: crate::state::type_name(kind),
                note: String::new(),
                kind: SectionKind::Account(Some(kind)),
                items: Vec::new(),
            });
        }
        sections.push(Section {
            title: "İzlemeye devam edin".into(),
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

    /// Opens "İzlemeye devam edin", with the remote in the grid: where the board's
    /// "Tümünü Gör" on "İzlemeye devam edin" leads.
    pub fn show_continuing(&mut self) {
        if let Some(index) = self
            .sections
            .iter()
            .position(|s| s.kind == SectionKind::Continuing)
        {
            self.tab = index;
            self.tab_focus = 0;
            self.zone = if self.items().is_empty() { Zone::Filters } else { Zone::Grid };
            self.clamp();
        }
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
        if !matches!(self.zone, Zone::Grid | Zone::Menu) {
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
        // The reference offers "Vazgeç" on any poster with progress on it.
        if item.continuing || item.progress > 0.0 {
            acts.push(Act::RemoveFromContinuing);
        }
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
        if len == 0 && matches!(self.zone, Zone::Grid | Zone::Menu) {
            self.zone = Zone::Filters;
        }
        self.tab_focus = self.tab_focus.min(self.filter_stops() - 1);
        self.action = self.action.min(self.actions().len().saturating_sub(1));
    }

    /// The filters' stops: the dropdown, then an order chip each where the
    /// open section is the account's.
    fn filter_stops(&self) -> usize {
        if self.sortable() { 1 + SORTS.len() } else { 1 }
    }

    /// The chip the remote is on, if it is on one.
    pub fn focused_chip(&self) -> Option<usize> {
        (self.zone == Zone::Filters && self.tab_focus > 0).then(|| self.tab_focus - 1)
    }

    pub fn on_types(&self) -> bool {
        self.zone == Zone::Filters && self.tab_focus == 0
    }

    fn to_places(&mut self) -> bool {
        self.returns_to = self.zone;
        self.zone = Zone::Places;
        self.place = 2;
        true
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        if let Some(open) = self.type_open {
            let next = (open as i32 + dy).clamp(0, self.sections.len() as i32 - 1) as usize;
            self.type_open = Some(next);
            return next != open;
        }
        match self.zone {
            Zone::Places => {
                if dx > 0 {
                    self.zone = self.returns_to;
                    if self.zone == Zone::Grid && self.items().is_empty() {
                        self.zone = Zone::Filters;
                    }
                    return true;
                }
                let next = (self.place as i32 + dy).clamp(0, 3) as usize;
                let moved = next != self.place;
                self.place = next;
                moved
            }
            Zone::Search => {
                if dx < 0 {
                    return self.to_places();
                }
                if dy > 0 {
                    self.zone = Zone::Filters;
                    return true;
                }
                false
            }
            Zone::Filters => {
                if dy > 0 && !self.items().is_empty() {
                    self.zone = Zone::Grid;
                    return true;
                }
                if dy < 0 {
                    self.zone = Zone::Search;
                    return true;
                }
                if dx < 0 && self.tab_focus == 0 {
                    return self.to_places();
                }
                if dx == 0 {
                    return false;
                }
                let next =
                    (self.tab_focus as i32 + dx).clamp(0, self.filter_stops() as i32 - 1) as usize;
                let moved = next != self.tab_focus;
                self.tab_focus = next;
                moved
            }
            Zone::Menu => {
                if dy == 0 {
                    return false;
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
            self.zone = Zone::Filters;
            return true;
        }
        let index = self.index();
        let column = index % COLUMNS;

        if dy < 0 && index < COLUMNS {
            // Up from the first row is the filters, the one thing above it.
            self.zone = Zone::Filters;
            return true;
        }
        if dx < 0 && column == 0 {
            return self.to_places();
        }
        let row_start = index - column;
        let width = COLUMNS.min(items - row_start);
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

    /// Ok on a poster: its menu, on its first line.
    pub fn open_menu(&mut self) -> bool {
        if self.zone != Zone::Grid || self.focused().is_none() {
            return false;
        }
        self.zone = Zone::Menu;
        self.action = 0;
        true
    }

    /// Ok on the dropdown opens its list, on the section showing.
    pub fn open_types(&mut self) -> bool {
        if !self.on_types() {
            return false;
        }
        self.type_open = Some(self.tab);
        true
    }

    /// Ok in the list: that section, and the list goes up.
    pub fn pick_type(&mut self) -> bool {
        let Some(open) = self.type_open.take() else {
            return false;
        };
        self.tab = open;
        self.clamp();
        true
    }

    pub fn close_types(&mut self) -> bool {
        self.type_open.take().is_some()
    }

    /// Ok on a chip: that order, as the reference's chips are chosen.
    pub fn choose_sort(&mut self) -> bool {
        let Some(chip) = self.focused_chip() else {
            return false;
        };
        if SORTS[chip] == self.sort {
            return false;
        }
        self.sort = SORTS[chip];
        self.apply_sort();
        // A new order starts at the top: the first title is the point of it.
        self.positions.iter_mut().for_each(|p| *p = 0);
        true
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
        assert_eq!(titles, vec!["Tümü", "Film", "Dizi", "İzlemeye devam edin", "Bu cihazda"]);
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
    fn an_order_is_chosen_from_its_chip_and_a_section_from_the_dropdown() {
        let mut library = built(vec![film("a"), series("b")]);
        assert!(library.on_types());
        assert!(library.step(1, 0));
        assert!(library.step(1, 0));
        assert_eq!(library.focused_chip(), Some(1));
        assert!(library.choose_sort());
        assert_eq!(library.sort, Sort::Name);

        library.tab_focus = 0;
        assert!(library.open_types());
        library.step(0, 1);
        assert!(library.pick_type());
        assert_eq!(library.sections[library.tab].title, "Film");
    }

    /// The board's frame: Up off the filters is the search box, Left off the
    /// first poster the places.
    #[test]
    fn the_places_and_the_search_box_are_the_boards() {
        let mut library = built((0..3).map(|i| film(&format!("a{i}"))).collect());
        library.step(0, 1);
        assert_eq!(library.zone, Zone::Grid);
        assert!(library.step(-1, 0));
        assert_eq!(library.zone, Zone::Places);
        assert_eq!(library.place, 2);
        assert!(library.step(1, 0));
        assert_eq!(library.zone, Zone::Grid);
        library.step(0, -1);
        assert!(library.step(0, -1));
        assert_eq!(library.zone, Zone::Search);
    }

    #[test]
    fn ok_on_a_poster_is_its_menu() {
        let mut library = built(vec![film("a")]);
        library.step(0, 1);
        assert!(library.open_menu());
        assert_eq!(library.focused_act(), Some(Act::Open));
        assert!(library.step(0, 1));
        assert_eq!(library.focused_act(), Some(Act::MarkWatched));
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
        assert_eq!(library.zone, Zone::Filters, "an emptied section leaves nothing to stand on");
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
