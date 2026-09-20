//! Typing with five buttons.
//!
//! A television remote has Up, Down, Left, Right and Ok, so anywhere this
//! product asks for text it has to put the letters on the panel. The search
//! screen did that first and this is the part of it that is not about
//! searching: what a key is, what it is called, how wide it is drawn, and how
//! the remote walks a grid of them.
//!
//! It is here rather than in `screens::search` because the account screen needs
//! the same thing and a Wi-Fi password will need it after that. Three copies of
//! a letter grid is three places for the Turkish İ to be wrong.

/// What a key on the grid does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Letter(char),
    Space,
    /// Space on a grid that has no Shift; six columns rather than four, so
    /// the last row still spans ten.
    WideSpace,
    Backspace,
    Clear,
    /// Upper case for the next letter and every one after it, until pressed
    /// again. Passwords are case-sensitive, so a grid without this can only
    /// enter half of them.
    Shift,
}

impl Cap {
    /// What the key says. `shifted` only changes a letter's face; the wide
    /// keys are words and words do not shout.
    pub fn label(self, shifted: bool) -> String {
        match self {
            Cap::Letter(c) => {
                if shifted {
                    upper(c)
                } else {
                    c.to_string()
                }
            }
            Cap::Space | Cap::WideSpace => "Boşluk".into(),
            Cap::Backspace => "Sil".into(),
            Cap::Clear => "Temizle".into(),
            Cap::Shift => "Büyük".into(),
        }
    }

    /// Wide keys are drawn wide. The grid's columns are uniform, so a key that
    /// needs two of them says so here rather than in the interface.
    pub fn span(self) -> u32 {
        match self {
            Cap::Letter(_) => 1,
            // Two, two, two and four: one row of ten, like every other row,
            // so the columns line up all the way down.
            Cap::Shift | Cap::Backspace | Cap::Clear => 2,
            Cap::Space => 4,
            Cap::WideSpace => 6,
        }
    }
}

/// Turkish uppercase, which is not Unicode's default.
///
/// The dotted and dotless i are different letters here, and `char::to_uppercase`
/// maps both to "I". On the letter grid that produced two keys that looked
/// identical and did different things — visible in the first snapshot of the
/// search screen, and unusable.
pub fn upper(c: char) -> String {
    match c {
        'i' => "İ".into(),
        'ı' => "I".into(),
        other => other.to_uppercase().to_string(),
    }
}

/// The letters every screen types on.
///
/// One layout, because there were three: the account form and the Wi-Fi
/// password used one without a single Turkish letter in it, and the search
/// screen had its own with them. A product whose every other word is Turkish
/// could not type "Çalışma Odası" into two of its three text fields.
///
/// Ten columns rather than seven, which is what turned eight rows into seven:
/// on a remote the cost of a grid is the number of presses to cross it, and
/// height costs more than width because the eye travels further and the hand
/// repeats the same key. Digits get their own row and punctuation the next
/// two, instead of being scattered through the letters — the old grid put "@"
/// after "y" and split the digits across two rows, so finding one meant
/// reading the whole thing.
pub fn layout() -> Vec<Vec<Cap>> {
    let mut rows = letters();
    rows.push(vec![Cap::Shift, Cap::Space, Cap::Backspace, Cap::Clear]);
    rows
}

/// The same grid without Shift, for a field where case does not matter.
///
/// The search box is the only one: a catalogue lookup is case-insensitive, so
/// a Shift there is a key that changes nothing and one more thing for the
/// remote to walk past. Space takes the width it frees, so every row is still
/// ten columns wide.
pub fn layout_without_shift() -> Vec<Vec<Cap>> {
    let mut rows = letters();
    rows.push(vec![Cap::WideSpace, Cap::Backspace, Cap::Clear]);
    rows
}

fn letters() -> Vec<Vec<Cap>> {
    let row = |s: &str| -> Vec<Cap> { s.chars().map(Cap::Letter).collect() };
    vec![
        row("abcçdefgğh"),
        row("ıijklmnoöp"),
        row("rsştuüvyzq"),
        row("wx01234567"),
        row("89.@_-+!?#"),
        row("$%&*=/:;()"),
    ]
}

/// How many columns a grid is laid out at. Every row is exactly this wide,
/// counting spans: the panel draws at a fixed column width and a row that came
/// to more put a key off the edge of the television.
pub const COLUMNS: usize = 10;

/// What pressing a key means to whatever is holding the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Insert(char),
    Backspace,
    Clear,
    /// The grid changed its own case. Nothing to do to the text.
    Handled,
}

/// The letters, and where the remote is on them.
///
/// Navigation stops at the edges rather than wrapping: Right on the last column
/// jumping to the far side of the screen is not something anybody expects. What
/// happens when the remote walks off the top or the bottom is the caller's —
/// the grid reports that it could not move and the screen decides what is above
/// and below it.
pub struct Grid {
    rows: Vec<Vec<Cap>>,
    row: usize,
    col: usize,
    shifted: bool,
}

impl Grid {
    pub fn new(rows: Vec<Vec<Cap>>) -> Self {
        Self {
            rows,
            row: 0,
            col: 0,
            shifted: false,
        }
    }

