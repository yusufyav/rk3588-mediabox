//! Signing in to a Stremio account from the sofa.
//!
//! The web interface has had this since the beginning — `mediabox-ui`'s
//! `Account()` — and the control plane has always carried it: `MediaLogin`,
//! `MediaLogout`, and the provider block inside the status. What the native
//! shell was missing was the screen. This is that screen, and it is deliberately
//! the same account model as the web one rather than a second one: connected or
//! not, an address, a count of add-ons, and one button.
//!
//! The remote is the whole input. Two fields and one letter grid, because a
//! remote has one focus and a second grid would only be more to walk past — the
//! field the letters go into is whichever one the remote was last on.
//!
//! The password is held here as text because it has to be typed before it can
//! be sent, and it leaves here exactly once: into `MediaLogin`. It is never
//! drawn (see [`Account::password_mask`]), never logged, and cleared the moment
//! the attempt finishes, whichever way it went.

use serde_json::Value;

use crate::keyboard::{Edit, Grid};

/// Which field the letters are building.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Email,
    Password,
}

/// Where the remote is.
///
/// A flat top-to-bottom order — address, password, the grid, the button — so
/// Up and Down are the whole model and there is nowhere that Back is the only
/// way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Field(Field),
    Keys,
    Submit,
}

/// What the screen last heard about the account, from `/media/provider`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    pub authenticated: bool,
    pub email: String,
    pub addons: u64,
}

