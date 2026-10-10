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
use crate::screens::wireless::{PeerChoice, PeerOp};

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

pub const CHOICES: [Choice; 4] = [
    Choice::Cancel,
    Choice::StandbyTelevision,
    Choice::Restart,
    Choice::Shutdown,
];

/// The power menu's rows, without the television's when CEC is off.
pub fn choices(television: bool) -> Vec<Choice> {
    CHOICES
        .into_iter()
        .filter(|choice| television || *choice != Choice::StandbyTelevision)
        .collect()
}

/// A sheet on the panel: the power menu, a yes/no over one decision, or where
/// a film the account was watching starts.
pub enum Sheet {
    Power {
        index: usize,
        /// Whether HDMI-CEC is on. Off, "Televizyonu kapat" is not on the
        /// sheet at all: it would only be refused.
        television: bool,
    },
    Confirm {
        question: String,
        action: Action,
        /// False is "Vazgeç" and is where the remote starts.
        yes: bool,
    },
    /// A film the account left part-way: from there, or from the start.
    /// The remote starts on carrying on, which is what a viewer coming back
    /// to a film nearly always wants.
    Resume {
        /// Where it was left, in seconds.
        at: u64,
        /// 0 is "Kaldığın yerden devam et", 1 is "Baştan başla".
        index: usize,
    },
    /// Which player the film opens in, when "Filmler ve Diziler > Ayarlar"
    /// says to ask. The remote starts on this interface's own.
    Player {
        /// Where it starts, already decided.
        start: u64,
        /// 0 is MediaBox, 1 is Kodi.
        index: usize,
    },
    /// What can be done to one remembered network or paired device: a long Ok
    /// on it, or Menu.
    Peer {
        title: String,
        choices: Vec<PeerChoice>,
        index: usize,
    },
    /// The yes/no over one of those choices: the ones that cut a link or lose
    /// a key. Starts on "Vazgeç", as every question here does.
    ConfirmPeer {
        question: String,
        op: PeerOp,
        yes: bool,
    },
}

impl Sheet {
    #[cfg(test)]
    pub fn power() -> Self {
        Self::power_with(true)
    }

    pub fn power_with(television: bool) -> Self {
        Sheet::Power { index: 0, television }
    }

    pub fn confirm(action: Action, question: &str) -> Self {
        Sheet::Confirm {
            question: question.to_string(),
            action,
            yes: false,
        }
    }

    pub fn resume(at: u64) -> Self {
        Sheet::Resume { at, index: 0 }
    }

    pub fn player(start: u64) -> Self {
        Sheet::Player { start, index: 0 }
    }

    pub fn peer(title: String, choices: Vec<PeerChoice>) -> Self {
        Sheet::Peer { title, choices, index: 0 }
    }

    pub fn title(&self) -> String {
        match self {
            Sheet::Power { .. } => "Güç".into(),
            Sheet::Confirm { question, .. } => question.clone(),
            Sheet::Resume { .. } => "Nereden başlasın?".into(),
            Sheet::Player { .. } => "Hangi oynatıcıda açılsın?".into(),
            Sheet::Peer { title, .. } => title.clone(),
            Sheet::ConfirmPeer { question, .. } => question.clone(),
        }
    }

