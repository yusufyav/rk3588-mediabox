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

/// How far either side of the focused poster is worth having ready. Measured
/// values from the web interface, where a clipped strip gave the browser no
/// "nearly visible" to work from and every step right landed on a blank tile.
const BEHIND: usize = 2;
const AHEAD: usize = 8;

/// Shelves are capped so one addon with forty catalogues cannot make the home
/// screen its own. The media core caps them too; this is the interface saying
/// the same thing about its own memory.
const RAIL_LIMIT: usize = 20;

/// The rows above the shelves: the bar, and then the launcher.
///
/// Named rather than written as `- 1` in six places, because it changed once —
/// the launcher was added between them — and every one of those places had to
/// be found by hand.
const SHELF_ROW: usize = 2;

/// The width posters are decoded at. The card is about 238 logical pixels wide
/// at the 1920-pixel design, and twice that on a 4K panel at integer scale two.
pub const POSTER_WIDTH: u32 = 480;
/// Backdrops are decoded once and drawn across the panel. The hosts serve about
/// a thousand pixels; nothing is upscaled past what arrives.
pub const BACKDROP_WIDTH: u32 = 1920;

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
        // The settings screen is not written yet. Shown and marked rather than
        // left out: a launcher that silently lacks a screen the other interface
        // has is a launcher nobody can trust.
        tile("settings", "Ayarlar", false, "yakında", AppAction::Absent),
    ];

    tiles.extend(
        display
            .applications
            .iter()
            .filter(|entry| {
                entry.id != "mediabox"
                    && entry.id != "browser"
                    && here != Some(entry.id.as_str())
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
    value.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

impl Item {
    fn from_preview(preview: &MetaPreview) -> Self {
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

pub struct Shelf {
    pub title: String,
    pub source: String,
    pub items: Vec<Item>,
}

/// The whole home surface, and the remote's place in it.
pub struct Home {
    pub shelves: Vec<Shelf>,
    /// What this box can be used for. The control plane's applications and the
    /// interface's own screens, in the order they are drawn.
    pub apps: Vec<AppEntryTile>,
    /// Row 0 is the bar, row 1 is the launcher; 2.. are the shelves.
    pub row: usize,
    /// One remembered column per row, including the bar.
    columns: Vec<usize>,

    /// The Slint side of the same thing. Kept beside the data rather than
    /// rebuilt from it, so a picture arriving changes one item rather than
    /// every shelf on the screen.
    pub rails: Rc<VecModel<RailModel>>,
    tiles: Vec<Rc<VecModel<PosterItem>>>,
    /// The launcher's Slint side, kept beside `apps` for the same reason the
    /// rails are: an application changing state redraws one tile.
    pub app_tiles: Rc<VecModel<AppTile>>,

    /// Which backdrop layer is showing, so a new one fades in over the old.
    pub fade: f32,
    backdrop: Option<String>,
}

impl Home {
    pub fn new() -> Self {
        Self {
            shelves: Vec::new(),
            apps: Vec::new(),
            // The launcher, because that is what this screen is for: the web
            // interface autofocuses its first application tile and so does
            // this.
            row: 1,
            columns: vec![0; SHELF_ROW],
            rails: Rc::new(VecModel::default()),
            tiles: Vec::new(),
            app_tiles: Rc::new(VecModel::default()),
            fade: 0.0,
            backdrop: None,
        }
    }

    pub fn nav_len(&self) -> usize {
        4
    }

    pub fn column(&self) -> usize {
        self.columns.get(self.row).copied().unwrap_or(0)
    }

    fn row_len(&self, row: usize) -> usize {
        match row {
            0 => self.nav_len(),
            1 => self.apps.len(),
            _ => self.shelves.get(row - SHELF_ROW).map(|s| s.items.len()).unwrap_or(0),
        }
    }

    pub fn rows(&self) -> usize {
        SHELF_ROW + self.shelves.len()
    }

    /// The focused title, which is what the hero shows.
    pub fn focused(&self) -> Option<&Item> {
        let shelf = self.shelves.get(self.row.checked_sub(SHELF_ROW)?)?;
        shelf.items.get(self.column())
    }

    /// The first shelf that has anything on it, as a row number.
    ///
    /// Shelves that arrived empty are not drawn, so "the catalogue" is the
    /// first one that is.
    pub fn first_shelf_row(&self) -> Option<usize> {
        (SHELF_ROW..self.rows()).find(|row| self.row_len(*row) > 0)
    }

    /// The focused application, when the remote is on the launcher.
    pub fn focused_app(&self) -> Option<&AppEntryTile> {
        (self.row == 1).then(|| self.apps.get(self.column()))?
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

        if self.row == 1 {
            let landed = holding
                .and_then(|id| self.apps.iter().position(|tile| tile.id == id))
                .unwrap_or_else(|| self.column().min(self.apps.len().saturating_sub(1)));
            self.columns[1] = landed;
        }
    }

    pub fn step(&mut self, dx: isize, dy: isize) -> bool {
        let mut moved = false;

        if dy != 0 {
            let mut row = self.row as isize + dy;
            // Rows that arrived empty are stepped over rather than landed on:
            // a shelf with nothing in it is not drawn, and focus on a thing
            // that is not on the panel is focus nobody can see.
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

    /// Rebuilds the shelves and the models behind them, keeping the remote
    /// where it was if that position still exists.
    pub fn set_shelves(&mut self, shelves: Vec<Shelf>) {
        self.shelves = shelves;
        self.columns.resize(self.rows(), 0);
        self.columns.truncate(self.rows());

        self.tiles.clear();
        let mut rails: Vec<RailModel> = Vec::with_capacity(self.shelves.len());

        for shelf in &self.shelves {
            let tiles: Vec<PosterItem> = shelf
                .items
                .iter()
                .map(|item| PosterItem {
                    id: item.id.clone().into(),
                    kind: item.kind.clone().into(),
                    title: item.title.clone().into(),
                    art: slint::Image::default(),
                    hue: item.hue,
                    progress: item.progress,
                    local: item.local,
                })
                .collect();
            let model = Rc::new(VecModel::from(tiles));
            rails.push(RailModel {
                title: shelf.title.clone().into(),
                source: shelf.source.clone().into(),
                items: ModelRc::from(model.clone()),
            });
            self.tiles.push(model);
        }

        self.rails.set_vec(rails);

        // Land on the first row that has anything in it.
        if self.row_len(self.row) == 0 {
            self.row = (1..self.rows()).find(|r| self.row_len(*r) > 0).unwrap_or(0);
        }

        // Something for the hero to stand on from the first frame.
        if self.backdrop.is_none() {
            self.backdrop = self
                .shelves
                .iter()
                .flat_map(|shelf| shelf.items.iter())
                .find_map(|item| item.background.clone());
        }
    }

    /// Asks for what the panel is showing and what it is about to, and hands
    /// over whatever has arrived. Everything outside the window is given an
    /// empty image, which is what keeps the whole catalogue off the GPU.
    pub fn sync_artwork(&mut self, images: &mut ImageManager) {
        // Which shelf the remote is on, as an index into `shelves`. Rows 0 and
        // 1 are the bar and the launcher, so above the shelves there is no
        // focused one and the first is treated as the centre — that is what the
        // panel is showing.
        let focus_shelf = self.row.saturating_sub(SHELF_ROW) as isize;

        for (index, shelf) in self.shelves.iter().enumerate() {
            if shelf.items.is_empty() {
                continue;
            }
            // This shelf and its two neighbours. Further than that is not about
            // to be on the panel, and holding it would be holding the catalogue.
            let nearby = (index as isize - focus_shelf).abs() <= 1;
            let centre = self.columns.get(index + 1).copied().unwrap_or(0);
            let last = shelf.items.len() - 1;
            let from = centre.saturating_sub(BEHIND);
            let to = (centre + AHEAD).min(last);

            let Some(tiles) = self.tiles.get(index) else { continue };

            for (column, item) in shelf.items.iter().enumerate() {
                let inside = nearby && column >= from && column <= to;
                let Some(mut tile) = tiles.row_data(column) else { continue };

                let wanted = if inside {
                    item.poster.as_deref().map(|url| Key::new(url, POSTER_WIDTH))
                } else {
                    None
                };

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
                    tiles.set_row_data(column, tile);
                }
            }
        }
    }

    /// The backdrop, and which layer to draw it in so the change is a fade.
    ///
    /// It follows the focused title when that title has one, and otherwise
    /// keeps whatever is already on the panel. The operator's own library is
    /// artwork-poor — the account sends a poster and nothing else — so a
    /// backdrop that strictly followed focus would mean a black hero for the
    /// two shelves every session starts on. A poster stretched across the panel
    /// is not the answer either: it is three hundred pixels wide at the source.
    pub fn backdrop(&mut self, images: &mut ImageManager) -> Option<(slint::Image, bool)> {
        let url = self
            .focused()
            .and_then(|item| item.background.clone())
            .or_else(|| self.backdrop.clone())?;

        if self.backdrop.as_deref() != Some(url.as_str()) {
            self.backdrop = Some(url.clone());
            self.fade = if self.fade > 0.5 { 0.0 } else { 1.0 };
        }

        let key = Key::new(&url, BACKDROP_WIDTH);
        images.want(&key);
        let art = images.get(&key)?;
        Some((art, self.fade > 0.5))
    }

    /// Title, the facts line, and the summary — for whatever is focused.
    pub fn hero(&self) -> (String, String, String) {
        let Some(item) = self.focused() else {
            return (String::new(), String::new(), String::new());
        };

        let mut facts: Vec<String> = Vec::new();
        if let Some(year) = &item.year {
            facts.push(year.clone());
        }
        if let Some(rating) = &item.rating {
            facts.push(format!("IMDb {rating}"));
        }
        facts.extend(item.genres.iter().take(2).cloned());

        (
            item.title.clone(),
            facts.join("  ·  "),
            item.summary.clone().unwrap_or_default(),
        )
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

        let local: Vec<Item> = library.items.iter().take(RAIL_LIMIT).map(Item::from_preview).collect();
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
            items: row.items.iter().take(RAIL_LIMIT).map(Item::from_preview).collect(),
        });
    }

    shelves
}

/// An addon's catalogue names are English and terse — "Popular", "New" — and
/// the product is Turkish. Anything not recognised keeps its own name with the
/// provider's word in front of it.
fn rail_title(name: &str, kind: &str) -> String {
    let noun = if kind == "series" { "Diziler" } else { "Filmler" };
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
    let hash = name.bytes().fold(17u32, |acc, byte| acc.wrapping_mul(31).wrapping_add(byte as u32));
    (hash % 360) as f32
}
