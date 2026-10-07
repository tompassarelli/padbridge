//! What the controller service knows, the messages on its local interface, and
//! the plain-language status a window shows for it. Map-agnostic except for the
//! profiles' binding tables. No I/O: the service and every window share it.
#![forbid(unsafe_code)]

pub mod any_map;
pub mod pad;

use serde::{Deserialize, Serialize};

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
    /// What the player chose; `Auto` runs Smashcraft during a Smashcraft session, else Any map.
    pub choice: ProfileChoice,
    pub output: Output,
    /// One plain-language sentence for the player, when something needs them.
    pub problem: Option<String>,
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
    /// The map's name as players know it, e.g. "Smashcraft".
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
    /// Smashcraft's own input protocol and menu control.
    #[default]
    Smashcraft,
    /// Buttons and sticks to configurable keys and mouse, for any map.
    AnyMap,
    /// Follow the pad and the game, press nothing.
    Off,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::Smashcraft, Profile::AnyMap, Profile::Off];

    pub fn label(self) -> &'static str {
        match self {
            Profile::Smashcraft => "Smashcraft",
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
    Smashcraft,
    AnyMap,
    Off,
}

impl ProfileChoice {
    /// The profile this choice runs, given whether a Smashcraft session is followed.
    pub fn resolve(self, smashcraft_session: bool) -> Profile {
        match self {
            ProfileChoice::Auto if smashcraft_session => Profile::Smashcraft,
            ProfileChoice::Auto => Profile::AnyMap,
            ProfileChoice::Smashcraft => Profile::Smashcraft,
            ProfileChoice::AnyMap => Profile::AnyMap,
            ProfileChoice::Off => Profile::Off,
        }
    }
}

/// Live pad state for display. Axes are SDL-normalized: -32768..=32767, and a
/// stick's y is negative when pushed up. Triggers are 0..=32767.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputView {
    pub left: [i16; 2],
    pub right: [i16; 2],
    pub lt: u16,
    pub rt: u16,
    /// One bit per [`Button`], at `1 << (button as u32)`.
    pub buttons: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    A,
    B,
    X,
    Y,
    Lb,
    Rb,
    Start,
    Back,
    LeftStick,
    RightStick,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl Button {
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }
}

impl InputView {
    pub fn pressed(&self, button: Button) -> bool {
        self.buttons & button.bit() != 0
    }
    pub fn press(&mut self, button: Button, down: bool) {
        if down {
            self.buttons |= button.bit();
        } else {
            self.buttons &= !button.bit();
        }
    }
}

/// Service to window, one JSON object per line: `{"status":{...}}` or `{"input":{...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceMessage {
    Status(Snapshot),
    Input(InputView),
}

/// Window to service, one JSON object per line, e.g. `{"profile":"any_map"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    Profile(ProfileChoice),
    PadPreset(PadPreset),
    TapJump(bool),
    /// Replaces the Any map profile's bindings.
    Bindings(Vec<Binding>),
}

impl ServiceMessage {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("status serializes") + "\n"
    }
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim_end())
    }
}

impl ClientMessage {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("message serializes") + "\n"
    }
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim_end())
    }
}

/// A physical control, with each stick direction separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    A,
    B,
    X,
    Y,
    Lb,
    Rb,
    Lt,
    Rt,
    Start,
    Back,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    LeftUp,
    LeftDown,
    LeftLeft,
    LeftRight,
    RightUp,
    RightDown,
    RightLeft,
    RightRight,
}

/// What a control does in the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Press {
    /// A keyboard key by its name: a letter or digit, or one of `space`,
    /// `escape`, `tab`, `enter`, `up`, `down`, `left`, `right`, `f1`..`f12`.
    Key(String),
    LeftClick,
    RightClick,
    /// Moves the mouse pointer in the stick's direction.
    Pointer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub control: Control,
    /// What it does, in the map's words, e.g. "Attack".
    pub action: String,
    pub press: Press,
}

fn bind(control: Control, action: &str, press: Press) -> Binding {
    Binding {
        control,
        action: action.to_owned(),
        press,
    }
}

fn key(name: &str) -> Press {
    Press::Key(name.to_owned())
}

