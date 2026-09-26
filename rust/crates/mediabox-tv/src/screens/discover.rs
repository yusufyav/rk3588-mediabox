//! Discover: every installed catalogue, browsed with its own filters.
//!
//! The reference's Discover, read out of stremio-core's `CatalogWithFilters`
//! rather than remembered:
//!
//! - the catalogues are the installed ones whose required extras can all be
//!   given a value, in the order the addons are installed; the media core
//!   decides which those are and says which filters each one takes;
//! - the types are the types those catalogues are for, films first, then
//!   series, channels and TV, anything else after;
//! - choosing a type opens its first catalogue; choosing a catalogue opens it
//!   on its defaults (the first option of each required extra);
//! - a filter that is not required can also be "Tümü", which leaves it out;
//! - the next page is asked for with `skip` equal to everything loaded so
//!   far, and a page that comes back empty is the end of the catalogue.
//!
//! On the panel: the filters along the top, a grid of posters under them, and
//! the focused title's record beside the grid.

use std::collections::BTreeMap;

use crate::model::DiscoverCatalog;
use crate::state::Item;

/// Across the grid. Fixed rather than measured: the focus model is a grid, and
/// a grid that reflowed would move a title out from under the remote.
pub const COLUMNS: usize = 5;

/// stremio-core's `TYPE_PRIORITIES`: films, series, channels, TV, then the
/// rest, and "other" last of all.
fn priority(kind: &str) -> i32 {
    match kind {
        "movie" => 4,
        "series" => 3,
        "channel" => 2,
        "tv" => 1,
        "other" => i32::MIN,
        _ => 0,
    }
}

pub fn kind_label(kind: &str) -> String {
    match kind {
        "movie" => "Film".into(),
        "series" => "Dizi".into(),
        "channel" => "Kanal".into(),
        "tv" => "TV".into(),
        "anime" => "Anime".into(),
        "other" => "Diğer".into(),
        other => other.to_string(),
    }
}

/// A catalogue's name in the product's language, where it is one of the few
/// every addon uses; anything else as the addon wrote it.
fn catalog_label(name: &str) -> String {
    match name.trim() {
        "Popular" => "Popüler".into(),
        "Featured" => "Öne Çıkanlar".into(),
        "New" | "Latest" => "Yeni".into(),
        "Top" => "En İyiler".into(),
        other => other.to_string(),
    }
}

