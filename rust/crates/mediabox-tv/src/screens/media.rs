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
//! up again lands where it started. Row 0 is the bar above the shelves.

use std::rc::Rc;

use slint::VecModel;

use crate::RailModel;
use crate::images::ImageManager;
use crate::screens::shelves::Shelves;
use crate::state::{Item, NAV, Nav, Shelf};

/// The one row above the shelves.
const BAR_ROW: usize = 1;

pub struct Media {
    shelves: Shelves,
    on_bar: bool,
    bar_column: usize,
}

impl Media {
    pub fn new() -> Self {
        Self {
            shelves: Shelves::new(),
            on_bar: false,
            bar_column: 0,
        }
    }

    pub fn rails(&self) -> Rc<VecModel<RailModel>> {
        self.shelves.rails.clone()
    }

    /// Where the remote is, counting the bar as row 0.
    pub fn row(&self) -> usize {
        if self.on_bar { 0 } else { self.shelves.row + BAR_ROW }
    }

    pub fn column(&self) -> usize {
        if self.on_bar { self.bar_column } else { self.shelves.column() }
    }

    pub fn focused(&self) -> Option<&Item> {
        if self.on_bar {
            return None;
        }
        self.shelves.focused()
    }

    /// Which screen the bar would open, when the remote is on it.
    pub fn focused_nav(&self) -> Option<Nav> {
        self.on_bar.then(|| NAV.get(self.bar_column).copied())?
    }

    /// Rebuilds the shelves, keeping the remote on the same title if it is
    /// still there, and landing on the first shelf that has anything on it:
    /// the bar is a place to leave from, not the place this screen opens.
    pub fn set_shelves(&mut self, shelves: Vec<Shelf>) {
        self.shelves.set(shelves);
        self.on_bar = self.shelves.is_empty();
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        let mut moved = false;

        if dy != 0 {
            if self.on_bar {
                if dy > 0 {
                    if let Some(first) = self.shelves.first_filled() {
                        self.shelves.row = first;
                        self.on_bar = false;
                        moved = true;
                    }
                }
            } else if self.shelves.step_row(dy) {
                moved = true;
            } else if dy < 0 {
                self.on_bar = true;
                moved = true;
            }
        }

        if dx != 0 {
            if self.on_bar {
                let next = (self.bar_column as i32 + dx).clamp(0, NAV.len() as i32 - 1) as usize;
                if next != self.bar_column {
                    self.bar_column = next;
                    moved = true;
                }
            } else if self.shelves.step_column(dx) {
                moved = true;
            }
        }

        moved
    }

    pub fn sync_artwork(&mut self, images: &mut ImageManager) {
        let anchor = if self.on_bar { -1 } else { self.shelves.row as i32 };
        self.shelves.sync_artwork(images, anchor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(name: &str, count: usize) -> Shelf {
        Shelf {
            title: name.into(),
            source: String::new(),
            note: String::new(),
            items: (0..count)
                .map(|i| Item::stub(&format!("{name}-{i}")))
                .collect(),
        }
    }

    #[test]
    fn focus_starts_on_the_first_shelf_that_has_anything() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("empty", 0), shelf("a", 4)]);
        assert_eq!(media.row(), BAR_ROW + 1);
        assert!(media.focused().is_some());
    }

    #[test]
    fn an_empty_shelf_is_stepped_over() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 3), shelf("empty", 0), shelf("c", 3)]);
        assert_eq!(media.row(), BAR_ROW);
        assert!(media.step(0, 1));
        assert_eq!(media.row(), BAR_ROW + 2);
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
        assert_eq!(media.row(), 0);
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
        assert_eq!(media.row(), BAR_ROW + 1);
        for _ in 0..10 {
            media.step(0, -1);
        }
        assert_eq!(media.row(), 0, "the bar is the top of this screen");
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
