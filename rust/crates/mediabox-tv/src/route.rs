//! Which screen the television is on, and how it gets back.
//!
//! A remote has one key for "not this" and it has to mean the same thing
//! everywhere, so the route is a stack rather than a variable: opening a screen
//! pushes, Back pops, and the screen underneath is found exactly as it was
//! rather than rebuilt from a guess. Home is the floor of the stack and cannot
//! be popped — there is nothing behind the screen the television was turned on
//! at.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Before the first shelf lands.
    Boot,
    Home,
    /// The catalogue, behind the "Filmler ve Diziler" tile.
    Media,
    Search,
    Library,
    Detail,
    NowPlaying,
    Settings,
    /// The Stremio sign-in form, opened from the settings screen.
    Account,
    Diagnostics,
    /// The Wi-Fi and Bluetooth screens, opened from the settings screen. Both
    /// need a list the size of the panel, and the Wi-Fi one needs a letter
    /// grid, so neither fits in the right-hand column of a two-pane screen.
    Wifi,
    Bluetooth,
}

impl Route {
    /// What the interface calls itself to Slint. The `screen` property is a
    /// string because Slint's `if` chain reads better than an integer, and
    /// because it is legible in a screenshot of the journal.
    pub fn name(self) -> &'static str {
        match self {
            Route::Boot => "boot",
            Route::Home => "home",
            Route::Media => "media",
            Route::Search => "search",
            Route::Library => "library",
            Route::Detail => "detail",
            Route::NowPlaying => "now-playing",
            Route::Settings => "settings",
            Route::Account => "account",
            Route::Diagnostics => "diagnostics",
            Route::Wifi => "wifi",
            Route::Bluetooth => "bluetooth",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "home" => Route::Home,
            "media" => Route::Media,
            "search" => Route::Search,
            "library" => Route::Library,
            "detail" => Route::Detail,
            "now-playing" => Route::NowPlaying,
            "settings" => Route::Settings,
            "account" => Route::Account,
            "diagnostics" => Route::Diagnostics,
            "wifi" => Route::Wifi,
            "bluetooth" => Route::Bluetooth,
            _ => return None,
        })
    }
}

/// The stack. Small on purpose: a television that can be eleven screens deep
/// is a television nobody can leave.
pub struct Stack {
    entries: Vec<Route>,
}

const DEPTH: usize = 6;

impl Stack {
    pub fn new() -> Self {
        Self {
            entries: vec![Route::Boot],
        }
    }

    pub fn current(&self) -> Route {
        self.entries.last().copied().unwrap_or(Route::Home)
    }

    /// Replaces the whole stack. Used once, when the shelves land and the boot
    /// screen is done with, and by Home.
    pub fn reset(&mut self, route: Route) {
        self.entries.clear();
        self.entries.push(route);
    }

    /// Opens a screen over this one.
    ///
    /// Pushing the screen that is already on the panel does nothing: a remote
    /// sends repeats, and a stack with Detail on it twice takes two presses of
    /// Back to leave.
    pub fn push(&mut self, route: Route) {
        if self.current() == route {
            return;
        }
        if self.entries.len() >= DEPTH {
            self.entries.remove(1);
        }
        self.entries.push(route);
    }

    /// Back. False when there was nothing behind this screen, which is the
    /// home screen's own signal to stay where it is.
    pub fn pop(&mut self) -> bool {
        if self.entries.len() <= 1 {
            return false;
        }
        self.entries.pop();
        true
    }

    pub fn depth(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_the_floor() {
        let mut stack = Stack::new();
        stack.reset(Route::Home);
        assert!(!stack.pop());
        assert_eq!(stack.current(), Route::Home);
    }

    #[test]
    fn back_finds_the_screen_underneath() {
        let mut stack = Stack::new();
        stack.reset(Route::Home);
        stack.push(Route::Search);
        stack.push(Route::Detail);
        assert!(stack.pop());
        assert_eq!(stack.current(), Route::Search);
        assert!(stack.pop());
        assert_eq!(stack.current(), Route::Home);
        assert!(!stack.pop());
    }

    #[test]
    fn a_repeat_does_not_deepen_the_stack() {
        let mut stack = Stack::new();
        stack.reset(Route::Home);
        stack.push(Route::Detail);
        stack.push(Route::Detail);
        assert_eq!(stack.depth(), 2);
    }

    #[test]
    fn the_stack_has_a_bottom_and_a_ceiling() {
        let mut stack = Stack::new();
        stack.reset(Route::Home);
        for route in [
            Route::Search,
            Route::Detail,
            Route::NowPlaying,
            Route::Settings,
            Route::Diagnostics,
            Route::Library,
            Route::Search,
        ] {
            stack.push(route);
        }
        assert!(stack.depth() <= DEPTH);
        while stack.pop() {}
        assert_eq!(stack.current(), Route::Home);
    }

    #[test]
    fn names_round_trip() {
        for route in [
            Route::Home,
            Route::Media,
            Route::Search,
            Route::Library,
            Route::Detail,
            Route::NowPlaying,
            Route::Settings,
            Route::Account,
            Route::Diagnostics,
        ] {
            assert_eq!(Route::from_name(route.name()), Some(route));
        }
    }
}
