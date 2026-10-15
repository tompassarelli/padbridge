use crate::model;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where the helper delivers what it types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Warcraft III's window on an X11 display, under niri.
    Window { display: String, x11_window: u32, niri_window: u64, niri_socket: PathBuf },
    /// A stand-in game with no window: the helper appends its typing to this file.
    Headless { text_out: PathBuf },
}

/// One running Warcraft III and its window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    pub pid: u32,
    /// The process start time in clock ticks: a reused PID is another game.
    pub birth: u64,
    /// Documents/Warcraft III in the game's Wine prefix.
    pub documents: PathBuf,
    pub target: Target,
}

/// The controller, found by its stable link or event node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pad {
    pub link: PathBuf,
    /// The event node the link points at now.
    pub device: PathBuf,
    pub name: String,
}

/// What the map currently asks of the helper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The helper is replaced when this changes.
    pub key: String,
    /// For people, as in "Hero Arena 1.2, player 1, hero selection".
    pub summary: String,
    /// What a window shows of it.
    pub shown: Option<model::Session>,
}

/// What a helper's log line says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperEvent {
    /// Waiting for a match, menus driven by the controller.
    Ready,
    InMatch,
    Focus(bool),
    PadLost,
    PadBack,
}

/// The helper process that serves a session: the service appends the fighter
/// layout's settings (`--preset`, `--tap-jump`, `--left-trigger`,
/// `--right-trigger`, `--right-stick`, and `--remaps` when any) and reads its standard error line by line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Helper {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// What one helper log line means.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Line {
    pub event: Option<HelperEvent>,
    /// A sentence for the player: why the helper stopped.
    pub problem: Option<String>,
}

/// A map plug-in: the session the map publishes and the helper that serves it.
/// Implement it in Rust and run it in this process, or behind
/// [`plugin::serve`](super::plugin::serve) as a separate program (README, "Map plug-ins").
pub trait Profile {
    /// A short machine name, e.g. `hero-arena`.
    fn name(&self) -> &str;
    /// The map's name as players know it; empty for no map.
    fn title(&self) -> &str;
    /// The map's current session in this game, if it runs one.
    fn session(&mut self, game: &Game) -> Option<Session>;
    /// The helper for this game, pad and session; `None` when the session needs none.
    fn helper(&mut self, game: &Game, pad: &Pad, session: &Session) -> Option<Helper>;
    /// What a line the helper wrote means.
    fn line(&mut self, _line: &str) -> Line {
        Line::default()
    }
    /// Whether the map shows a menu the pad drives with the desktop pointer now.
    fn pointer_menu(&mut self) -> bool {
        false
    }
    /// Whether the session plays on keys: no helper; the service presses the fighter layout itself.
    fn keys(&mut self) -> bool {
        false
    }
}

/// No map plug-in: every map plays with the Any map profile.
pub struct NoMap;

impl Profile for NoMap {
    fn name(&self) -> &str {
        "none"
    }
    fn title(&self) -> &str {
        ""
    }
    fn session(&mut self, _game: &Game) -> Option<Session> {
        None
    }
    fn helper(&mut self, _game: &Game, _pad: &Pad, _session: &Session) -> Option<Helper> {
        None
    }
}
