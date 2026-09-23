//! The screens this interface has, as state rather than as drawing.
//!
//! Each of these is plain data and arithmetic: where the remote is, what it
//! means to move, what a press chooses. None of them talks to the control
//! plane, opens a socket or touches Slint, which is what makes the focus model
//! testable — and the focus model is the product on a television.

pub mod account;
pub mod cooling;
pub mod diagnostics;
pub mod library;
pub mod media;
pub mod now_playing;
pub mod power;
pub mod search;
pub mod settings;
pub mod wireless;
