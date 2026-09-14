//! The catalogue, which is an application of the box rather than its home.
//!
//! This is what is behind the "Filmler ve Diziler" tile: shelves of titles, the
//! way the reference does it — a row per catalogue, every poster named, the
//! unfinished ones first. The home screen is a launcher and does not carry any
//! of this; only the one shelf of half-watched titles stays there, because
//! "carry on with what you were doing" is a property of the box rather than of
//! the catalogue.
//!
//! The focus model is the same structural one as everywhere else: a row, a
//! column, and one remembered column per row, so walking down a shelf and back
//! up again lands where it started.

use std::rc::Rc;

use slint::{Model, ModelRc, VecModel};

use crate::images::{ImageManager, Key};
use crate::state::{Item, NAV, Nav, POSTER_WIDTH, Shelf};
use crate::{PosterItem, RailModel};

/// How far either side of the focused poster is worth having ready.
const BEHIND: usize = 2;
const AHEAD: usize = 8;

/// The one row above the shelves.
const BAR_ROW: usize = 1;

pub struct Media {
    pub shelves: Vec<Shelf>,
    pub row: usize,
    columns: Vec<usize>,

    /// The Slint side, kept beside the data rather than rebuilt from it, so a
    /// picture arriving changes one tile rather than every shelf on the panel.
    pub rails: Rc<VecModel<RailModel>>,
    tiles: Vec<Rc<VecModel<PosterItem>>>,
}

impl Media {
    pub fn new() -> Self {
        Self {
            shelves: Vec::new(),
            row: 0,
            columns: Vec::new(),
            rails: Rc::new(VecModel::default()),
            tiles: Vec::new(),
        }
    }

    /// The bar, then a row per shelf. Searching and the library are on the bar
    /// because they are things one does inside a catalogue — the home screen is
    /// a launcher and has neither.
    pub fn rows(&self) -> usize {
        BAR_ROW + self.shelves.len()
    }

    pub fn column(&self) -> usize {
        self.columns.get(self.row).copied().unwrap_or(0)
    }

    pub fn focused(&self) -> Option<&Item> {
        let shelf = self.shelves.get(self.row.checked_sub(BAR_ROW)?)?;
        shelf.items.get(self.column())
    }

    /// Which screen the bar would open, when the remote is on it.
    pub fn focused_nav(&self) -> Option<Nav> {
        (self.row == 0).then(|| NAV.get(self.column()).copied())?
    }

    fn row_len(&self, row: usize) -> usize {
        if row == 0 {
            return NAV.len();
        }
        self.shelves.get(row - BAR_ROW).map(|shelf| shelf.items.len()).unwrap_or(0)
    }

    /// Rebuilds the shelves, keeping the remote on the same title if it is
    /// still there. The catalogue is re-read when the screen is opened, and
    /// focus that jumped each time would be unusable.
    pub fn set_shelves(&mut self, shelves: Vec<Shelf>) {
        let holding = self.focused().map(|item| item.id.clone());

        self.shelves = shelves;
        self.columns.resize(self.rows().max(BAR_ROW + 1), 0);
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
                    subtitle: item.year.clone().unwrap_or_default().into(),
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

        // Land on the first shelf that has anything on it. The bar is a place
        // to leave from, not the place this screen opens.
        if self.row == 0 || self.row_len(self.row) == 0 {
            self.row = (BAR_ROW..self.rows())
                .find(|row| self.row_len(*row) > 0)
                .unwrap_or(0);
        }
        if let Some(id) = holding {
            if let Some(column) = self
                .shelves
                .get(self.row.saturating_sub(BAR_ROW))
                .and_then(|shelf| shelf.items.iter().position(|item| item.id == id))
            {
                self.columns[self.row] = column;
            }
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        let len = self.row_len(self.row);
        if len > 0 {
            if let Some(slot) = self.columns.get_mut(self.row) {
                *slot = (*slot).min(len - 1);
            }
        }
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        let mut moved = false;

        if dy != 0 {
            let mut row = self.row as i32 + dy;
            // A shelf that arrived empty is stepped over rather than landed on.
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
                let current = self.column() as i32;
                let next = (current + dx).clamp(0, len as i32 - 1);
                if next != current {
                    self.columns[self.row] = next as usize;
                    moved = true;
                }
            }
        }

        self.clamp();
        moved
    }