fn extra_label(name: &str) -> String {
    match name {
        "genre" => "Tür".into(),
        "year" => "Yıl".into(),
        "sort" => "Sıra".into(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

/// A value an extra can take, as it is shown. Genres are the catalogue's
/// English words and the product is Turkish; everything else is shown as the
/// addon wrote it.
fn option_label(extra: &str, value: &str) -> String {
    if extra == "genre" {
        crate::detail::genre_in_turkish(value)
    } else {
        value.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Zone {
    Filters,
    Grid,
}

/// One of the dropdowns along the top, as it is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub label: String,
    pub options: Vec<String>,
    pub selected: usize,
}

impl Filter {
    pub fn value(&self) -> String {
        self.options.get(self.selected).cloned().unwrap_or_default()
    }
}

/// What a page request is for.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub generation: u64,
    pub kind: String,
    pub id: String,
    pub addon_id: String,
    pub extra: BTreeMap<String, String>,
}

pub struct Discover {
    pub catalogs: Vec<DiscoverCatalog>,
    pub kind: String,
    /// The catalogue shown, as an index into `catalogs`.
    pub catalog: usize,
    /// The extras chosen, the required ones' defaults included.
    pub extra: BTreeMap<String, String>,

    pub items: Vec<Item>,
    loaded: usize,
    pub loading: bool,
    pub ended: bool,
    pub error: Option<String>,
    /// Bumped whenever what is shown changes, so a page for something the
    /// viewer has since changed away from is dropped rather than drawn.
    pub generation: u64,

    pub zone: Zone,
    pub filter: usize,
    /// The open dropdown's highlighted option.
    pub picker: Option<usize>,
    pub index: usize,
    /// A catalogue asked for by "Tümünü Gör" before the list of catalogues
    /// has arrived: opened as soon as it does.
    wanted: Option<(String, String, String)>,
}

impl Discover {
    pub fn new() -> Self {
        Self {
            catalogs: Vec::new(),
            kind: String::new(),
            catalog: 0,
            extra: BTreeMap::new(),
            items: Vec::new(),
            loaded: 0,
            loading: false,
            ended: false,
            error: None,
            generation: 0,
            zone: Zone::Grid,
            filter: 0,
            picker: None,
            index: 0,
            wanted: None,
        }
    }

    /// Opens a particular catalogue: the reference's "Tümünü Gör" on a board
    /// shelf. Returns its first page when the catalogues are known; otherwise
    /// it is opened when they arrive.
    pub fn show(&mut self, addon_id: &str, kind: &str, id: &str) -> Option<Request> {
        self.zone = Zone::Grid;
        self.picker = None;
        let found = self
            .catalogs
            .iter()
            .position(|c| c.addon_id == addon_id && c.kind == kind && c.id == id);
        match found {
            Some(index) => {
                self.wanted = None;
                if index == self.catalog && !self.items.is_empty() {
                    return None;
                }
                self.kind = kind.to_string();
                self.open_catalog(index)
            }
            None => {
                self.wanted = Some((addon_id.into(), kind.into(), id.into()));
                None
            }
        }
    }

    pub fn types(&self) -> Vec<String> {
        let mut kinds: Vec<String> = Vec::new();
        for catalog in &self.catalogs {
            if !kinds.contains(&catalog.kind) {
                kinds.push(catalog.kind.clone());
            }
        }
        kinds.sort_by_key(|kind| std::cmp::Reverse(priority(kind)));
        kinds
    }

    /// The catalogues of the type shown, as indices into `catalogs`.
    pub fn of_kind(&self) -> Vec<usize> {
        (0..self.catalogs.len())
            .filter(|index| self.catalogs[*index].kind == self.kind)
            .collect()
    }

    pub fn current(&self) -> Option<&DiscoverCatalog> {
        self.catalogs.get(self.catalog)
    }

    /// The media core's list of catalogues. Keeps what is shown if it is still
    /// there; otherwise opens the first type's first catalogue. Returns the
    /// first page to ask for, when one is owed.
    pub fn take_catalogs(&mut self, catalogs: Vec<DiscoverCatalog>) -> Option<Request> {
        let shown = self
            .current()
            .map(|c| (c.addon_id.clone(), c.kind.clone(), c.id.clone()));
        self.catalogs = catalogs;
        if let Some((addon, kind, id)) = self.wanted.take() {
            if self.catalogs.iter().any(|c| c.addon_id == addon && c.kind == kind && c.id == id) {
                return self.show(&addon, &kind, &id);
            }
        }
        if let Some((addon, kind, id)) = shown {
            if let Some(index) = self
                .catalogs
                .iter()
                .position(|c| c.addon_id == addon && c.kind == kind && c.id == id)
            {
                self.catalog = index;
                return None;
            }
        }
        let kind = self.types().into_iter().next()?;
        self.open_kind(kind)
    }

    fn open_kind(&mut self, kind: String) -> Option<Request> {
        self.kind = kind;
        let first = *self.of_kind().first()?;
        self.open_catalog(first)
    }

    fn open_catalog(&mut self, index: usize) -> Option<Request> {
        let catalog = self.catalogs.get(index)?;
        self.catalog = index;
        self.extra = catalog.defaults.clone();
        self.reload()
    }

    fn reload(&mut self) -> Option<Request> {
        // A page still on its way is for what was shown before; the new
        // generation is what drops it when it lands.
        self.generation += 1;
        self.loading = false;
        self.items.clear();
        self.loaded = 0;
        self.ended = false;
        self.error = None;
        self.index = 0;
        self.page()
    }

    /// The next page, if there is one and none is on its way.
    fn page(&mut self) -> Option<Request> {
        if self.loading || self.ended {
            return None;
        }
        let catalog = self.current()?;
        let mut extra = self.extra.clone();
        if self.loaded > 0 {
            if !catalog.pages {
                return None;
            }
            extra.insert("skip".into(), self.loaded.to_string());
        }
        let request = Request {
            generation: self.generation,
            kind: catalog.kind.clone(),
            id: catalog.id.clone(),
            addon_id: catalog.addon_id.clone(),
            extra,
        };
        self.loading = true;
        Some(request)
    }

    /// A page, if it is still a page of what is shown.
    pub fn take_page(&mut self, generation: u64, items: Vec<Item>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        if items.is_empty() {
            self.ended = true;
            return;
        }
        self.loaded += items.len();
        // A catalogue that pages can send a title again on the next page;
        // it is shown once.
        for item in items {
            if !self.items.iter().any(|held| held.id == item.id) {
                self.items.push(item);
            }
        }
        if !self.current().is_some_and(|c| c.pages) {
            self.ended = true;
        }
    }

    pub fn fail(&mut self, generation: u64, why: &str) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        self.error = Some(why.to_string());
    }

    /// Tries the page that failed again.
    pub fn retry(&mut self) -> Option<Request> {
        self.error.take()?;
        self.page()
    }

    /// The dropdowns along the top: type, catalogue, then the catalogue's own
    /// filters.
    pub fn filters(&self) -> Vec<Filter> {
        let mut filters = Vec::new();
        let types = self.types();
        filters.push(Filter {
            label: "İçerik".into(),
            options: types.iter().map(|kind| kind_label(kind)).collect(),
            selected: types.iter().position(|kind| *kind == self.kind).unwrap_or(0),
        });
        let of_kind = self.of_kind();
        filters.push(Filter {
            label: "Katalog".into(),
            options: of_kind
                .iter()
                .map(|index| {
                    let catalog = &self.catalogs[*index];
                    format!("{} · {}", catalog_label(&catalog.name), catalog.addon_name)
                })
                .collect(),
            selected: of_kind.iter().position(|index| *index == self.catalog).unwrap_or(0),
        });
        if let Some(catalog) = self.current() {
            for extra in &catalog.extra {
                let mut options: Vec<String> = Vec::new();
                if !extra.required {
                    options.push("Tümü".into());
                }
                options.extend(extra.options.iter().map(|value| option_label(&extra.name, value)));
                let chosen = self.extra.get(&extra.name);
                let selected = match chosen {
                    Some(value) => {
                        extra.options.iter().position(|o| o == value).unwrap_or(0)
                            + usize::from(!extra.required)
                    }
                    None => 0,
                };
                filters.push(Filter {
                    label: extra_label(&extra.name),
                    options,
                    selected,
                });
            }
        }
        filters
    }

    /// Chooses option `option` of filter `filter`. Returns the first page of
    /// what is now shown, when it changed.
    pub fn choose(&mut self, filter: usize, option: usize) -> Option<Request> {
        match filter {
            0 => {
                let kind = self.types().get(option)?.clone();
                if kind == self.kind {
                    return None;
                }
                self.open_kind(kind)
            }
            1 => {
                let index = *self.of_kind().get(option)?;
                if index == self.catalog {
                    return None;
                }
                self.open_catalog(index)
            }
            n => {
                let catalog = self.current()?;
                let extra = catalog.extra.get(n - 2)?.clone();
                let value = if extra.required {
                    extra.options.get(option).cloned()
                } else if option == 0 {
                    None
                } else {
                    extra.options.get(option - 1).cloned()
                };
                if extra.required && value.is_none() {
                    return None;
                }
                if self.extra.get(&extra.name) == value.as_ref() {
                    return None;
                }
                match value {
                    Some(value) => self.extra.insert(extra.name.clone(), value),
                    None => self.extra.remove(&extra.name),
                };
                self.reload()
            }
        }
    }

    pub fn focused(&self) -> Option<&Item> {
        if self.zone != Zone::Grid {
            return None;
        }
        self.items.get(self.index)
    }

    /// Moves the remote. Returns whether anything changed, and the next page
    /// when the remote has reached the last row.
    pub fn step(&mut self, dx: i32, dy: i32) -> (bool, Option<Request>) {
        if let Some(open) = self.picker {
            let count = self.filters().get(self.filter).map(|f| f.options.len()).unwrap_or(0);
            let next = (open as i32 + dy).clamp(0, count.saturating_sub(1) as i32) as usize;
            let moved = next != open && dy != 0;
            self.picker = Some(next);
            return (moved, None);
        }
        match self.zone {
            Zone::Filters => {
                if dy > 0 && !self.items.is_empty() {
                    self.zone = Zone::Grid;
                    return (true, None);
                }
                if dx != 0 {
                    let count = self.filters().len() as i32;
                    let next = (self.filter as i32 + dx).clamp(0, count - 1) as usize;
                    if next != self.filter {
                        self.filter = next;
                        return (true, None);
                    }
                }
                (false, None)
            }
            Zone::Grid => {
                let len = self.items.len();
                if len == 0 {
                    self.zone = Zone::Filters;
                    return (true, None);
                }
                let column = self.index % COLUMNS;
                let moved = if dy < 0 && self.index < COLUMNS {
                    self.zone = Zone::Filters;
                    true
                } else if dx != 0 {
                    // Left and right stay on their row.
                    let row_start = self.index - column;
                    let width = COLUMNS.min(len - row_start);
                    let next = (column as i32 + dx).clamp(0, width as i32 - 1) as usize;
                    let moved = row_start + next != self.index;
                    self.index = row_start + next;
                    moved
                } else if dy > 0 {
                    // Down into a shorter last row lands on its last title.
                    let next = self.index + COLUMNS;
                    let last_row_start = (len - 1) / COLUMNS * COLUMNS;
                    if next < len {
                        self.index = next;
                        true
                    } else if self.index < last_row_start {
                        self.index = len - 1;
                        true
                    } else {
                        false
                    }
                } else if dy < 0 {
                    self.index -= COLUMNS;
                    true
                } else {
                    false
                };
                // Within a row of the end, the next page: it is on the panel
                // by the time the remote gets there.
                let near_end = self.index + COLUMNS * 2 >= len;
                let request = if near_end { self.page() } else { None };
                (moved, request)
            }
        }
    }

    /// Ok on a filter opens its list, on the option it is showing.
    pub fn open_picker(&mut self) -> bool {
        if self.zone != Zone::Filters || self.picker.is_some() {
            return false;
        }
        let Some(filter) = self.filters().get(self.filter).cloned() else {
            return false;
        };
        if filter.options.len() < 2 {
            return false;
        }
        self.picker = Some(filter.selected);
        true
    }

    /// Ok in an open list: that option, and the list goes up.
    pub fn pick(&mut self) -> Option<Request> {
        let option = self.picker.take()?;
        self.choose(self.filter, option)
    }

    pub fn close_picker(&mut self) -> bool {
        self.picker.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DiscoverExtra;

    fn catalog(addon: &str, kind: &str, id: &str, genres: &[&str], pages: bool) -> DiscoverCatalog {
        DiscoverCatalog {
            addon_id: addon.into(),
            addon_name: addon.to_uppercase(),
            kind: kind.into(),
            id: id.into(),
            name: id.into(),
            pages,
            defaults: BTreeMap::new(),
            extra: if genres.is_empty() {
                Vec::new()
            } else {
                vec![DiscoverExtra {
                    name: "genre".into(),
                    required: false,
                    options: genres.iter().map(|g| g.to_string()).collect(),
                }]
            },
        }
    }

    fn items(prefix: &str, n: usize) -> Vec<Item> {
        (0..n).map(|i| Item::stub(&format!("{prefix}{i}"))).collect()
    }

    fn opened() -> (Discover, Request) {
        let mut discover = Discover::new();
        let request = discover
            .take_catalogs(vec![
                catalog("b", "series", "top", &[], true),
                catalog("a", "movie", "top", &["Action", "Drama"], true),
                catalog("c", "movie", "new", &[], false),
            ])
            .expect("a first page");
        (discover, request)
    }

    #[test]
    fn films_come_before_series_whatever_order_the_addons_are_in() {
        let (discover, request) = opened();
        assert_eq!(discover.types(), vec!["movie", "series"]);
        assert_eq!((request.addon_id.as_str(), request.kind.as_str(), request.id.as_str()), ("a", "movie", "top"));
        assert!(request.extra.is_empty(), "a catalogue opens on its defaults");
    }

    #[test]
    fn the_genre_filter_is_offered_only_by_a_catalogue_that_takes_one() {
        let (mut discover, _) = opened();
        let labels: Vec<String> = discover.filters().iter().map(|f| f.label.clone()).collect();
        assert_eq!(labels, vec!["İçerik", "Katalog", "Tür"]);
        assert_eq!(discover.filters()[2].options, vec!["Tümü", "Aksiyon", "Dram"]);
        discover.choose(1, 1);
        assert_eq!(discover.filters().len(), 2, "the second film catalogue takes no genre");
    }

    #[test]
    fn choosing_a_genre_asks_for_its_first_page_and_tumu_takes_it_away() {
        let (mut discover, _) = opened();
        discover.loading = false;
        let request = discover.choose(2, 2).expect("a new first page");
        assert_eq!(request.extra.get("genre").map(String::as_str), Some("Drama"));
        assert!(!request.extra.contains_key("skip"));
        discover.loading = false;
        let request = discover.choose(2, 0).expect("the catalogue without the genre");
        assert!(request.extra.is_empty());
    }

    #[test]
    fn a_new_type_opens_its_first_catalogue() {
        let (mut discover, _) = opened();
        discover.loading = false;
        let request = discover.choose(0, 1).expect("the series catalogue");
        assert_eq!((request.kind.as_str(), request.addon_id.as_str()), ("series", "b"));
    }

    #[test]
    fn the_next_page_skips_what_is_loaded_and_an_empty_page_ends_it() {
        let (mut discover, first) = opened();
        discover.take_page(first.generation, items("p1-", 12));
        discover.zone = Zone::Grid;
        let mut asked = None;
        for _ in 0..3 {
            let (_, request) = discover.step(0, 1);
            asked = asked.or(request);
        }
        let second = asked.expect("the second page, as the remote nears the end");
        assert_eq!(second.extra.get("skip").map(String::as_str), Some("12"));
        discover.take_page(second.generation, Vec::new());
        assert!(discover.ended);
        assert_eq!(discover.step(0, 1).1, None, "nothing after the end");
    }

    #[test]
    fn a_page_of_what_the_viewer_changed_away_from_is_dropped() {
        let (mut discover, first) = opened();
        discover.loading = false;
        discover.choose(2, 1);
        discover.take_page(first.generation, items("stale-", 5));
        assert!(discover.items.is_empty());
    }

    #[test]
    fn a_catalogue_that_does_not_page_is_one_page() {
        let (mut discover, _) = opened();
        discover.loading = false;
        let request = discover.choose(1, 1).unwrap();
        discover.take_page(request.generation, items("n", 20));
        assert!(discover.ended);
    }

    #[test]
    fn a_title_sent_again_on_the_next_page_is_shown_once() {
        let (mut discover, first) = opened();
        discover.take_page(first.generation, items("x", 3));
        discover.take_page(first.generation, items("x", 4));
        assert_eq!(discover.items.len(), 4);
    }

    #[test]
    fn up_from_the_first_row_is_the_filters_and_down_comes_back() {
        let (mut discover, first) = opened();
        discover.take_page(first.generation, items("g", 12));
        discover.zone = Zone::Grid;
        discover.step(1, 0);
        assert!(discover.step(0, -1).0);
        assert_eq!(discover.zone, Zone::Filters);
        assert!(discover.step(0, 1).0);
        assert_eq!((discover.zone.clone(), discover.index), (Zone::Grid, 1));
    }

    #[test]
    fn down_into_a_short_last_row_lands_on_its_last_title() {
        let (mut discover, first) = opened();
        discover.take_page(first.generation, items("g", 7));
        discover.ended = true;
        discover.zone = Zone::Grid;
        discover.index = 4;
        assert!(discover.step(0, 1).0);
        assert_eq!(discover.index, 6);
    }

    #[test]
    fn the_picker_opens_on_what_is_chosen_and_back_closes_it_unchanged() {
        let (mut discover, _) = opened();
        discover.zone = Zone::Filters;
        discover.filter = 2;
        assert!(discover.open_picker());
        assert_eq!(discover.picker, Some(0));
        discover.step(0, 1);
        assert!(discover.close_picker());
        assert!(!discover.extra.contains_key("genre"));
    }

    #[test]
    fn a_failed_page_can_be_tried_again() {
        let (mut discover, first) = opened();
        discover.fail(first.generation, "zaman aşımı");
        assert!(discover.error.is_some());
        let again = discover.retry().expect("the same page");
        assert_eq!(again.extra, first.extra);
    }

    #[test]
    fn see_all_opens_that_catalogue_whenever_the_list_arrives() {
        let mut discover = Discover::new();
        assert!(discover.show("c", "movie", "new").is_none(), "nothing known yet");
        let request = discover
            .take_catalogs(vec![
                catalog("a", "movie", "top", &[], true),
                catalog("c", "movie", "new", &[], false),
            ])
            .expect("the wanted catalogue's first page");
        assert_eq!((request.addon_id.as_str(), request.id.as_str()), ("c", "new"));
        assert_eq!(discover.filters()[1].selected, 1);
    }
}
