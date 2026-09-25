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

use slint::VecModel;

use crate::AppTile;
use crate::model::{LibraryListing, MetaPreview};

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

/// Kodi's mark as Simple Icons (CC0-1.0) draws it in a 24x24 box. It is a
/// filled glyph there; stroked here like every other tile, it is the same four
/// shapes in outline.
const KODI_MARK: &str = "M12.03.047c-.226 0-.452.107-.669.324-.922.922-1.842 1.845-2.763 2.768-.233.233-.455.48-.703.695-.31.267-.405.583-.399.988.02 1.399.008 2.799.008 4.198 0 1.453-.002 2.907 0 4.36 0 .11.002.223.03.327.087.337.303.393.546.15 1.31-1.31 2.618-2.622 3.928-3.933l4.449-4.453c.43-.431.43-.905 0-1.336L12.697.37c-.216-.217-.442-.324-.668-.324zm7.224 7.23c-.223 0-.445.104-.65.309L14.82 11.37c-.428.429-.427.895 0 1.322l3.76 3.766c.44.44.908.44 1.346.002 1.215-1.216 2.427-2.433 3.644-3.647.182-.18.353-.364.43-.615v-.33c-.077-.251-.246-.436-.428-.617-1.224-1.22-2.443-2.445-3.666-3.668-.205-.205-.429-.307-.652-.307zM4.18 7.611c-.086.014-.145.094-.207.157L.209 11.572c-.28.284-.278.677.004.96l2.043 2.046c.59.59 1.177 1.182 1.767 1.772.169.168.33.139.416-.084.044-.114.062-.242.063-.364.004-1.283.004-2.567.004-3.851h-.002V8.184c0-.085-.01-.169-.022-.252-.019-.135-.072-.258-.207-.309a.186.186 0 0 0-.095-.012zm7.908 6.838c-.224 0-.447.106-.656.315L7.66 18.537c-.433.434-.433.899.002 1.334 1.215 1.216 2.43 2.43 3.643 3.649.18.18.361.354.611.433h.33c.244-.069.423-.226.598-.402 1.222-1.23 2.45-2.453 3.676-3.68.43-.43.427-.905-.004-1.338l-3.772-3.773c-.208-.208-.432-.311-.656-.31z";

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
        // Kodi's own mark, from Simple Icons (CC0), stroked like the rest.
        "kodi" => (
            KODI_MARK,
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
    /// On the account's "Devam Et". Said by the media core, not worked out
    /// here from the progress: a title can be on it with no duration at all.
    pub continuing: bool,
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
            continuing: false,
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
            continuing: false,
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

/// The home surface: what this box can be used for, and the remote's place on
/// it.
///
/// Nothing from the catalogue or the account is on this screen, "Devam Et"
/// included -- that is inside "Filmler ve Diziler", with the rest of what the
/// account carries. MediaBox is an environment for this board rather than a
/// player with a menu, and a home screen that waited for a streaming account
/// before it could be drawn was the account's screen, not the board's: every
/// return from Kodi restarts this interface, and each one opened on "Raflar
/// getiriliyor" while the account was asked again.
pub struct Home {
    /// What this box can be used for.
    pub apps: Vec<AppEntryTile>,
    column: usize,
    pub app_tiles: Rc<VecModel<AppTile>>,
}

impl Home {
    pub fn new() -> Self {
        Self {
            apps: Vec::new(),
            // The web interface autofocuses its first application tile and so
            // does this: the first thing a person meets is what the box can do.
            column: 0,
            app_tiles: Rc::new(VecModel::default()),
        }
    }

    pub fn column(&self) -> usize {
        self.column
    }

    /// The focused application.
    pub fn focused_app(&self) -> Option<&AppEntryTile> {
        self.apps.get(self.column)
    }

    /// Replaces the launcher's tiles, keeping the remote on the same
    /// application if it is still there -- the list is re-read every few
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

        self.column = holding
            .and_then(|id| self.apps.iter().position(|tile| tile.id == id))
            .unwrap_or_else(|| self.column.min(self.apps.len().saturating_sub(1)));
    }

    /// Along the launcher. There is one row, so up and down go nowhere.
    pub fn step(&mut self, dx: isize) -> bool {
        if self.apps.is_empty() || dx == 0 {
            return false;
        }
        let next = (self.column as isize + dx).clamp(0, self.apps.len() as isize - 1) as usize;
        let moved = next != self.column;
        self.column = next;
        moved
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
        // As the media core ordered it, and all of it: this is the list the
        // phone shows, and dropping or re-sorting any of it here is what made
        // the two disagree.
        let unfinished: Vec<Item> = library
            .continue_watching
            .iter()
            .map(|preview| Item {
                continuing: true,
                ..Item::from_preview(preview)
            })
            .collect();
        let continuing: std::collections::HashSet<&str> =
            unfinished.iter().map(|item| item.id.as_str()).collect();
        if !unfinished.is_empty() {
            shelves.push(Shelf {
                title: "Devam Et".into(),
                source: String::new(),
                items: unfinished.clone(),
            });
        }

        let mine: Vec<Item> = library
            .stremio
            .iter()
            .filter(|item| !continuing.contains(item.id.as_str()))
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

    /// The home screen opens on the launcher, and the launcher is all of it:
    /// up and down go nowhere, and nothing from the catalogue or the account
    /// is on it -- the media tile is where that is.
    #[test]
    fn the_home_screen_is_the_launcher_and_nothing_else() {
        let mut home = Home::new();
        home.set_apps(launcher());
        assert_eq!(home.column(), 0);
        assert!(home.focused_app().is_some());
        assert!(!home.step(0));

        let media = home
            .apps
            .iter()
            .find(|t| t.id == "media")
            .expect("the tile");
        assert_eq!(media.action, AppAction::Shelves);
    }

    #[test]
    fn the_launcher_ends_where_its_tiles_do() {
        let mut home = Home::new();
        home.set_apps(launcher());
        for _ in 0..8 {
            home.step(1);
        }
        assert_eq!(home.column(), home.apps.len() - 1);
        for _ in 0..8 {
            home.step(-1);
        }
        assert_eq!(home.column(), 0);
    }

    /// "Devam Et" is the media core's list, as it came: a series episode
    /// with no duration and a film near its end are on it, in the order it
    /// gave, and neither is repeated under "Kitaplığım".
    #[test]
    fn devam_et_is_the_account_list_as_it_came() {
        let listing: LibraryListing = serde_json::from_value(serde_json::json!({
            "stremio": [
                {"id": "kept", "name": "Kept"},
                {"id": "tail", "name": "Tail"}
            ],
            "continueWatching": [
                {"id": "episode", "type": "series", "name": "Episode",
                 "state": {"timeOffset": 1000, "duration": 0}},
                {"id": "tail", "name": "Tail",
                 "state": {"timeOffset": 98, "duration": 100}}
            ]
        }))
        .unwrap();
        let shelves = shelves_from(&crate::model::HomeRows { rows: Vec::new() }, Some(&listing));

        let ids = |title: &str| -> Vec<String> {
            shelves
                .iter()
                .find(|shelf| shelf.title == title)
                .map(|shelf| shelf.items.iter().map(|item| item.id.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(ids("Devam Et"), ["episode", "tail"]);
        assert_eq!(ids("Kitaplığım"), ["kept"]);
    }

    #[test]
    fn a_relaunched_list_keeps_the_remote_on_the_same_application() {
        let mut home = Home::new();
        home.set_apps(launcher());
        home.step(1);
        let id = home.focused_app().unwrap().id.clone();
        let mut shuffled = launcher();
        shuffled.insert(
            0,
            tile(
                "kodi",
                "KODI",
                true,
                "",
                AppAction::Launch("kodi".into()),
            ),
        );
        home.set_apps(shuffled);
        assert_eq!(home.focused_app().unwrap().id, id);
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