    /// Asks for what the panel is showing and what it is about to, and hands
    /// over whatever has arrived. Everything outside the window is given an
    /// empty image, which is what keeps the whole catalogue off the GPU.
    pub fn sync_artwork(&mut self, images: &mut ImageManager) {
        for (index, shelf) in self.shelves.iter().enumerate() {
            if shelf.items.is_empty() {
                continue;
            }
            // This shelf and its two neighbours. Further than that is not about
            // to be on the panel, and holding it would be holding the
            // catalogue.
            let nearby = (index as i32 - (self.row as i32 - BAR_ROW as i32)).abs() <= 1;
            let centre = self.columns.get(index + BAR_ROW).copied().unwrap_or(0);
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

                // Only write back when it actually changed: set_row_data is
                // what makes Slint redraw the tile.
                let had = tile.art.size().width > 0;
                let has = art.size().width > 0;
                if had != has {
                    tile.art = art;
                    tiles.set_row_data(column, tile);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(name: &str, count: usize) -> Shelf {
        Shelf {
            title: name.into(),
            source: String::new(),
            items: (0..count).map(|i| Item::stub(&format!("{name}-{i}"))).collect(),
        }
    }

    #[test]
    fn focus_starts_on_the_first_shelf_that_has_anything() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("empty", 0), shelf("a", 4)]);
        assert_eq!(media.row, BAR_ROW + 1);
        assert!(media.focused().is_some());
    }

    #[test]
    fn an_empty_shelf_is_stepped_over() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 3), shelf("empty", 0), shelf("c", 3)]);
        assert_eq!(media.row, BAR_ROW);
        assert!(media.step(0, 1));
        assert_eq!(media.row, BAR_ROW + 2);
    }

    /// Searching is inside the catalogue, on its own bar, above the shelves.
    #[test]
    fn the_bar_is_above_the_shelves_and_carries_search() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 3)]);
        assert!(media.focused_nav().is_none());
        for _ in 0..5 {
            media.step(0, -1);
        }
        assert_eq!(media.row, 0);
        assert_eq!(media.focused_nav(), Some(Nav::Search));
        assert!(media.step(1, 0));
        assert_eq!(media.focused_nav(), Some(Nav::Library));
        assert!(media.step(0, 1));
        assert!(media.focused().is_some());
    }

    #[test]
    fn each_shelf_remembers_its_own_column() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 9), shelf("b", 9)]);
        media.step(1, 0);
        media.step(1, 0);
        assert_eq!(media.column(), 2);
        media.step(0, 1);
        assert_eq!(media.column(), 0);
        media.step(0, -1);
        assert_eq!(media.column(), 2);
    }

    #[test]
    fn the_column_never_points_past_a_shorter_shelf() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 9), shelf("b", 2)]);
        for _ in 0..8 {
            media.step(1, 0);
        }
        media.step(0, 1);
        assert!(media.column() < 2);
        assert!(media.focused().is_some());
    }

    #[test]
    fn focus_never_steps_off_the_catalogue() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 3), shelf("b", 3)]);
        for _ in 0..10 {
            media.step(0, 1);
        }
        assert_eq!(media.row, BAR_ROW + 1);
        for _ in 0..10 {
            media.step(0, -1);
        }
        assert_eq!(media.row, 0, "the bar is the top of this screen");
    }

    #[test]
    fn a_refreshed_catalogue_keeps_the_remote_on_the_same_title() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 9)]);
        media.step(1, 0);
        media.step(1, 0);
        let id = media.focused().unwrap().id.clone();

        let mut moved = shelf("a", 9);
        moved.items.insert(0, Item::stub("newcomer"));
        media.set_shelves(vec![moved]);
        assert_eq!(media.focused().unwrap().id, id);
    }
}
