//! The screens this interface has, as state rather than as drawing.
//!
//! Each of these is plain data and arithmetic: where the remote is, what it
//! means to move, what a press chooses. None of them talks to the control
//! plane, opens a socket or touches Slint, which is what makes the focus model
//! testable — and the focus model is the product on a television.

pub mod account;
pub mod audio;
pub mod cooling;
pub mod ethernet;
pub mod output;
pub mod diagnostics;
pub mod discover;
pub mod library;
pub mod live;
pub mod media;
pub mod media_settings;
pub mod now_playing;
pub mod power;
pub mod search;
pub mod settings;
pub mod shelves;
pub mod wireless;

/// What a pointer is over on one of the catalogue's screens, as they draw
/// it: a place down the left, the search box across the top (both in the
/// shared frame, ui/chrome.slint), a poster of a grid, a poster of a shelf
/// (row, column; the column past the last poster is "Tümünü Gör").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogTarget {
    Place(usize),
    SearchBox,
    Item(usize),
    Shelf(usize, usize),
}