/// Smashcraft's fixed layout (Xbox labels), as the README's mapping table.
pub fn smashcraft_bindings() -> Vec<Binding> {
    smashcraft_bindings_for(PadPreset::Standard)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PadPreset {
    #[default]
    Standard,
    ZJump,
}

impl PadPreset {
    pub fn name(self) -> &'static str {
        match self { Self::Standard => "standard", Self::ZJump => "z-jump" }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        match name { "standard" => Ok(Self::Standard), "z-jump" => Ok(Self::ZJump), _ => Err(format!("unknown pad preset {name:?}: standard or z-jump")) }
    }
}

pub fn smashcraft_bindings_for(preset: PadPreset) -> Vec<Binding> {
    use Control::*;
    vec![
        bind(A, "Attack", key("n")),
        bind(X, "Special", key("u")),
        bind(B, if preset == PadPreset::ZJump { "Grab" } else { "Jump" }, key(if preset == PadPreset::ZJump { "o" } else { "i" })),
        bind(Y, "Jump", key("i")),
        bind(Rb, if preset == PadPreset::ZJump { "Jump" } else { "Grab" }, key(if preset == PadPreset::ZJump { "i" } else { "o" })),
        bind(Lb, "Tilt", key("p")),
        bind(Lt, "Light shield", key("9")),
        bind(Rt, "Shield", key("q")),
        bind(Start, "Pause", key("y")),
        bind(LeftLeft, "Move left", key("w")),
        bind(LeftRight, "Move right", key("r")),
        bind(LeftDown, "Crouch / fast fall", key("e")),
        bind(LeftUp, "Up (aim, up special)", key("space")),
        bind(RightUp, "Up smash", key("j")),
        bind(RightDown, "Down smash", key("h")),
        bind(RightLeft, "Smash left", key("b")),
        bind(RightRight, "Smash right", key("m")),
    ]
}

/// Smashcraft's fighter, stage and results menus: the left stick moves the
/// pointer, as a hand cursor does in Smash, and the face buttons click.
pub fn smashcraft_menu_bindings() -> Vec<Binding> {
    use Control::*;
    vec![
        bind(LeftUp, "Move the pointer", Press::Pointer),
        bind(LeftDown, "Move the pointer", Press::Pointer),
        bind(LeftLeft, "Move the pointer", Press::Pointer),
        bind(LeftRight, "Move the pointer", Press::Pointer),
        bind(A, "Choose", Press::LeftClick),
        bind(B, "Back", Press::RightClick),
        bind(Start, "Start the match", key("y")),
    ]
}

/// Defaults for melee and any map: camera on the left stick, pointer on the
/// right, select and command on A and B, the command card's top row on X, Y
/// and the triggers, control groups on the bumpers and the pad.
pub fn any_map_bindings() -> Vec<Binding> {
    use Control::*;
    vec![
        bind(A, "Select / click", Press::LeftClick),
        bind(B, "Command / right click", Press::RightClick),
        bind(X, "Ability 1", key("q")),
        bind(Y, "Ability 2", key("w")),
        bind(Rt, "Ability 3", key("e")),
        bind(Lt, "Ability 4", key("r")),
        bind(Lb, "Control group 1", key("1")),
        bind(Rb, "Control group 2", key("2")),
        bind(Start, "Game menu", key("f10")),
        bind(Back, "Cancel", key("escape")),
        bind(DpadUp, "Control group 3", key("3")),
        bind(DpadRight, "Control group 4", key("4")),
        bind(DpadDown, "Select hero", key("f1")),
        bind(DpadLeft, "Next unit in group", key("tab")),
        bind(LeftUp, "Camera up", key("up")),
        bind(LeftDown, "Camera down", key("down")),
        bind(LeftLeft, "Camera left", key("left")),
        bind(LeftRight, "Camera right", key("right")),
        bind(RightUp, "Pointer", Press::Pointer),
        bind(RightDown, "Pointer", Press::Pointer),
        bind(RightLeft, "Pointer", Press::Pointer),
        bind(RightRight, "Pointer", Press::Pointer),
    ]
}

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

/// Status light colours. `Off` is grey: not known while the service is off.
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