impl Session {
    /// Reads the provider block of a control-plane status.
    ///
    /// The same three fields the web interface reads, from the same place. An
    /// answer that does not carry them is a box that is not signed in, which is
    /// also what a box that has never been asked looks like.
    pub fn from_status(status: Option<&Value>) -> Self {
        let provider = status.and_then(|value| value.pointer("/media/provider"));
        Self {
            authenticated: provider
                .and_then(|p| p.get("authenticated"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            email: provider
                .and_then(|p| p.get("email"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            addons: provider
                .and_then(|p| p.get("addonCount"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        }
    }
}

pub struct Account {
    pub session: Session,
    pub focus: Focus,
    /// Which field the grid types into. Kept across a trip to the letters so
    /// that Back from the grid comes home to the field it was serving.
    pub field: Field,
    pub email: String,
    password: String,
    pub keys: Grid,
    /// True while a login or a logout is in flight. A second press must not
    /// send a second one.
    pub busy: bool,
    /// One short line under the button. Never carries the password, and never
    /// carries whatever the server said about it verbatim beyond its message.
    pub notice: String,
}

impl Account {
    pub fn new() -> Self {
        Self {
            session: Session::default(),
            focus: Focus::Field(Field::Email),
            field: Field::Email,
            email: String::new(),
            password: String::new(),
            keys: Grid::text(),
            busy: false,
            notice: String::new(),
        }
    }

    /// Opened from the settings screen. The form starts empty every time: a
    /// half-typed address from last week is not a convenience.
    pub fn open(&mut self, status: Option<&Value>) {
        self.session = Session::from_status(status);
        self.focus = Focus::Field(Field::Email);
        self.field = Field::Email;
        self.email.clear();
        self.forget_password();
        self.busy = false;
        self.notice.clear();
    }

    /// The only way the password leaves this struct, and the only caller is the
    /// login request itself.
    pub fn password(&self) -> &str {
        &self.password
    }

    /// Wipe it. Called after every attempt, successful or not.
    pub fn forget_password(&mut self) {
        self.password.clear();
        self.password.shrink_to_fit();
    }

    /// What the panel is allowed to draw for the password: its length, and
    /// nothing else.
    pub fn password_mask(&self) -> String {
        "•".repeat(self.password.chars().count())
    }

    pub fn can_submit(&self) -> bool {
        !self.busy && !self.email.trim().is_empty() && !self.password.is_empty()
    }

    /// Why the button is not available, for the line under it. Empty when it is.
    pub fn blocked_because(&self) -> &'static str {
        if self.busy {
            ""
        } else if self.email.trim().is_empty() {
            "E-posta girin"
        } else if self.password.is_empty() {
            "Parola girin"
        } else {
            ""
        }
    }

    fn text_mut(&mut self) -> &mut String {
        match self.field {
            Field::Email => &mut self.email,
            Field::Password => &mut self.password,
        }
    }

    /// Moves the remote. Returns whether anything changed.
    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        match self.focus {
            // Left and right do nothing on a field: there is one column here,
            // and a press that silently does nothing is better than one that
            // teleports.
            Focus::Field(Field::Email) => {
                if dy > 0 {
                    self.focus = Focus::Field(Field::Password);
                    self.field = Field::Password;
                    return true;
                }
                false
            }
            Focus::Field(Field::Password) => {
                if dy < 0 {
                    self.focus = Focus::Field(Field::Email);
                    self.field = Field::Email;
                    return true;
                }
                if dy > 0 {
                    self.keys.enter(false);
                    self.focus = Focus::Keys;
                    return true;
                }
                false
            }
            Focus::Keys => {
                if self.keys.step(dx, dy) {
                    return true;
                }
                // Off the top of the grid is the field it is typing into; off
                // the bottom is the button. Both are where a viewer is looking.
                if dy < 0 && self.keys.at_top() {
                    self.focus = Focus::Field(self.field);
                    return true;
                }
                if dy > 0 && self.keys.at_bottom() {
                    self.focus = Focus::Submit;
                    return true;
                }
                false
            }
            Focus::Submit => {
                if dy < 0 {
                    self.keys.enter(true);
                    self.focus = Focus::Keys;
                    return true;
                }
                false
            }
        }
    }

    /// Ok. `Submit` is the caller's cue to send the login; everything else is
    /// answered here.
    pub fn press(&mut self) -> Press {
        match self.focus {
            // Ok on a field is "type into this one": it chooses the field and
            // puts the remote on the letters in one press.
            Focus::Field(field) => {
                self.field = field;
                self.keys.enter(false);
                self.focus = Focus::Keys;
                Press::Changed
            }
            Focus::Keys => {
                let Some(edit) = self.keys.press() else {
                    return Press::Nothing;
                };
                match edit {
                    Edit::Handled => {}
                    Edit::Insert(c) => {
                        let text = self.text_mut();
                        // A cap, so that a stuck remote cannot grow a string
                        // without end and so that what goes over the socket is
                        // bounded.
                        if text.chars().count() < 128 {
                            text.push(c);
                        }
                    }
                    Edit::Backspace => {
                        self.text_mut().pop();
                    }
                    Edit::Clear => self.text_mut().clear(),
                }
                self.notice.clear();
                Press::Changed
            }
            Focus::Submit => {
                if self.busy {
                    return Press::Nothing;
                }
                let reason = self.blocked_because();
                if !reason.is_empty() {
                    self.notice = reason.to_string();
                    return Press::Changed;
                }
                Press::SignIn
            }
        }
    }

    /// Back, from inside the screen. False means there is nothing left to back
    /// out of and the route should pop — which is how the settings screen is
    /// reached again, and why this is never a trap.
    pub fn dismiss(&mut self) -> bool {
        match self.focus {
            Focus::Keys | Focus::Submit => {
                self.focus = Focus::Field(self.field);
                true
            }
            Focus::Field(_) => false,
        }
    }

    pub fn begin(&mut self, what: &str) {
        self.busy = true;
        self.notice = what.to_string();
    }

    /// The answer to a login. The password goes either way.
    pub fn signed_in(&mut self, status: Option<&Value>) {
        self.busy = false;
        self.forget_password();
        self.session = Session::from_status(status);
        self.notice = if self.session.authenticated {
            "Hesap bağlandı".into()
        } else {
            String::new()
        };
        self.focus = Focus::Field(Field::Email);
        self.field = Field::Email;
    }

    pub fn failed(&mut self, why: &str) {
        self.busy = false;
        self.forget_password();
        // Whatever went wrong, the thing that was typed is not part of the
        // explanation.
        self.notice = format!("Giriş başarısız: {why}");
        self.focus = Focus::Field(Field::Email);
        self.field = Field::Email;
    }

    /// A fresh status while the screen is open, from the timer that already
    /// polls the control plane. It must not move the remote or wipe the form.
    pub fn refresh(&mut self, status: Option<&Value>) -> bool {
        if self.busy {
            return false;
        }
        let session = Session::from_status(status);
        if session == self.session {
            return false;
        }
        self.session = session;
        true
    }
}

/// What Ok meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Nothing,
    Changed,
    SignIn,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn connected() -> Value {
        json!({"media": {"provider": {
            "authenticated": true, "email": "someone@example.com", "addonCount": 7
        }}})
    }

    fn typed(account: &mut Account, text: &str) {
        for wanted in text.chars() {
            let found = account
                .keys
                .rows()
                .iter()
                .enumerate()
                .find_map(|(r, row)| {
                    row.iter()
                        .position(|cap| *cap == crate::keyboard::Cap::Letter(wanted))
                        .map(|c| (r, c))
                })
                .unwrap_or_else(|| panic!("no key for {wanted:?}"));
            account.focus = Focus::Keys;
            account.keys.enter(false);
            while account.keys.row() < found.0 {
                account.keys.step(0, 1);
            }
            while account.keys.col() < found.1 {
                account.keys.step(1, 0);
            }
            while account.keys.col() > found.1 {
                account.keys.step(-1, 0);
            }
            account.press();
        }
    }

    #[test]
    fn a_box_that_was_never_asked_is_not_signed_in() {
        let account = Account::new();
        assert!(!account.session.authenticated);
        assert!(!Session::from_status(None).authenticated);
        assert!(!Session::from_status(Some(&json!({}))).authenticated);
    }

    #[test]
    fn a_connected_box_carries_the_address_and_the_count() {
        let session = Session::from_status(Some(&connected()));
        assert!(session.authenticated);
        assert_eq!(session.email, "someone@example.com");
        assert_eq!(session.addons, 7);
    }

    /// The whole reason the screen exists: an address has to be enterable with
    /// five buttons.
    #[test]
    fn an_address_can_be_typed_with_the_remote() {
        let mut account = Account::new();
        typed(&mut account, "a@b.c");
        assert_eq!(account.email, "a@b.c");
    }

    #[test]
    fn the_password_is_never_drawn() {
        let mut account = Account::new();
        account.field = Field::Password;
        typed(&mut account, "hunter2");
        assert_eq!(account.password(), "hunter2");
        assert_eq!(account.password_mask(), "•••••••");
        assert!(!account.password_mask().contains('h'));
        assert_eq!(account.password_mask().chars().count(), 7);
    }

    /// Whatever happened, what was typed does not stay in the process.
    #[test]
    fn the_password_is_forgotten_after_every_attempt() {
        for outcome in 0..2 {
            let mut account = Account::new();
            account.field = Field::Password;
            typed(&mut account, "abc");
            assert!(!account.password().is_empty());
            if outcome == 0 {
                account.signed_in(Some(&connected()));
            } else {
                account.failed("yanlış parola");
            }
            assert!(account.password().is_empty(), "outcome {outcome}");
            assert!(account.password_mask().is_empty());
            assert!(!account.busy);
        }
    }

    #[test]
    fn a_failure_never_repeats_what_was_typed() {
        let mut account = Account::new();
        account.field = Field::Password;
        typed(&mut account, "abc");
        account.failed("kimlik doğrulanamadı");
        assert!(!account.notice.contains("abc"));
        assert!(account.notice.contains("kimlik doğrulanamadı"));
    }

    #[test]
    fn an_empty_credential_does_not_send_a_login() {
        let mut account = Account::new();
        account.focus = Focus::Submit;
        assert_eq!(account.press(), Press::Changed);
        assert_eq!(account.notice, "E-posta girin");
        assert!(!account.can_submit());

        typed(&mut account, "a@b.c");
        account.focus = Focus::Submit;
        assert_eq!(account.press(), Press::Changed);
        assert_eq!(account.notice, "Parola girin");

        account.field = Field::Password;
        typed(&mut account, "x");
        assert!(account.can_submit());
        account.focus = Focus::Submit;
        assert_eq!(account.press(), Press::SignIn);
    }

    #[test]
    fn a_second_press_while_busy_sends_nothing() {
        let mut account = Account::new();
        typed(&mut account, "a@b.c");
        account.field = Field::Password;
        typed(&mut account, "x");
        account.begin("Bağlanıyor…");
        account.focus = Focus::Submit;
        assert_eq!(account.press(), Press::Nothing);
        assert!(!account.can_submit());
    }

    /// Up and down, top to bottom, and every stop reachable from the one above.
    #[test]
    fn the_remote_walks_the_screen_top_to_bottom_and_back() {
        let mut account = Account::new();
        assert_eq!(account.focus, Focus::Field(Field::Email));
        assert!(account.step(0, 1));
        assert_eq!(account.focus, Focus::Field(Field::Password));
        assert!(account.step(0, 1));
        assert_eq!(account.focus, Focus::Keys);
        for _ in 0..40 {
            account.step(0, 1);
        }
        assert_eq!(account.focus, Focus::Submit);
        assert!(account.step(0, -1));
        assert_eq!(account.focus, Focus::Keys);
        // Up off the top of the grid comes back to the field it was typing
        // into, which is the one the walk down came through.
        while account.focus == Focus::Keys {
            assert!(account.step(0, -1));
        }
        assert_eq!(account.focus, Focus::Field(Field::Password));
        assert!(account.step(0, -1));
        assert_eq!(account.focus, Focus::Field(Field::Email));
        assert!(!account.step(0, -1), "nothing above the first field");
    }

    /// Back gets out of the letters, and then out of the screen. Neither is a
    /// surprise and neither is a trap.
    #[test]
    fn back_leaves_the_letters_before_it_leaves_the_screen() {
        let mut account = Account::new();
        account.field = Field::Password;
        account.focus = Focus::Keys;
        assert!(account.dismiss(), "the first Back stays on the screen");
        assert_eq!(account.focus, Focus::Field(Field::Password));
        assert!(!account.dismiss(), "the second Back pops the route");

        account.focus = Focus::Submit;
        assert!(account.dismiss());
        assert_eq!(account.focus, Focus::Field(Field::Password));
    }

    /// Ok on a field is "type into this one".
    #[test]
    fn choosing_a_field_puts_the_remote_on_the_letters() {
        let mut account = Account::new();
        account.focus = Focus::Field(Field::Password);
        assert_eq!(account.press(), Press::Changed);
        assert_eq!(account.focus, Focus::Keys);
        assert_eq!(account.field, Field::Password);
        typed(&mut account, "z");
        assert_eq!(account.password(), "z");
        assert!(
            account.email.is_empty(),
            "the letters went to one field only"
        );
    }

    /// The screen is polled while it is open. That must not move the remote or
    /// throw away half a typed address.
    #[test]
    fn a_refresh_does_not_disturb_a_form_being_filled() {
        let mut account = Account::new();
        typed(&mut account, "a@b.c");
        account.focus = Focus::Submit;
        assert!(account.refresh(Some(&connected())));
        assert_eq!(account.email, "a@b.c");
        assert_eq!(account.focus, Focus::Submit);
        assert!(
            !account.refresh(Some(&connected())),
            "no change, no repaint"
        );
    }

    #[test]
    fn a_refresh_is_ignored_while_a_login_is_in_flight() {
        let mut account = Account::new();
        account.begin("Bağlanıyor…");
        assert!(!account.refresh(Some(&connected())));
        assert!(!account.session.authenticated);
    }

    #[test]
    fn opening_the_screen_starts_from_empty() {
        let mut account = Account::new();
        typed(&mut account, "a@b.c");
        account.field = Field::Password;
        typed(&mut account, "secret");
        account.open(Some(&connected()));
        assert!(account.email.is_empty());
        assert!(account.password().is_empty());
        assert_eq!(account.focus, Focus::Field(Field::Email));
        assert!(account.session.authenticated);
    }
}
