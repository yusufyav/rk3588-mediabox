//! Where the remote is, and what that means for what is drawn.
//!
//! The focus model is structural rather than geometric. The web interface
//! scores candidates by distance because the DOM is the only description of the
//! screen it has; here the screen *is* rows of items, so the position is a row
//! and a column and moving is arithmetic. That has three properties worth more
//! than cleverness on a television: focus cannot be lost, the same press always
//! does the same thing, and Back can restore a position exactly rather than by
//! looking for a landmark that may not have loaded yet.
//!
//! Each row remembers its own column, which is what makes moving down a shelf
//! and back up again land where it started.

use std::rc::Rc;

use slint::{Model, ModelRc, VecModel};

use crate::images::{ImageManager, Key};
use crate::model::{LibraryListing, MetaPreview};
use crate::{AppTile, PosterItem, RailModel};

/// How far either side of the focused poster is worth having ready.
///
/// Asymmetric was wrong, and the reason is where the rail puts the focus. It
/// pins the focused poster to the left edge and scrolls the strip under it --
/// so everything visible is ahead of the focus, and two behind was plenty --
/// *until the end of the strip*, where the scroll clamps and the focus walks
/// rightwards across a stationary rail instead. At the far end of a long
/// catalogue the focus sits at the right of the screen with seven posters
/// visible to its left, of which five were outside the window and had their
/// artwork taken away: the row emptied itself as the viewer arrived at it.
///
/// About eight posters fit across a 16:9 panel at this card width, so the
/// window is eight either way. That is one screenful behind and one ahead,
/// which is what "nearly visible" means on a rail that can scroll both ways.
const BEHIND: usize = 8;
const AHEAD: usize = 8;

/// Shelves are capped so one addon with forty catalogues cannot make the home
/// screen its own. The media core caps them too; this is the interface saying
/// the same thing about its own memory.
const RAIL_LIMIT: usize = 20;

/// The one row above the shelves: the bar.
///
/// The launcher used to be the second, between the hero and the catalogue,
/// which made the box's applications the first thing a viewer met and the
/// films the second. It is a shelf of its own at the bottom now — still in
/// reach from the sofa, no longer in front of the product.
const SHELF_ROW: usize = 1;

/// The width posters are decoded at. The card is about 238 logical pixels wide
/// at the 1920-pixel design, and twice that on a 4K panel at integer scale two.
pub const POSTER_WIDTH: u32 = 480;
/// Backdrops are decoded once and drawn across the panel. The hosts serve about
/// a thousand pixels; nothing is upscaled past what arrives.
pub const BACKDROP_WIDTH: u32 = 1920;

/// What is on the bar at the top of the *catalogue*, not of the home screen.
///
/// Searching for a film and browsing a library are things one does inside the
/// catalogue application, the way the reference does them: its own bar, its own
/// screens, behind its own tile. They were on the home screen's bar for one
/// build and that was wrong — the home screen is a launcher, and a launcher
/// with a film search box on it is a launcher pretending to be a video shop.
///
/// The home screen's bar carries the product's name, whether the box is on the
/// network, and the time. None of those is focusable: they are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Search,
    Library,
    Settings,
}

pub const NAV: [Nav; 2] = [Nav::Search, Nav::Library];

impl Nav {
    pub fn label(self) -> &'static str {
        match self {
            Nav::Search => "Ara",
            Nav::Library => "Kitaplık",
            Nav::Settings => "Ayarlar",
        }
    }
}

/// What pressing Ok on a launcher tile does.
#[derive(Debug, Clone, PartialEq)]
pub enum AppAction {
    /// Move the remote down to the catalogue shelves. The web interface opens a
    /// screen of its own for this; here the shelves are already on the panel,
    /// so the honest thing is to go to them rather than to redraw them.
    Shelves,
    /// Hand the television to another application. This interface is one of
    /// them, so doing it ends this process.
    Launch(String),
    /// One of this interface's own screens.
    Screen(Nav),
    /// The screen this tile would open has not been built yet. The tile is
    /// still drawn — what the box can do should be visible from the sofa — but
    /// it says so rather than doing nothing when it is pressed.
    Absent,
}

/// One launcher tile, decided in Rust.
///
/// The mark and the hue come from the same table the web interface draws from,
/// so an application is the same colour and the same shape on the television as
/// it is on a phone.
#[derive(Debug, Clone, PartialEq)]
pub struct AppEntryTile {
    pub id: String,
    pub name: String,
    pub mark: &'static str,
    pub hue: f32,
    pub ready: bool,
    pub note: String,
    pub action: AppAction,
}