    /// Letters, digits and the punctuation an address or a password is made of.
    ///
    /// ASCII rather than the search screen's Turkish alphabet, and deliberately:
    /// an e-mail address cannot contain ö, and a password containing one could
    /// not be typed on a keyboard that was missing the rest of ASCII anyway.
    /// The letters, ten to a row.
    ///
    /// Turkish first and complete — ç, ğ, ı, ö, ş, ü — in the alphabet's own
    /// order. The grid had none of them, on a product whose every other word
    /// is Turkish: a network called "Çalışma Odası" could not be typed at all,
    /// and neither could a password with a Turkish letter in it. q, w and x
    /// follow, because SSIDs and passwords are not written in one alphabet.
    ///
    /// Ten columns rather than seven, which is what turned eight rows into
    /// six: on a remote the cost of a grid is the number of presses to cross
    /// it, and height is more expensive than width because the eye has
    /// further to travel and the hand repeats the same key.
    ///
    /// Digits on their own row, punctuation on the next, and the four wide
    /// keys last. The old grid split the digits across two rows and scattered
    /// punctuation through the letters, so finding "." meant reading the
    /// whole thing.
    pub fn text() -> Self {
        Self::new(layout())
    }

    pub fn rows(&self) -> &[Vec<Cap>] {
        &self.rows
    }

    pub fn row(&self) -> usize {
        self.row
    }

    pub fn col(&self) -> usize {
        self.col
    }

    pub fn shifted(&self) -> bool {
        self.shifted
    }

    pub fn focused(&self) -> Option<Cap> {
        self.rows.get(self.row)?.get(self.col).copied()
    }

    /// Puts the remote on the first or the last row, for a screen handing focus
    /// in from above or below.
    pub fn enter(&mut self, from_below: bool) {
        self.row = if from_below {
            self.rows.len().saturating_sub(1)
        } else {
            0
        };
        self.col = self.col.min(self.width().saturating_sub(1));
    }

    fn width(&self) -> usize {
        self.rows.get(self.row).map(|row| row.len()).unwrap_or(0)
    }

    /// Moves within the grid. False means the edge, and the edge is the
    /// caller's business.
    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        if dx != 0 {
            let next = self.col as i32 + dx;
            if next < 0 || next as usize >= self.width() {
                return false;
            }
            self.col = next as usize;
            return true;
        }
        if dy != 0 {
            let next = self.row as i32 + dy;
            if next < 0 || next as usize >= self.rows.len() {
                return false;
            }
            self.row = next as usize;
            self.col = self.col.min(self.width().saturating_sub(1));
            return true;
        }
        false
    }

    pub fn at_top(&self) -> bool {
        self.row == 0
    }

    pub fn at_bottom(&self) -> bool {
        self.row + 1 >= self.rows.len()
    }

    /// Ok. Shift is answered here because it is the grid's own state.
    pub fn press(&mut self) -> Option<Edit> {
        Some(match self.focused()? {
            Cap::Letter(c) => {
                if self.shifted {
                    // One shift, one letter, like a phone: a remote has no way
                    // to hold a key down and a lock nobody can see is a lock
                    // that types the next word in capitals.
                    self.shifted = false;
                    let capital = upper(c);
                    Edit::Insert(capital.chars().next().unwrap_or(c))
                } else {
                    Edit::Insert(c)
                }
            }
            Cap::Space | Cap::WideSpace => Edit::Insert(' '),
            Cap::Backspace => Edit::Backspace,
            Cap::Clear => Edit::Clear,
            Cap::Shift => {
                self.shifted = !self.shifted;
                Edit::Handled
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid is drawn at a fixed column width, so a row that is wider than
    /// the grid puts a key off the edge of the television.
    #[test]
    fn every_row_fits_the_grid() {
        let grid = Grid::text();
        for row in grid.rows() {
            let span: u32 = row.iter().map(|cap| cap.span()).sum();
            assert_eq!(
                span as usize,
                COLUMNS,
                "{:?}",
                row.iter().map(|c| c.label(false)).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn an_address_can_be_typed_on_it() {
        let grid = Grid::text();
        let have: std::collections::HashSet<char> = grid
            .rows()
            .iter()
            .flatten()
            .filter_map(|cap| match cap {
                Cap::Letter(c) => Some(*c),
                _ => None,
            })
            .collect();
        for needed in "abcdefghijklmnopqrstuvwxyz0123456789@._-".chars() {
            assert!(have.contains(&needed), "no key for {needed:?}");
        }
    }

    #[test]
    fn the_turkish_i_is_two_letters() {
        assert_eq!(upper('i'), "İ");
        assert_eq!(upper('ı'), "I");
    }

    #[test]
    fn the_grid_never_steps_off_its_own_edges() {
        let mut grid = Grid::text();
        for _ in 0..20 {
            grid.step(0, -1);
            grid.step(-1, 0);
        }
        assert_eq!((grid.row(), grid.col()), (0, 0));
        assert!(grid.at_top());
        for _ in 0..40 {
            grid.step(0, 1);
            grid.step(1, 0);
        }
        assert!(grid.at_bottom());
        assert!(grid.focused().is_some());
    }

    /// A password is case-sensitive, so a grid that cannot reach capitals can
    /// only enter half of them.
    #[test]
    fn shift_capitalises_exactly_one_letter() {
        let mut grid = Grid::text();
        let shift = grid
            .rows()
            .iter()
            .enumerate()
            .find_map(|(r, row)| row.iter().position(|c| *c == Cap::Shift).map(|c| (r, c)))
            .expect("a shift key");
        assert_eq!(grid.press(), Some(Edit::Insert('a')));
        (grid.row, grid.col) = shift;
        assert_eq!(grid.press(), Some(Edit::Handled));
        assert!(grid.shifted());
        (grid.row, grid.col) = (0, 0);
        assert_eq!(grid.press(), Some(Edit::Insert('A')));
        assert!(!grid.shifted(), "the case must not stay on");
        assert_eq!(grid.press(), Some(Edit::Insert('a')));
    }

    #[test]
    fn a_key_says_what_it_does() {
        assert_eq!(Cap::Letter('a').label(false), "a");
        assert_eq!(Cap::Letter('a').label(true), "A");
        assert_eq!(Cap::Letter('@').label(true), "@");
        assert_eq!(Cap::Backspace.label(true), "Sil");
    }
}