const CLICK_GAME: &str = "Click the Warcraft III window to use your controller.";

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
    let label = match s.profile {
        Profile::Smashcraft => "Smashcraft",
        Profile::AnyMap => "Map",
        Profile::Off => "Controller output",
    };
    if s.profile == Profile::Off {
        return row(label, "Off: your controller doesn't press anything", Light::Amber);
    }
    if s.game.is_none() {
        return row(label, "Waiting for Warcraft III", Light::Amber);
    }
    let (doing, followed) = match (s.profile, &s.session) {
        (Profile::Smashcraft, None) => return row(label, "Waiting for Smashcraft", Light::Amber),
        (Profile::Smashcraft, Some(session)) => (phase_text(session), true),
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

/// Plain-language status for a window. Product words only.
pub fn view(link: Link, s: &Snapshot) -> View {
    let rows = match link {
        Link::Off => [
            row("Controller", "Controller support is off", Light::Off),
            row("Warcraft III", "Not checked", Light::Off),
            row(s.profile.label(), "Not checked", Light::Off),
        ],
        Link::Starting => [
            row("Controller", "Turning on controller support…", Light::Amber),
            row("Warcraft III", "Checking…", Light::Amber),
            row(s.profile.label(), "Checking…", Light::Amber),
        ],
        Link::Connected => [
            match &s.pad {
                Some(pad) => row("Controller", format!("{} connected", pad.name), Light::Green),
                None => row(
                    "Controller",
                    "No controller found. Plug one in or switch it on.",
                    Light::Red,
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
    let worst = rows.iter().map(|r| r.light).max().unwrap_or(Light::Off);
    let overall = if problem.is_some() { Light::Red } else { worst };
    View { rows, overall, problem }
}

#[cfg(test)]
mod tests {
    #[test]
    fn standard_and_z_jump_presets_keep_their_bindings() {
        let standard = smashcraft_bindings();
        let z_jump = smashcraft_bindings_for(PadPreset::ZJump);
        for (standard, z_jump) in standard.iter().zip(&z_jump) {
            match standard.control {
                Control::B => { assert_eq!(standard.action, "Jump"); assert_eq!(z_jump.action, "Grab"); assert_eq!(z_jump.press, key("o")); }
                Control::Rb => { assert_eq!(standard.action, "Grab"); assert_eq!(z_jump.action, "Jump"); assert_eq!(z_jump.press, key("i")); }
                _ => assert_eq!(standard, z_jump),
            }
        }
        assert_eq!(z_jump.iter().find(|binding| binding.control == Control::Y).unwrap().action, "Jump");
        assert_eq!(ClientMessage::PadPreset(PadPreset::ZJump).line(), "{\"pad_preset\":\"z-jump\"}\n");
        assert_eq!(ClientMessage::TapJump(true).line(), "{\"tap_jump\":true}\n");
    }
    use super::*;

    fn playing() -> Snapshot {
        Snapshot {
            pad: Some(Pad {
                name: "Xbox One S pad".into(),
                id: "usb-Microsoft_Controller-event-joystick".into(),
            }),
            game: Some(Game { pid: 4242, window: true }),
            session: Some(Session {
                map: "Smashcraft".into(),
                phase: Phase::Match,
                player: Some(1),
            }),
            profile: Profile::Smashcraft,
            choice: ProfileChoice::Auto,
            output: Output { running: true, ready: true, focused: true },
            problem: None,
        }
    }

    fn texts(v: &View) -> Vec<(&str, Light)> {
        v.rows.iter().map(|r| (r.text.as_str(), r.light)).collect()
    }

    #[test]
    fn a_ready_match_is_all_green_in_plain_words() {
        let v = view(Link::Connected, &playing());
        assert_eq!(
            texts(&v),
            vec![
                ("Xbox One S pad connected", Light::Green),
                ("Running", Light::Green),
                ("In a match as Player 1", Light::Green),
            ]
        );
        assert_eq!(v.rows[2].label, "Smashcraft");
        assert_eq!(v.overall, Light::Green);
    }

    #[test]
    fn waiting_states_are_amber_and_a_missing_pad_is_red() {
        let mut s = playing();
        s.session = None;
        assert_eq!(view(Link::Connected, &s).rows[2].text, "Waiting for Smashcraft");
        assert_eq!(view(Link::Connected, &s).overall, Light::Amber);
        s.game = None;
        let v = view(Link::Connected, &s);
        assert_eq!(v.rows[1].text, "Not running");
        assert_eq!(v.rows[2].text, "Waiting for Warcraft III");
        s.pad = None;
        let v = view(Link::Connected, &s);
        assert_eq!(v.rows[0].light, Light::Red);
        assert_eq!(v.overall, Light::Red);
    }

    #[test]
    fn unfocused_or_unready_output_says_what_to_do() {
        let mut s = playing();
        s.output.focused = false;
        let v = view(Link::Connected, &s);
        assert_eq!(
            v.rows[2].text,
            "In a match as Player 1. Click the Warcraft III window to use your controller."
        );
        assert_eq!(v.overall, Light::Amber);
        s.output.ready = false;
        assert_eq!(
            view(Link::Connected, &s).rows[2].text,
            "In a match as Player 1. Getting your controller ready…"
        );
    }

    #[test]
    fn any_map_and_off_profiles_need_no_session() {
        let mut s = playing();
        s.session = None;
        s.profile = Profile::AnyMap;
        let v = view(Link::Connected, &s);
        assert_eq!(v.rows[2].label, "Map");
        assert_eq!(v.rows[2].text, "Your controller works in Warcraft III");
        assert_eq!(v.overall, Light::Green);
        s.profile = Profile::Off;
        assert_eq!(view(Link::Connected, &s).rows[2].light, Light::Amber);
    }

    #[test]
    fn service_off_is_grey_and_starting_is_amber_and_problems_are_red() {
        let s = playing();
        assert_eq!(view(Link::Off, &s).overall, Light::Off);
        assert_eq!(view(Link::Starting, &s).overall, Light::Amber);
        let mut s = playing();
        s.problem = Some("Warcraft III closed during the match.".into());
        let v = view(Link::Connected, &s);
        assert_eq!(v.overall, Light::Red);
        assert_eq!(v.problem.as_deref(), Some("Warcraft III closed during the match."));
    }

    #[test]
    fn view_text_never_names_internals() {
        let mut states = vec![playing()];
        let mut s = playing();
        s.output = Output::default();
        states.push(s.clone());
        s.session = None;
        s.game = None;
        s.pad = None;
        states.push(s);
        for link in [Link::Off, Link::Starting, Link::Connected] {
            for s in &states {
                for r in view(link, s).rows {
                    let t = format!("{} {}", r.label, r.text).to_lowercase();
                    for word in ["epoch", "journal", "x11", "slot", "pid", "helper", "socket", "evdev", "service"] {
                        assert!(!t.contains(word), "{t:?} names {word}");
                    }
                }
            }
        }
    }

    #[test]
    fn wire_lines_match_the_agreed_shape() {
        let status = ServiceMessage::Status(playing()).line();
        assert!(status.starts_with("{\"status\":{\"pad\":{\"name\":\"Xbox One S pad\""));
        assert!(status.ends_with("}\n"));
        assert_eq!(ServiceMessage::parse(&status).unwrap(), ServiceMessage::Status(playing()));
        let mut input = InputView { left: [0, -32768], lt: 9000, ..InputView::default() };
        input.press(Button::A, true);
        input.press(Button::Start, true);
        input.press(Button::Start, false);
        assert!(input.pressed(Button::A) && !input.pressed(Button::Start));
        assert_eq!(
            ServiceMessage::Input(input).line(),
            "{\"input\":{\"left\":[0,-32768],\"right\":[0,0],\"lt\":9000,\"rt\":0,\"buttons\":1}}\n"
        );
        assert_eq!(ClientMessage::Profile(ProfileChoice::AnyMap).line(), "{\"profile\":\"any_map\"}\n");
        assert_eq!(
            ClientMessage::parse("{\"profile\":\"auto\"}").unwrap(),
            ClientMessage::Profile(ProfileChoice::Auto)
        );
        let bindings = ClientMessage::Bindings(any_map_bindings()).line();
        assert!(bindings.contains("{\"control\":\"a\",\"action\":\"Select / click\",\"press\":\"left_click\"}"));
        assert!(bindings.contains("\"press\":{\"key\":\"q\"}"));
    }

    #[test]
    fn auto_follows_a_smashcraft_session() {
        assert_eq!(ProfileChoice::Auto.resolve(true), Profile::Smashcraft);
        assert_eq!(ProfileChoice::Auto.resolve(false), Profile::AnyMap);
        assert_eq!(ProfileChoice::Off.resolve(true), Profile::Off);
    }

    #[test]
    fn every_control_is_bound_at_most_once_per_profile() {
        for bindings in [smashcraft_bindings(), any_map_bindings()] {
            let mut seen = std::collections::BTreeSet::new();
            for b in &bindings {
                assert!(seen.insert(format!("{:?}", b.control)), "{:?} twice", b.control);
            }
        }
    }
}
