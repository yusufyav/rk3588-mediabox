//! The catalogue, which is an application of the box rather than its home.
//!
//! This is what is behind the "Filmler ve Diziler" tile, laid out as the
//! reference's board: a column of places down the left, the search box across
//! the top, and a shelf per catalogue under it -- "İzlemeye devam edin" first,
//! every poster named, "Tümünü Gör" at the end of each shelf.
//!
//! The focus model is the same structural one as everywhere else: a row, a
//! column, and one remembered column per row, so walking down a shelf and back
//! up again lands where it started. Left off the first poster is the column of
//! places; Up off the first shelf is the search box.

use std::rc::Rc;

use slint::VecModel;

use crate::RailModel;
use crate::images::ImageManager;
use crate::screens::shelves::Shelves;
use crate::state::{Item, More, Shelf};

/// The places down the left, in the reference's order. The reference also has
/// a calendar and its addons there; this box has neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Board,
    Discover,
    Library,
    Settings,
}

pub const PLACES: [Place; 4] = [Place::Board, Place::Discover, Place::Library, Place::Settings];

impl Place {
    /// The mark, by the name the interface draws it under.
    pub fn icon(self) -> &'static str {
        match self {
            Place::Board => "home",
            Place::Discover => "compass",
            Place::Library => "library",
            Place::Settings => "gear",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Shelves,
    Places,
    Search,
}

pub struct Media {
    shelves: Shelves,
    pub zone: Zone,
    pub place: usize,
    /// Whether the viewer has moved yet. Until they have, the remote is put
    /// on the first poster the moment there is one.
    moved: bool,
}

impl Media {
    pub fn new() -> Self {
        Self {
            shelves: Shelves::new(),
            zone: Zone::Shelves,
            place: 0,
            moved: false,
        }
    }

    pub fn rails(&self) -> Rc<VecModel<RailModel>> {
        self.shelves.rails.clone()
    }

    /// The shelf the remote is on, or was last on.
    pub fn row(&self) -> usize {
        self.shelves.row
    }

    pub fn column(&self) -> usize {
        self.shelves.column()
    }

    pub fn focused(&self) -> Option<&Item> {
        if self.zone != Zone::Shelves {
            return None;
        }
        self.shelves.focused()
    }

    /// Where "Tümünü Gör" leads, when the remote is on it.
    pub fn focused_more(&self) -> Option<&More> {
        if self.zone != Zone::Shelves {
            return None;
        }
        self.shelves.focused_more()
    }

    /// The place the remote is on, in the column down the left.
    pub fn focused_place(&self) -> Option<Place> {
        (self.zone == Zone::Places).then(|| PLACES.get(self.place).copied())?
    }

    /// Rebuilds the shelves, keeping the remote on the same title if it is
    /// still there. Nothing to stand on is the search box.
    pub fn set_shelves(&mut self, shelves: Vec<Shelf>) {
        self.shelves.set(shelves);
        if self.shelves.is_empty() && self.zone == Zone::Shelves {
            self.zone = Zone::Search;
        } else if !self.shelves.is_empty() && !self.moved {
            self.zone = Zone::Shelves;
        }
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        self.moved = true;
        match self.zone {
            Zone::Places => {
                if dx > 0 {
                    self.zone = if self.shelves.is_empty() { Zone::Search } else { Zone::Shelves };
                    return true;
                }
                if dy != 0 {
                    let next = (self.place as i32 + dy).clamp(0, PLACES.len() as i32 - 1) as usize;
                    let moved = next != self.place;
                    self.place = next;
                    return moved;
                }
                false
            }
            Zone::Search => {
                if dx < 0 {
                    self.zone = Zone::Places;
                    self.place = 0;
                    return true;
                }
                if dy > 0 {
                    if let Some(first) = self.shelves.first_filled() {
                        if self.shelves.row_len(self.shelves.row) == 0 {
                            self.shelves.row = first;
                        }
                        self.zone = Zone::Shelves;
                        return true;
                    }
                }
                false
            }
            Zone::Shelves => {
                if dy != 0 {
                    if self.shelves.step_row(dy) {
                        return true;
                    }
                    if dy < 0 {
                        self.zone = Zone::Search;
                        return true;
                    }
                    return false;
                }
                if dx != 0 {
                    if self.shelves.step_column(dx) {
                        return true;
                    }
                    if dx < 0 {
                        // Off the first poster: the places, on the one this is.
                        self.zone = Zone::Places;
                        self.place = 0;
                        return true;
                    }
                }
                false
            }
        }
    }

    pub fn sync_artwork(&mut self, images: &mut ImageManager) {
        let anchor = self.shelves.row as i32;
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
            more: None,
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
        assert_eq!(media.row(), 1);
        assert!(media.focused().is_some());
    }

    #[test]
    fn an_empty_shelf_is_stepped_over() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 3), shelf("empty", 0), shelf("c", 3)]);
        assert!(media.step(0, 1));
        assert_eq!(media.row(), 2);
    }

    /// The reference's board: the search box above the shelves, the places
    /// down the left.
    #[test]
    fn up_off_the_first_shelf_is_search_and_left_off_the_first_poster_is_the_places() {
        let mut media = Media::new();
        media.set_shelves(vec![shelf("a", 3), shelf("b", 3)]);
        assert!(media.step(0, -1));
        assert_eq!(media.zone, Zone::Search);
        assert!(media.step(0, 1));
        assert!(media.focused().is_some());
        assert!(media.step(-1, 0));
        assert_eq!(media.focused_place(), Some(Place::Board));
        assert!(media.step(0, 1));
        assert_eq!(media.focused_place(), Some(Place::Discover));
        assert!(media.step(1, 0));
        assert_eq!(media.zone, Zone::Shelves);
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
        assert_eq!(media.row(), 1);
        for _ in 0..10 {
            media.step(0, -1);
        }
        assert_eq!(media.zone, Zone::Search, "the search box is the top of this screen");
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

    /// The catalogue arrives after the screen opens: the remote goes to its
    /// first poster, not to the search box it had to stand on meanwhile.
    #[test]
    fn a_catalogue_that_arrives_late_gets_the_remote() {
        let mut media = Media::new();
        media.set_shelves(Vec::new());
        assert_eq!(media.zone, Zone::Search);
        media.set_shelves(vec![shelf("a", 3)]);
        assert!(media.focused().is_some());
    }
}
