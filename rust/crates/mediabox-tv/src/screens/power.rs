//! The sheet that stands between a key and the appliance's power state.
//!
//! An appliance restarted because somebody pressed Ok is the fault this whole
//! milestone opened with. It was caused outside this process — systemd-logind
//! was acting on the HDMI block's CEC remote — but the shape of the answer is
//! the same on both sides of that line: nothing that ends the session happens
//! on one press, and the press that would is never the one focus starts on.
//!
//! So the sheet has its own focus scope, its first row is always the harmless
//! one, and the two destructive rows each open a second sheet that asks.

use crate::screens::settings::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Close the sheet and change nothing.
    Cancel,
    /// Send the television to standby over CEC. The appliance keeps running.
    StandbyTelevision,
    Restart,
    Shutdown,
}

impl Choice {
    pub fn label(self) -> &'static str {
        match self {
            Choice::Cancel => "Vazgeç",
            Choice::StandbyTelevision => "Televizyonu kapat",
            Choice::Restart => "Cihazı yeniden başlat",
            Choice::Shutdown => "Cihazı kapat",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Choice::Cancel => "Bu ekrana dön",
            Choice::StandbyTelevision => "CEC ile beklemeye alır, cihaz açık kalır",
            Choice::Restart => "Açık olan her şey kapanır",
            Choice::Shutdown => "Cihaz tamamen kapanır",
        }
    }

    pub fn destructive(self) -> bool {
        matches!(self, Choice::Restart | Choice::Shutdown)
    }

    pub fn action(self) -> Option<Action> {
        match self {
            Choice::Cancel => None,
            Choice::StandbyTelevision => Some(Action::StandbyTelevision),
            Choice::Restart => Some(Action::Restart),
            Choice::Shutdown => Some(Action::Shutdown),
        }
    }
}

pub const CHOICES: [Choice; 4] =
    [Choice::Cancel, Choice::StandbyTelevision, Choice::Restart, Choice::Shutdown];

/// A sheet on the panel. Either the power menu, or a yes/no over one decision.
pub enum Sheet {
    Power {
        index: usize,
    },
    Confirm {
        question: String,
        action: Action,
        /// False is "Vazgeç" and is where the remote starts.
        yes: bool,
    },
}

impl Sheet {
    pub fn power() -> Self {
        Sheet::Power { index: 0 }
    }

    pub fn confirm(action: Action, question: &str) -> Self {
        Sheet::Confirm { question: question.to_string(), action, yes: false }
    }

    pub fn title(&self) -> String {
        match self {
            Sheet::Power { .. } => "Güç".into(),
            Sheet::Confirm { question, .. } => question.clone(),
        }
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        match self {
            Sheet::Power { index } => {
                if dy == 0 {
                    return false;
                }
                let next = (*index as i32 + dy).clamp(0, CHOICES.len() as i32 - 1) as usize;
                if next == *index {
                    return false;
                }
                *index = next;
                true
            }
            Sheet::Confirm { yes, .. } => {
                if dx == 0 {
                    return false;
                }
                let next = dx > 0;
                if next == *yes {
                    return false;
                }
                *yes = next;
                true
            }
        }
    }

    /// What Ok on this sheet means. `Ask` opens a second sheet; `Do` is the
    /// only way an action ever leaves this module, and it can only be reached
    /// from a confirmation whose focus was deliberately moved to "Evet".
    pub fn press(&self) -> Press {
        match self {
            Sheet::Power { index } => {
                let choice = CHOICES[(*index).min(CHOICES.len() - 1)];
                match choice.action() {
                    None => Press::Close,
                    Some(action) if choice.destructive() => {
                        Press::Ask(action, action.question().to_string())
                    }
                    Some(action) => Press::Do(action),
                }
            }
            Sheet::Confirm { action, yes, .. } => {
                if *yes {
                    Press::Do(*action)
                } else {
                    Press::Close
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Press {
    Close,
    Ask(Action, String),
    Do(Action),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_power_sheet_opens_on_the_harmless_row() {
        let sheet = Sheet::power();
        assert_eq!(sheet.press(), Press::Close);
    }

    #[test]
    fn a_confirmation_opens_on_no() {
        let sheet = Sheet::confirm(Action::Restart, "?");
        assert_eq!(sheet.press(), Press::Close);
    }

    /// One press of Ok, anywhere on either sheet, must never restart the box.
    #[test]
    fn nothing_destructive_happens_on_the_first_press() {
        let mut sheet = Sheet::power();
        for _ in 0..CHOICES.len() {
            match sheet.press() {
                Press::Do(action) => {
                    assert!(!Action::confirms(action));
                    assert!(!matches!(action, Action::Restart | Action::Shutdown));
                }
                Press::Ask(action, question) => {
                    assert!(matches!(action, Action::Restart | Action::Shutdown));
                    assert!(!question.is_empty());
                }
                Press::Close => {}
            }
            sheet.step(0, 1);
        }
    }

    #[test]
    fn restarting_takes_three_deliberate_presses() {
        // Power key -> sheet; two steps down to "Cihazı yeniden başlat"; Ok
        // asks; a step right to "Evet"; Ok does.
        let mut sheet = Sheet::power();
        sheet.step(0, 1);
        sheet.step(0, 1);
        let Press::Ask(action, question) = sheet.press() else {
            panic!("a destructive row must ask");
        };
        assert_eq!(action, Action::Restart);
        let mut confirm = Sheet::confirm(action, &question);
        assert_eq!(confirm.press(), Press::Close);
        confirm.step(1, 0);
        assert_eq!(confirm.press(), Press::Do(Action::Restart));
    }

    #[test]
    fn standby_is_the_television_and_not_the_appliance() {
        let mut sheet = Sheet::power();
        sheet.step(0, 1);
        assert_eq!(sheet.press(), Press::Do(Action::StandbyTelevision));
    }

    #[test]
    fn the_sheet_never_steps_off_its_own_list() {
        let mut sheet = Sheet::power();
        for _ in 0..20 {
            sheet.step(0, 1);
        }
        assert!(matches!(sheet.press(), Press::Ask(Action::Shutdown, _)));
        for _ in 0..20 {
            sheet.step(0, -1);
        }
        assert_eq!(sheet.press(), Press::Close);
    }
}
