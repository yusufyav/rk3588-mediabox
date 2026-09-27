//! Rows of posters, one per catalogue: the shape the catalogue screen and the
//! search results share.
//!
//! The focus model is the structural one used everywhere else: a row, a
//! column, and one remembered column per row, so walking down a shelf and back
//! up again lands where it started. The Slint models are kept beside the data
//! rather than rebuilt from it, so a picture arriving changes one tile rather
//! than every shelf on the panel.

use std::rc::Rc;

use slint::{Model, ModelRc, VecModel};

use crate::images::{ImageManager, Key};
use crate::state::{Item, POSTER_WIDTH, Shelf};
use crate::{PosterItem, RailModel};

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
/// Nine posters fit across the panel at the reference's card width, and at
/// the end of a shelf -- on its "Tümünü Gör" -- the nine before it are all on
/// the panel, so the window is ten either way. That is one screenful behind
/// and one ahead, which is what "nearly visible" means on a rail that can
/// scroll both ways.
const BEHIND: usize = 10;
const AHEAD: usize = 10;

pub struct Shelves {
    pub shelves: Vec<Shelf>,
    /// Which shelf the remote is on.
    pub row: usize,
    columns: Vec<usize>,

    pub rails: Rc<VecModel<RailModel>>,
    tiles: Vec<Rc<VecModel<PosterItem>>>,
}

