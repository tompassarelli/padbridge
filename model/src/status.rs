use serde::{Deserialize, Serialize};

use crate::ControllerSettings;

/// Everything the service currently knows. Sent whole on every change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The followed controller; `None` while none is plugged in.
    pub pad: Option<Pad>,
    /// The followed Warcraft III; `None` while it isn't running.
    pub game: Option<Game>,
    /// The map session the active profile follows; `None` while there is none.
    pub session: Option<Session>,
    /// The profile running now.
    pub profile: Profile,
    /// What the player chose; `Auto` runs the map plug-in during its session, else Any map.
    pub choice: ProfileChoice,
    /// The map plug-in's title as players know it, e.g. "Hero Arena"; `None` without a plug-in.
    #[serde(default)]
    pub map: Option<String>,
    pub output: Output,
    /// One plain-language sentence for the player, when something needs them.
    pub problem: Option<String>,
    pub settings: ControllerSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pad {
    /// The name the device reports, e.g. "Xbox Wireless Controller".
    pub name: String,
    /// Stable identity across reconnects (Linux: the by-id link).
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    pub pid: u32,
    /// Its game window exists.
    pub window: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The map's name as players know it, e.g. "Hero Arena".
    pub map: String,
    pub phase: Phase,
    /// Player number as the game shows it (1-based), when known.
    pub player: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Lobby,
    CharacterSelect,
    StageSelect,
    Match,
    Results,
}

/// Whether presses currently reach the game.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// The active profile's key output is running.
    pub running: bool,
    /// It has finished starting and accepts presses.
    pub ready: bool,
    /// The followed Warcraft III window has keyboard focus.
    pub focused: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// The map plug-in's session: its helper, keys and menu pointer.
    #[default]
    Map,
    /// Buttons and sticks to configurable keys and mouse, for any map.
    AnyMap,
    /// Follow the pad and the game, press nothing.
    Off,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::Map, Profile::AnyMap, Profile::Off];

    pub fn label(self) -> &'static str {
        match self {
            Profile::Map => "Map plug-in",
            Profile::AnyMap => "Any map",
            Profile::Off => "Off",
        }
    }
}

/// The player's profile choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileChoice {
    #[default]
    Auto,
    Map,
    AnyMap,
    Off,
}

impl ProfileChoice {
    /// The profile this choice runs, given whether the map plug-in follows a session.
    pub fn resolve(self, map_session: bool) -> Profile {
        match self {
            ProfileChoice::Auto if map_session => Profile::Map,
            ProfileChoice::Auto => Profile::AnyMap,
            ProfileChoice::Map => Profile::Map,
            ProfileChoice::AnyMap => Profile::AnyMap,
            ProfileChoice::Off => Profile::Off,
        }
    }
}