/// A mark per application, drawn rather than fetched.
///
/// An appliance has no icon theme to ask and no network to depend on, so each
/// application is a stroked path in a 24x24 box and one hue. Lifted from the
/// web interface's own table: an application nobody has registered a mark for
/// still gets a proper tile rather than an empty box.
fn app_mark(id: &str) -> (&'static str, f32) {
    match id {
        "media" => (
            "M3.6 4.6h16.8v14.8H3.6V4.6ZM3.6 9.2h16.8M8.2 4.6v4.6M15.8 4.6v4.6M10.2 12.4v4.2l4-2.1-4-2.1Z",
            340.0,
        ),
        "kodi" => (
            "M12 3.4a8.6 8.6 0 1 0 0 17.2 8.6 8.6 0 0 0 0-17.2ZM10.3 8.5v7l5.9-3.5-5.9-3.5Z",
            198.0,
        ),
        "browser" => (
            "M12 3.2a8.8 8.8 0 1 0 0 17.6 8.8 8.8 0 0 0 0-17.6ZM3.2 12h17.6M12 3.2c2.3 2.4 3.5 5.4 3.5 8.8s-1.2 6.4-3.5 8.8c-2.3-2.4-3.5-5.4-3.5-8.8S9.7 5.6 12 3.2Z",
            32.0,
        ),
        "settings" => (
            "M12 9.4a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2ZM12 3.4l1.4 2.2 2.6-.5.6 2.6 2.2 1.3L19.6 12l1.2 2.4-2.2 1.3-.6 2.6-2.6-.5-1.4 2.2-1.4-2.2-2.6.5-.6-2.6-2.2-1.3L8.4 12 7.2 9.6l2.2-1.3.6-2.6 2.6.5L12 3.4Z",
            150.0,
        ),
        "stremio" => (
            "M12 3.6 4.4 8v8l7.6 4.4L19.6 16V8L12 3.6ZM12 3.6v16.8M4.4 8l7.6 4.4L19.6 8",
            262.0,
        ),
        "screenbridge" => (
            "M3.4 5.6h17.2v10.2H3.4V5.6ZM8.6 19.6h6.8M3.4 5.6 12 11l8.6-5.4",
            268.0,
        ),
        _ => (
            "M4.4 4.4h6v6h-6v-6ZM13.6 4.4h6v6h-6v-6ZM4.4 13.6h6v6h-6v-6ZM13.6 13.6h6v6h-6v-6Z",
            212.0,
        ),
    }
}

fn tile(id: &str, name: &str, ready: bool, note: &str, action: AppAction) -> AppEntryTile {
    let (mark, hue) = app_mark(id);
    AppEntryTile {
        id: id.to_string(),
        name: name.to_string(),
        mark,
        hue,
        ready,
        note: note.to_string(),
        action,
    }
}

/// The launcher, assembled the way the web interface assembles it: this
/// interface's own screens first, then whatever else the box can run.
///
/// The filter is the web interface's too. The television's own unit is where
/// you already are, so a tile for it would reopen what you are looking at; the
/// browser is on this screen as its own tile already; and whatever currently
/// owns the display is not something to be handed the display again.
pub fn app_tiles_from(display: &crate::model::DisplayStatus) -> Vec<AppEntryTile> {
    let here = display.owner.as_deref();
    let mut tiles = vec![
        tile("media", "Filmler ve Diziler", true, "", AppAction::Shelves),
        tile(
            "browser",
            "Tarayıcı",
            true,
            "",
            AppAction::Launch("browser".to_string()),
        ),
        tile(
            "settings",
            "Ayarlar",
            true,
            "",
            AppAction::Screen(Nav::Settings),
        ),
    ];

    tiles.extend(
        display
            .applications
            .iter()
            .filter(|entry| {
                entry.id != "mediabox" && entry.id != "browser" && here != Some(entry.id.as_str())
            })
            .map(|entry| {
                let name = if entry.name.is_empty() {
                    entry.id.clone()
                } else {
                    entry.name.clone()
                };
                tile(
                    &entry.id,
                    &name,
                    entry.installed,
                    if entry.installed { "" } else { "kurulu değil" },
                    AppAction::Launch(entry.id.clone()),
                )
            }),
    );

    tiles
}