impl Shelves {
    pub fn new() -> Self {
        Self {
            shelves: Vec::new(),
            row: 0,
            columns: Vec::new(),
            rails: Rc::new(VecModel::default()),
            tiles: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.shelves.len()
    }

    pub fn row_len(&self, row: usize) -> usize {
        self.shelves.get(row).map(|shelf| shelf.items.len()).unwrap_or(0)
    }

    pub fn column(&self) -> usize {
        self.columns.get(self.row).copied().unwrap_or(0)
    }

    /// Back to the top: the first shelf with anything on it, and every shelf
    /// at its start. False when that is where the remote already is.
    pub fn to_top(&mut self) -> bool {
        let first = self.first_filled().unwrap_or(0);
        let at_top = self.row == first && self.columns.iter().all(|&column| column == 0);
        self.row = first;
        self.columns.iter_mut().for_each(|column| *column = 0);
        !at_top
    }

    /// Every shelf's own remembered column, for each to be scrolled to its
    /// own: the one its artwork is loaded around.
    pub fn columns(&self) -> Vec<i32> {
        self.columns.iter().map(|&column| column as i32).collect()
    }

    /// The stops along a shelf: its titles, and "Tümünü Gör" after them when
    /// the shelf has one.
    fn stops(&self, row: usize) -> usize {
        let len = self.row_len(row);
        let more = self.shelves.get(row).is_some_and(|s| s.more.is_some());
        len + usize::from(more && len > 0)
    }

    /// Where "Tümünü Gör" leads, when the remote is on it.
    pub fn focused_more(&self) -> Option<&crate::state::More> {
        let shelf = self.shelves.get(self.row)?;
        (self.column() == shelf.items.len()).then_some(shelf.more.as_ref())?
    }

    pub fn focused(&self) -> Option<&Item> {
        self.shelves.get(self.row)?.items.get(self.column())
    }

    /// The first shelf with anything on it.
    pub fn first_filled(&self) -> Option<usize> {
        (0..self.len()).find(|row| self.row_len(*row) > 0)
    }

    pub fn is_empty(&self) -> bool {
        self.first_filled().is_none()
    }

    /// Rebuilds the shelves, keeping the remote on the same title if it is
    /// still there. A catalogue is re-read whenever its screen is opened, and
    /// focus that jumped each time would be unusable.
    pub fn set(&mut self, shelves: Vec<Shelf>) {
        let holding = self.focused().map(|item| item.id.clone());

        self.shelves = shelves;
        self.columns.resize(self.len().max(1), 0);
        self.tiles.clear();

        let mut rails: Vec<RailModel> = Vec::with_capacity(self.shelves.len());
        for shelf in &self.shelves {
            let tiles: Vec<PosterItem> = shelf.items.iter().map(tile).collect();
            let model = Rc::new(VecModel::from(tiles));
            rails.push(RailModel {
                title: shelf.title.clone().into(),
                source: shelf.source.clone().into(),
                note: shelf.note.clone().into(),
                more: if shelf.more.is_some() { "Tümünü Gör".into() } else { Default::default() },
                items: ModelRc::from(model.clone()),
            });
            self.tiles.push(model);
        }
        self.rails.set_vec(rails);

        if self.row_len(self.row) == 0 {
            self.row = self.first_filled().unwrap_or(0);
        }
        if let Some(id) = holding {
            if let Some(column) = self
                .shelves
                .get(self.row)
                .and_then(|shelf| shelf.items.iter().position(|item| item.id == id))
            {
                self.columns[self.row] = column;
            }
        }
        self.clamp();
    }

    /// Empties it, and forgets where the remote was: a new set of results is
    /// not the old one moved.
    pub fn clear(&mut self) {
        self.set(Vec::new());
        self.row = 0;
        self.columns.iter_mut().for_each(|column| *column = 0);
    }

    fn clamp(&mut self) {
        let len = self.stops(self.row);
        if len > 0 {
            if let Some(slot) = self.columns.get_mut(self.row) {
                *slot = (*slot).min(len - 1);
            }
        }
    }

    /// Up or down a shelf, stepping over the ones that arrived empty. False
    /// when there is no shelf that way, so the caller can decide what is past
    /// the edge.
    pub fn step_row(&mut self, dy: i32) -> bool {
        let mut row = self.row as i32 + dy;
        while row >= 0 && (row as usize) < self.len() && self.row_len(row as usize) == 0 {
            row += dy.signum();
        }
        if row < 0 || row as usize >= self.len() || row as usize == self.row {
            return false;
        }
        self.row = row as usize;
        self.clamp();
        true
    }

    /// Along the shelf. False at either end.
    pub fn step_column(&mut self, dx: i32) -> bool {
        let len = self.stops(self.row);
        if len == 0 {
            return false;
        }
        let current = self.column() as i32;
        let next = (current + dx).clamp(0, len as i32 - 1);
        if next == current {
            return false;
        }
        self.columns[self.row] = next as usize;
        true
    }

    /// Asks for what the panel is showing and what it is about to, and hands
    /// over whatever has arrived. Everything outside the window is given an
    /// empty image, which is what keeps the whole catalogue off the GPU.
    ///
    /// `anchor` is the shelf the panel is centred on, which is the focused
    /// one; -1 when the remote is above the first.
    pub fn sync_artwork(&mut self, images: &mut ImageManager, anchor: i32) {
        for (index, shelf) in self.shelves.iter().enumerate() {
            if shelf.items.is_empty() {
                continue;
            }
            // This shelf and the two either side of it. Further than that is
            // not about to be on the panel, and holding it would be holding
            // the catalogue.
            //
            // One either side was not enough for the same reason eight
            // posters are needed behind the focus: the screen shows three
            // rails at once, and at the bottom of the list the focus stops
            // moving the strip and walks down it, so the rail two above the
            // focused one is still in front of the viewer when its artwork is
            // taken away.
            let nearby = (index as i32 - anchor).abs() <= 2;
            let centre = self.columns.get(index).copied().unwrap_or(0);
            let last = shelf.items.len() - 1;
            let from = centre.saturating_sub(BEHIND);
            let to = (centre + AHEAD).min(last);

            let Some(tiles) = self.tiles.get(index) else {
                continue;
            };

            for (column, item) in shelf.items.iter().enumerate() {
                let inside = nearby && column >= from && column <= to;
                let Some(mut tile) = tiles.row_data(column) else {
                    continue;
                };

                let wanted = if inside {
                    item.poster
                        .as_deref()
                        .map(|url| Key::new(url, POSTER_WIDTH))
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

fn tile(item: &Item) -> PosterItem {
    PosterItem {
        id: item.id.clone().into(),
        kind: item.kind.clone().into(),
        title: item.title.clone().into(),
        subtitle: item.year.clone().unwrap_or_default().into(),
        art: slint::Image::default(),
        hue: item.hue,
        progress: item.progress,
        local: item.local,
        watched: item.record.as_ref().is_some_and(|r| r.times_watched > 0),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn shelf(name: &str, count: usize) -> Shelf {
        Shelf {
            title: name.into(),
            source: String::new(),
            more: None,
            note: String::new(),
            items: (0..count)
                .map(|i| Item::stub(&format!("{name}-{i}")))
                .collect(),
        }
    }

    #[test]
    fn each_shelf_remembers_its_own_column() {
        let mut shelves = Shelves::new();
        shelves.set(vec![shelf("a", 9), shelf("b", 9)]);
        shelves.step_column(1);
        shelves.step_column(1);
        assert!(shelves.step_row(1));
        assert_eq!(shelves.column(), 0);
        assert!(shelves.step_row(-1));
        assert_eq!(shelves.column(), 2);
    }

    #[test]
    fn an_empty_shelf_is_stepped_over_and_the_edges_say_so() {
        let mut shelves = Shelves::new();
        shelves.set(vec![shelf("a", 3), shelf("empty", 0), shelf("c", 3)]);
        assert!(!shelves.step_row(-1), "nothing above the first shelf");
        assert!(shelves.step_row(1));
        assert_eq!(shelves.row, 2);
        assert!(!shelves.step_row(1), "nothing below the last shelf");
    }

    #[test]
    fn clearing_forgets_the_old_position() {
        let mut shelves = Shelves::new();
        shelves.set(vec![shelf("a", 9), shelf("b", 9)]);
        shelves.step_row(1);
        shelves.step_column(1);
        shelves.clear();
        shelves.set(vec![shelf("x", 4), shelf("y", 4)]);
        assert_eq!((shelves.row, shelves.column()), (0, 0));
    }

    #[test]
    fn right_past_the_last_title_is_see_all() {
        let mut shelves = Shelves::new();
        let mut row = shelf("a", 2);
        row.more = Some(crate::state::More::Continuing);
        shelves.set(vec![row]);
        assert!(shelves.step_column(1));
        assert!(shelves.focused_more().is_none());
        assert!(shelves.step_column(1));
        assert_eq!(shelves.focused_more(), Some(&crate::state::More::Continuing));
        assert!(shelves.focused().is_none());
        assert!(!shelves.step_column(1), "nothing past it");
    }
}