    pub fn step(&mut self, dx: i32, dy: i32) -> bool {
        match self {
            Sheet::Power { index, television } => {
                if dy == 0 {
                    return false;
                }
                let count = choices(*television).len();
                let next = (*index as i32 + dy).clamp(0, count as i32 - 1) as usize;
                if next == *index {
                    return false;
                }
                *index = next;
                true
            }
            Sheet::Resume { index, .. } | Sheet::Player { index, .. } => {
                if dy == 0 {
                    return false;
                }
                let next = (*index as i32 + dy).clamp(0, 1) as usize;
                if next == *index {
                    return false;
                }
                *index = next;
                true
            }
            Sheet::Peer { choices, index, .. } => {
                if dy == 0 {
                    return false;
                }
                let next = (*index as i32 + dy).clamp(0, choices.len() as i32 - 1) as usize;
                if next == *index {
                    return false;
                }
                *index = next;
                true
            }
            Sheet::Confirm { yes, .. } | Sheet::ConfirmPeer { yes, .. } => {
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
            Sheet::Power { index, television } => {
                let choices = choices(*television);
                let choice = choices[(*index).min(choices.len() - 1)];
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
            Sheet::Peer { choices, index, .. } => {
                match choices.get(*index).and_then(|choice| choice.op.clone().map(|op| (op, choice))) {
                    None => Press::Stay,
                    Some((op, choice)) => match &choice.ask {
                        Some(question) => Press::AskPeer(op, question.clone()),
                        None => Press::Peer(op),
                    },
                }
            }
            Sheet::ConfirmPeer { op, yes, .. } => {
                if *yes {
                    Press::Peer(op.clone())
                } else {
                    Press::Close
                }
            }
            Sheet::Resume { at, index } => Press::Play(if *index == 0 { *at } else { 0 }),
            Sheet::Player { start, index } => {
                if *index == 0 {
                    Press::PlayHere(*start)
                } else {
                    Press::PlayKodi(*start)
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
    /// Start the film chosen on the page, from this second, in the player
    /// the settings say.
    Play(u64),
    /// Start it from this second in this interface's own player.
    PlayHere(u64),
    /// Start it from this second in Kodi.
    PlayKodi(u64),
    /// Nothing: a row that only says something.
    Stay,
    /// Do this to a network or a device.
    Peer(PeerOp),
    /// Ask this first, then do it.
    AskPeer(PeerOp, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_cec_off_the_power_sheet_has_no_television_row() {
        let mut sheet = Sheet::power_with(false);
        let mut seen = vec![sheet.press()];
        while sheet.step(0, 1) {
            seen.push(sheet.press());
        }
        assert!(!seen.contains(&Press::Do(Action::StandbyTelevision)), "{seen:?}");
        assert_eq!(seen.len(), CHOICES.len() - 1);
        let mut on = Sheet::power_with(true);
        on.step(0, 1);
        assert_eq!(on.press(), Press::Do(Action::StandbyTelevision));
    }

    #[test]
    fn a_network_menu_asks_before_forgetting_and_starts_the_question_on_no() {
        let choices = vec![
            PeerChoice {
                label: "İnternet bağlantısı",
                value: "Bağlı".into(),
                op: None,
                ask: None,
            },
            PeerChoice {
                label: "Ağı unut",
                value: String::new(),
                op: Some(PeerOp::WifiForget("Ev".into())),
                ask: Some("Ev unutulsun mu?".into()),
            },
        ];
        let mut sheet = Sheet::peer("Ev".into(), choices);
        assert_eq!(sheet.title(), "Ev");
        assert_eq!(sheet.press(), Press::Stay, "a fact does nothing when pressed");
        assert!(sheet.step(0, 1));
        let Press::AskPeer(op, question) = sheet.press() else {
            panic!("forgetting must ask");
        };
        let mut confirm = Sheet::ConfirmPeer { question, op, yes: false };
        assert_eq!(confirm.press(), Press::Close, "the question starts on Vazgeç");
        assert!(confirm.step(1, 0));
        assert_eq!(confirm.press(), Press::Peer(PeerOp::WifiForget("Ev".into())));
        assert!(!sheet.step(0, 1), "Ağı unut is the last row");
    }

    #[test]
    fn the_power_sheet_opens_on_the_harmless_row() {
        let sheet = Sheet::power();
        assert_eq!(sheet.press(), Press::Close);
    }

    #[test]
    fn a_film_left_part_way_asks_and_starts_on_carrying_on() {
        let mut sheet = Sheet::resume(754);
        assert_eq!(sheet.press(), Press::Play(754));
        assert!(!sheet.step(1, 0), "left and right are not its way");
        assert!(sheet.step(0, 1));
        assert_eq!(sheet.press(), Press::Play(0));
        assert!(!sheet.step(0, 1), "two rows only");
        assert!(sheet.step(0, -1));
        assert_eq!(sheet.press(), Press::Play(754));
    }

    #[test]
    fn asked_which_player_it_starts_on_this_one() {
        let mut sheet = Sheet::player(754);
        assert_eq!(sheet.press(), Press::PlayHere(754));
        assert!(sheet.step(0, 1));
        assert_eq!(sheet.press(), Press::PlayKodi(754));
        assert!(!sheet.step(0, 1), "two rows only");
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
                Press::Play(_) | Press::PlayHere(_) | Press::PlayKodi(_) => {
                    panic!("the power sheet never starts a film")
                }
                Press::Peer(_) | Press::AskPeer(..) | Press::Stay => {
                    panic!("the power sheet never touches a radio")
                }
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