#[derive(Clone)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub poster: Option<String>,
    pub background: Option<String>,
    pub summary: Option<String>,
    pub year: Option<String>,
    pub rating: Option<String>,
    pub genres: Vec<String>,
    pub progress: f32,
    pub local: bool,
    pub hue: f32,
}

/// An addon that has no answer sends an empty string as often as it omits the
/// field. Treating the two differently means a hero with a backdrop URL of ""
/// and nothing behind it.
fn some(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Item {
    /// A title with nothing but a name. Only the focus-model tests use it:
    /// where the remote goes must not depend on what an addon sent.
    #[cfg(test)]
    pub fn stub(id: &str) -> Self {
        Self {
            id: id.to_string(),
            kind: "movie".into(),
            title: id.to_string(),
            poster: None,
            background: None,
            summary: None,
            year: None,
            rating: None,
            genres: Vec::new(),
            progress: 0.0,
            local: false,
            hue: 0.0,
        }
    }

    pub fn from_preview(preview: &MetaPreview) -> Self {
        let progress = preview.state.as_ref().map(|s| s.progress()).unwrap_or(0.0) as f32;
        Self {
            hue: hue_of(&preview.name),
            id: preview.id.clone(),
            kind: preview.kind.clone(),
            title: preview.name.clone(),
            poster: some(&preview.poster),
            background: some(&preview.background),
            summary: some(&preview.description),
            year: some(&preview.release_info),
            rating: some(&preview.imdb_rating),
            genres: preview.genres.clone(),
            progress,
            local: preview.is_library(),
        }
    }
}

#[derive(Clone)]
pub struct Shelf {
    pub title: String,
    pub source: String,
    pub items: Vec<Item>,
}

/// The whole home surface, and the remote's place in it.
/// The home surface, and the remote's place in it.
///
/// Two rows, and the order is the product's argument about what this box is:
/// what the box can be used for, and then the one shelf that belongs on an
/// appliance's front page — what somebody had not finished watching.
///
/// The catalogue is not here. It is behind the "Filmler ve Diziler" tile, on
/// the library screen, which is where a catalogue belongs: MediaBox is an
/// environment for this board rather than a player with a menu, and a home
/// screen printed full of posters is a video shop's front page with an
/// operating system behind it.
pub struct Home {
    /// Everything the media core answered with. Not drawn here — the library
    /// screen is built from it — but held here because this is where it lands.
    pub shelves: Vec<Shelf>,
    /// The one shelf this screen draws: titles left part-way through.
    pub recent: Vec<Item>,
    /// What this box can be used for.
    pub apps: Vec<AppEntryTile>,

    /// Row 0 is the launcher, row 1 the unfinished shelf.
    pub row: usize,
    /// One remembered column per row.
    columns: Vec<usize>,

    /// The Slint side of the same thing, kept beside the data rather than
    /// rebuilt from it, so a picture arriving changes one tile rather than the
    /// whole shelf.
    tiles: Rc<VecModel<PosterItem>>,
    pub app_tiles: Rc<VecModel<AppTile>>,
}

/// The row the unfinished shelf is on, when there is one.
const RECENT_ROW: usize = 1;

impl Home {
    pub fn new() -> Self {
        Self {
            shelves: Vec::new(),
            recent: Vec::new(),
            apps: Vec::new(),
            // The launcher. The web interface autofocuses its first application
            // tile and so does this: the first thing a person meets is what the
            // box can do.
            row: 0,
            columns: vec![0; 2],
            tiles: Rc::new(VecModel::default()),
            app_tiles: Rc::new(VecModel::default()),
        }
    }

    pub fn column(&self) -> usize {
        self.columns.get(self.row).copied().unwrap_or(0)
    }

    /// The shelf's Slint model, for the window to hold.
    pub fn recent_tiles(&self) -> Rc<VecModel<PosterItem>> {
        self.tiles.clone()
    }

    /// The row the unfinished shelf is on, or none when nothing is unfinished.
    pub fn recent_row(&self) -> Option<usize> {
        (!self.recent.is_empty()).then_some(RECENT_ROW)
    }

    fn row_len(&self, row: usize) -> usize {
        match row {
            0 => self.apps.len(),
            _ if Some(row) == self.recent_row() => self.recent.len(),
            _ => 0,
        }
    }

    pub fn rows(&self) -> usize {
        RECENT_ROW + usize::from(!self.recent.is_empty())
    }

    /// The focused title, when the remote is on the shelf.
    pub fn focused(&self) -> Option<&Item> {
        (Some(self.row) == self.recent_row()).then(|| self.recent.get(self.column()))?
    }

    /// The focused application, when the remote is on the launcher.
    pub fn focused_app(&self) -> Option<&AppEntryTile> {
        (self.row == 0).then(|| self.apps.get(self.column()))?
    }

    /// Replaces the launcher's tiles, keeping the remote on the same
    /// application if it is still there — the list is re-read every few
    /// seconds, and focus that jumped each time would be unusable.
    pub fn set_apps(&mut self, apps: Vec<AppEntryTile>) {
        let holding = self.focused_app().map(|tile| tile.id.clone());
        self.apps = apps;

        self.app_tiles.set_vec(
            self.apps
                .iter()
                .map(|tile| AppTile {
                    id: tile.id.clone().into(),
                    name: tile.name.clone().into(),
                    mark: tile.mark.into(),
                    hue: tile.hue,
                    ready: tile.ready,
                    note: tile.note.clone().into(),
                })
                .collect::<Vec<_>>(),
        );

        if self.row == 0 {
            let landed = holding
                .and_then(|id| self.apps.iter().position(|tile| tile.id == id))
                .unwrap_or_else(|| self.column().min(self.apps.len().saturating_sub(1)));
            self.columns[0] = landed;
        }
    }

    pub fn step(&mut self, dx: isize, dy: isize) -> bool {
        let mut moved = false;

        if dy != 0 {
            let mut row = self.row as isize + dy;
            // A row with nothing on it is stepped over rather than landed on.
            while row >= 0 && (row as usize) < self.rows() && self.row_len(row as usize) == 0 {
                row += dy;
            }
            if row >= 0 && (row as usize) < self.rows() {
                self.row = row as usize;
                moved = true;
            }
        }

        if dx != 0 {
            let len = self.row_len(self.row);
            if len > 0 {
                let current = self.column() as isize;
                let next = (current + dx).clamp(0, len as isize - 1);
                if next != current {
                    self.columns[self.row] = next as usize;
                    moved = true;
                }
            }
        }

        // A row's remembered column may be past the end of a shorter row.
        let len = self.row_len(self.row);
        if len > 0 {
            let column = self.column().min(len - 1);
            self.columns[self.row] = column;
        }

        moved
    }

    /// Takes the whole catalogue, and keeps the part of it this screen draws.
    pub fn set_shelves(&mut self, shelves: Vec<Shelf>) {
        self.shelves = shelves;

        // Unfinished titles, wherever they came from. The media core labels a
        // shelf "Devam Et", but the property that matters is the watch state,
        // not the shelf's name.
        let mut seen = std::collections::HashSet::new();
        self.recent = self
            .shelves
            .iter()
            .flat_map(|shelf| shelf.items.iter())
            .filter(|item| item.progress > 0.0 && item.progress < 0.95)
            .filter(|item| seen.insert(item.id.clone()))
            .take(RAIL_LIMIT)
            .cloned()
            .collect();

        self.tiles.set_vec(
            self.recent
                .iter()
                .map(|item| PosterItem {
                    id: item.id.clone().into(),
                    kind: item.kind.clone().into(),
                    title: item.title.clone().into(),
                    subtitle: item.year.clone().unwrap_or_default().into(),
                    art: slint::Image::default(),
                    hue: item.hue,
                    progress: item.progress,
                    local: item.local,
                })
                .collect::<Vec<_>>(),
        );

        self.columns.resize(2, 0);
        if self.row_len(self.row) == 0 {
            self.row = 0;
        }
    }

    /// Asks for the pictures this screen is showing and about to show, and
    /// hands over whatever has arrived. Everything outside the window is given
    /// an empty image, which is what keeps the catalogue off the GPU.
    pub fn sync_artwork(&mut self, images: &mut ImageManager) {
        if self.recent.is_empty() {
            return;
        }
        let centre = self.columns.get(RECENT_ROW).copied().unwrap_or(0);
        let last = self.recent.len() - 1;
        let from = centre.saturating_sub(BEHIND);
        let to = (centre + AHEAD).min(last);

        for (column, item) in self.recent.iter().enumerate() {
            let Some(mut tile) = self.tiles.row_data(column) else {
                continue;
            };

            let wanted = (column >= from && column <= to)
                .then(|| {
                    item.poster
                        .as_deref()
                        .map(|url| Key::new(url, POSTER_WIDTH))
                })
                .flatten();

            let art = match &wanted {
                Some(key) => {
                    images.want(key);
                    images.get(key).unwrap_or_default()
                }
                None => slint::Image::default(),
            };

            // Only write back when it actually changed: set_row_data is what
            // makes Slint redraw the tile.
            let had = tile.art.size().width > 0;
            let has = art.size().width > 0;
            if had != has {
                tile.art = art;
                self.tiles.set_row_data(column, tile);
            }
        }
    }
}

/// The shelves the home screen is made of, in the order the product shows them.
///
/// The appliance's own library and the operator's account come first because
/// they are the two answers to "what was I doing", and they are local or nearly
/// so; the catalogues follow in the order the media core returned them.
pub fn shelves_from(home: &crate::model::HomeRows, library: Option<&LibraryListing>) -> Vec<Shelf> {
    let mut shelves = Vec::new();

    if let Some(library) = library {
        let unfinished: Vec<Item> = library
            .stremio
            .iter()
            .filter(|item| item.state.as_ref().map(|s| s.unfinished()).unwrap_or(false))
            .take(RAIL_LIMIT)
            .map(Item::from_preview)
            .collect();
        if !unfinished.is_empty() {
            shelves.push(Shelf {
                title: "Devam Et".into(),
                source: String::new(),
                items: unfinished,
            });
        }

        let mine: Vec<Item> = library
            .stremio
            .iter()
            .filter(|item| !item.state.as_ref().map(|s| s.unfinished()).unwrap_or(false))
            .take(RAIL_LIMIT)
            .map(Item::from_preview)
            .collect();
        if !mine.is_empty() {
            shelves.push(Shelf {
                title: "Kitaplığım".into(),
                source: "Stremio hesabın".into(),
                items: mine,
            });
        }

        let local: Vec<Item> = library
            .items
            .iter()
            .take(RAIL_LIMIT)
            .map(Item::from_preview)
            .collect();
        if !local.is_empty() {
            shelves.push(Shelf {
                title: "Kitaplık".into(),
                source: "Bu cihazda".into(),
                items: local,
            });
        }
    }

    for row in &home.rows {
        if row.addon_id == crate::model::LIBRARY_ADDON_ID || row.items.is_empty() {
            continue;
        }
        shelves.push(Shelf {
            title: rail_title(&row.name, &row.kind),
            source: row.addon_name.clone(),
            items: row
                .items
                .iter()
                .take(RAIL_LIMIT)
                .map(Item::from_preview)
                .collect(),
        });
    }

    shelves
}

/// An addon's catalogue names are English and terse — "Popular", "New" — and
/// the product is Turkish. Anything not recognised keeps its own name with the
/// provider's word in front of it.
fn rail_title(name: &str, kind: &str) -> String {
    let noun = if kind == "series" {
        "Diziler"
    } else {
        "Filmler"
    };
    match name.trim() {
        "Popular" => format!("Popüler {noun}"),
        "Featured" => format!("Öne Çıkan {noun}"),
        "New" | "Latest" => format!("Yeni {noun}"),
        other if other.is_empty() => noun.to_string(),
        other => format!("{noun} · {other}"),
    }
}

/// A stable colour for a title, so the same film's fallback tile is the same
/// colour wherever it turns up. The web interface uses this hash; matching it
/// means the two surfaces agree about what a film looks like before its poster
/// arrives.
fn hue_of(name: &str) -> f32 {
    let hash = name.bytes().fold(17u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as u32)
    });
    (hash % 360) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(name: &str, count: usize, progress: f32) -> Shelf {
        Shelf {
            title: name.into(),
            source: String::new(),
            items: (0..count)
                .map(|i| {
                    let mut item = Item::stub(&format!("{name}-{i}"));
                    item.progress = progress;
                    item
                })
                .collect(),
        }
    }

    fn launcher() -> Vec<AppEntryTile> {
        vec![
            tile("media", "Filmler ve Diziler", true, "", AppAction::Shelves),
            tile(
                "browser",
                "Tarayıcı",
                true,
                "",
                AppAction::Launch("browser".into()),
            ),
            tile(
                "settings",
                "Ayarlar",
                true,
                "",
                AppAction::Screen(Nav::Settings),
            ),
        ]
    }

    /// The home screen opens on the launcher. MediaBox is an environment for
    /// this board rather than a player with a menu.
    #[test]
    fn focus_starts_on_the_launcher() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.set_shelves(vec![shelf("a", 5, 0.4)]);
        assert_eq!(home.row, 0);
        assert!(home.focused_app().is_some());
        assert!(home.focused().is_none());
    }

    /// The catalogue is not printed onto the home screen. It is behind a tile.
    #[test]
    fn the_catalogue_is_not_on_the_home_screen() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.set_shelves(vec![
            shelf("finished", 4, 0.0),
            shelf("popular", 20, 0.0),
            shelf("half", 3, 0.5),
        ]);
        // Two rows at most: the launcher, and what was left unfinished.
        // Twenty-seven titles arrived; three are on this screen.
        assert_eq!(home.rows(), 2);
        assert_eq!(home.recent.len(), 3);
        assert_eq!(
            home.shelves.len(),
            3,
            "the catalogue is still held for the library"
        );

        let media = home
            .apps
            .iter()
            .find(|t| t.id == "media")
            .expect("the tile");
        assert_eq!(media.action, AppAction::Shelves);
    }

    #[test]
    fn nothing_unfinished_means_one_row() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.set_shelves(vec![shelf("popular", 6, 0.0)]);
        assert_eq!(home.rows(), 1);
        assert_eq!(home.recent_row(), None);
        for _ in 0..5 {
            home.step(0, 1);
        }
        assert_eq!(home.row, 0, "there is nowhere below the launcher to go");
    }

    #[test]
    fn a_finished_title_is_not_unfinished() {
        let mut home = Home::new();
        home.set_shelves(vec![shelf("credits", 3, 0.98)]);
        assert!(home.recent.is_empty());
    }

    #[test]
    fn the_same_title_on_two_shelves_is_one_tile() {
        let mut home = Home::new();
        let mut one = shelf("a", 1, 0.5);
        let two = Shelf {
            title: "b".into(),
            source: String::new(),
            items: one.items.clone(),
        };
        one.items[0].progress = 0.5;
        home.set_shelves(vec![one, two]);
        assert_eq!(home.recent.len(), 1);
    }

    /// The launcher is the top of this screen and there is nothing above it.
    /// Searching for a film is not a thing one does on a launcher; it is a
    /// thing one does inside the catalogue.
    #[test]
    fn the_launcher_is_the_top_and_the_shelf_is_the_bottom() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.set_shelves(vec![shelf("a", 3, 0.3)]);
        for _ in 0..8 {
            home.step(0, -1);
        }
        assert_eq!(home.row, 0);
        assert!(home.focused_app().is_some());
        for _ in 0..8 {
            home.step(0, 1);
        }
        assert_eq!(home.row, RECENT_ROW);
        assert!(home.focused().is_some());
    }

    #[test]
    fn each_row_remembers_its_own_column() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.set_shelves(vec![shelf("a", 8, 0.4)]);
        home.step(1, 0);
        home.step(1, 0);
        assert_eq!(home.column(), 2);
        home.step(0, 1);
        assert_eq!(home.column(), 0);
        home.step(0, -1);
        assert_eq!(home.column(), 2);
    }

    #[test]
    fn a_relaunched_list_keeps_the_remote_on_the_same_application() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.step(1, 0);
        let id = home.focused_app().unwrap().id.clone();
        let mut shuffled = launcher();
        shuffled.insert(
            0,
            tile(
                "kodi",
                "Oynatıcı",
                true,
                "",
                AppAction::Launch("kodi".into()),
            ),
        );
        home.set_apps(shuffled);
        assert_eq!(home.focused_app().unwrap().id, id);
    }

    #[test]
    fn the_column_never_points_past_a_shorter_row() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.set_shelves(vec![shelf("a", 2, 0.4)]);
        home.row = 0;
        for _ in 0..8 {
            home.step(1, 0);
        }
        home.step(0, 1);
        assert!(home.column() < 2);
        assert!(home.focused().is_some());
    }

    /// Settings has a screen now; the launcher must not still say "yakında".
    #[test]
    fn the_launcher_offers_the_screens_this_interface_has() {
        let display = crate::model::DisplayStatus {
            owner: None,
            applications: Vec::new(),
        };
        let tiles = app_tiles_from(&display);
        let settings = tiles
            .iter()
            .find(|t| t.id == "settings")
            .expect("settings tile");
        assert!(settings.ready);
        assert_eq!(settings.action, AppAction::Screen(Nav::Settings));

        let media = tiles
            .iter()
            .find(|t| t.id == "media")
            .expect("catalogue tile");
        assert_eq!(media.name, "Filmler ve Diziler");
        assert!(media.ready);
    }
}
