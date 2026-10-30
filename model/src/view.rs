use serde::{Deserialize, Serialize};

use crate::{Game, Phase, Profile, Session, Snapshot};

/// The window's connection to the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    /// No service is running and the player hasn't turned it on.
    Off,
    /// Starting it, or waiting for its first status.
    Starting,
    Connected,
}

/// Status light colours. `Off` is grey: nothing to report, as with the service off or no controller (keyboard play needs none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Light {
    Green,
    Off,
    Amber,
    Red,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub label: String,
    pub text: String,
    pub light: Light,
}

/// The three status rows a window shows, plus the overall light.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub rows: [Row; 3],
    pub overall: Light,
    pub problem: Option<String>,
}

fn row(label: &str, text: impl Into<String>, light: Light) -> Row {
    Row {
        label: label.to_owned(),
        text: text.into(),
        light,
    }
}

const CLICK_GAME: &str = "Click the Warcraft III window to play.";

fn phase_text(session: &Session) -> String {
    match session.phase {
        Phase::Lobby => "In the lobby".to_owned(),
        Phase::CharacterSelect => "Choosing fighters".to_owned(),
        Phase::StageSelect => "Choosing a stage".to_owned(),
        Phase::Results => "Match over".to_owned(),
        Phase::Match => match session.player {
            Some(n) => format!("In a match as Player {n}"),
            None => "In a match".to_owned(),
        },
    }
}

fn session_row(s: &Snapshot) -> Row {
    let label = session_label(s);
    if s.profile == Profile::Off {
        return row(label, "Off: your controller doesn't press anything", Light::Amber);
    }
    if s.game.is_none() {
        return row(label, "No match running", Light::Amber);
    }
    let (doing, followed) = match (s.profile, &s.session) {
        (Profile::Map, None) => return row(label, format!("No {} match yet", s.map.as_deref().unwrap_or("map")), Light::Amber),
        (Profile::Map, Some(session)) => (phase_text(session), true),
        _ => ("Your controller works in Warcraft III".to_owned(), false),
    };
    if !s.output.running || !s.output.ready {
        return row(label, format!("{doing}. Getting your controller ready…"), Light::Amber);
    }
    if !s.output.focused {
        let text = if followed {
            format!("{doing}. {CLICK_GAME}")
        } else {
            CLICK_GAME.to_owned()
        };
        return row(label, text, Light::Amber);
    }
    row(label, doing, Light::Green)
}

fn session_label(s: &Snapshot) -> &'static str {
    match s.profile {
        Profile::Off => "Controller output",
        Profile::Map | Profile::AnyMap => "Match",
    }
}

/// Plain-language status for a window. Product words only.
pub fn view(link: Link, s: &Snapshot) -> View {
    let rows = match link {
        Link::Off => [
            row("Controller", "Controller support is off", Light::Off),
            row("Warcraft III", "Not checked", Light::Off),
            row(session_label(s), "Not checked", Light::Off),
        ],
        Link::Starting => [
            row("Controller", "Turning on controller support…", Light::Amber),
            row("Warcraft III", "Checking…", Light::Amber),
            row(session_label(s), "Checking…", Light::Amber),
        ],
        Link::Connected => [
            match &s.pad {
                Some(pad) => row("Controller", format!("{} connected", pad.name), Light::Green),
                None => row(
                    "Controller",
                    "No controller connected. Keyboard play needs none.",
                    Light::Off,
                ),
            },
            match &s.game {
                Some(Game { window: true, .. }) => row("Warcraft III", "Running", Light::Green),
                Some(_) => row("Warcraft III", "Starting", Light::Amber),
                None => row("Warcraft III", "Not running", Light::Amber),
            },
            session_row(s),
        ],
    };
    let problem = if link == Link::Connected { s.problem.clone() } else { None };
    let keyboard_only = link == Link::Connected && s.pad.is_none();
    let counted = if keyboard_only { &rows[1..] } else { &rows[..] };
    let worst = counted.iter().map(|r| r.light).max().unwrap_or(Light::Off);
    let overall = if problem.is_some() { Light::Red } else { worst };
    View { rows, overall, problem }
}
