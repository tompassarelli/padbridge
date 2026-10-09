//! What the controller service knows, the messages on its local interface, and
//! the plain-language status a window shows for it, and the built-in binding
//! tables. No I/O: the service and every window share it.
#![forbid(unsafe_code)]

pub mod any_map;
pub mod pad;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    TriggerShields(TriggerShields),
    /// Replaces the per-control changes on top of the pad preset.
    Remaps(Remaps),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    LeftStick,
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

/// The default fighter layout (Xbox labels), as the README's mapping table:
/// Melee's buttons by function.
pub fn fighter_bindings() -> Vec<Binding> {
    fighter_bindings_for(PadPreset::Melee)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PadPreset {
    #[default]
    Melee,
    ZJump,
    Tom,
    /// Hidden: the encoding of recorded pad scripts (B and Y jump, LB tilt,
    /// both triggers shield, L3 short hop). Test drivers pass `--preset script`.
    Script,
}

impl PadPreset {
    /// The presets players choose from; `Script` is left out.
    pub const ALL: [Self; 3] = [Self::Melee, Self::ZJump, Self::Tom];

    pub fn name(self) -> &'static str {
        match self { Self::Melee => "melee", Self::ZJump => "z-jump", Self::Tom => "tom", Self::Script => "script" }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL.into_iter().chain([Self::Script]).find(|preset| preset.name() == name)
            .ok_or_else(|| format!("unknown pad preset {name:?}: {}", Self::ALL.map(Self::name).join(", ")))
    }
}

pub fn fighter_bindings_for(preset: PadPreset) -> Vec<Binding> {
    fighter_bindings_with(preset, TriggerShields::default())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerShield {
    #[default]
    Full,
    Light,
}

impl TriggerShield {
    pub fn name(self) -> &'static str { match self { Self::Full => "full", Self::Light => "light" } }
    pub fn parse(name: &str) -> Result<Self, String> {
        match name { "full" => Ok(Self::Full), "light" => Ok(Self::Light), _ => Err("trigger shield needs full or light".into()) }
    }
    pub fn pressure(self) -> u16 { match self { Self::Full => 255, Self::Light => 77 } }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerShields {
    pub left: TriggerShield,
    pub right: TriggerShield,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControllerSettings {
    pub pad_preset: PadPreset,
    pub tap_jump: bool,
    pub triggers: TriggerShields,
    pub remaps: Remaps,
}

impl ControllerSettings {
    /// Applies a settings message; remaps naming a control that can't be remapped are refused whole.
    pub fn apply(&mut self, message: &ClientMessage) -> bool {
        let before = self.clone();
        match message {
            ClientMessage::PadPreset(preset) => self.pad_preset = *preset,
            ClientMessage::TapJump(on) => self.tap_jump = *on,
            ClientMessage::TriggerShields(triggers) => self.triggers = *triggers,
            ClientMessage::Remaps(remaps) if remaps.keys().all(|control| REMAPPABLE.contains(control)) => self.remaps = remaps.clone(),
            _ => {}
        }
        *self != before
    }

    /// The preset on a pad of `kind` with the remaps applied.
    pub fn bindings(&self, kind: PadKind) -> Vec<Binding> {
        remap(pad_bindings(self.pad_preset, self.triggers, kind), &self.remaps, self.triggers)
    }
}

/// A fighter move a remapped control presses; `None` leaves the control unbound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Move {
    Attack,
    Special,
    Jump,
    Grab,
    Shield,
    Tilt,
    ShortHop,
    Meter,
    None,
}

impl Move {
    pub const ALL: [Self; 9] = [Self::Attack, Self::Special, Self::Jump, Self::Grab, Self::Shield, Self::Tilt, Self::ShortHop, Self::Meter, Self::None];

    pub fn name(self) -> &'static str {
        match self {
            Self::Attack => "attack", Self::Special => "special", Self::Jump => "jump", Self::Grab => "grab",
            Self::Shield => "shield", Self::Tilt => "tilt", Self::ShortHop => "short_hop", Self::Meter => "meter", Self::None => "none",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|candidate| candidate.name() == name)
            .ok_or_else(|| format!("unknown move {name:?}: {}", Self::ALL.map(Self::name).join(", ")))
    }
}

/// Per-control changes on top of a preset, keyed by physical control (SDL position, after a GameCube pad's letter swap).
pub type Remaps = BTreeMap<Control, Move>;

/// The controls a player can remap: buttons, triggers and the left stick click.
pub const REMAPPABLE: [Control; 9] = [Control::A, Control::B, Control::X, Control::Y, Control::Lb, Control::Rb, Control::Lt, Control::Rt, Control::LeftStick];

impl Control {
    /// The control's name in JSON and on the command line, e.g. `lb`.
    pub fn name(self) -> String {
        serde_json::to_value(self).ok().and_then(|value| value.as_str().map(str::to_owned)).expect("a control is a string")
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::Value::String(name.to_owned())).map_err(|_| format!("unknown control {name:?}"))
    }
}

/// Remaps as a command-line value, `lb=grab,rb=none`; empty when there are none.
pub fn remaps_arg(remaps: &Remaps) -> String {
    remaps.iter().map(|(control, step)| format!("{}={}", control.name(), step.name())).collect::<Vec<_>>().join(",")
}

pub fn parse_remaps(text: &str) -> Result<Remaps, String> {
    text.split(',').filter(|part| !part.is_empty()).map(|part| {
        let (control, step) = part.split_once('=').ok_or_else(|| format!("remap {part:?} needs CONTROL=MOVE"))?;
        let control = Control::parse(control)?;
        if !REMAPPABLE.contains(&control) {
            return Err(format!("{control:?} can't be remapped: {}", REMAPPABLE.map(Control::name).join(", ")));
        }
        Ok((control, Move::parse(step)?))
    }).collect()
}

fn shield(control: Control, triggers: TriggerShields) -> Binding {
    let mode = match control { Control::Lt => triggers.left, Control::Rt => triggers.right, _ => TriggerShield::Full };
    match (control, mode) {
        (Control::Rt, TriggerShield::Full) => bind(control, "Shield", key("7")),
        (_, TriggerShield::Full) => bind(control, "Shield", key("q")),
        (_, TriggerShield::Light) => bind(control, "Light shield", key("t")),
    }
}

/// `bindings` with each remapped control's binding replaced by its move.
pub fn remap(bindings: Vec<Binding>, remaps: &Remaps, triggers: TriggerShields) -> Vec<Binding> {
    let kept = bindings.into_iter().filter(|binding| !remaps.contains_key(&binding.control));
    kept.chain(remaps.iter().filter_map(|(&control, step)| Some(match step {
        Move::Attack => bind(control, "Attack", key("n")),
        Move::Special => bind(control, "Special", key("u")),
        Move::Jump => bind(control, "Jump", key("i")),
        Move::Grab => bind(control, "Grab", key("o")),
        Move::Shield => shield(control, triggers),
        Move::Tilt => bind(control, "Tilt", key("p")),
        Move::ShortHop => bind(control, "Short hop", key("z")),
        Move::Meter => bind(control, "Meter", key("x")),
        Move::None => return None,
    }))).collect()
}

/// The pad family SDL reports (`SDL_GetGamepadType`). Controls are named by
/// SDL position with Xbox labels; a GameCube pad's west button is B, its east
/// button X, its right shoulder Z and its triggers L and R.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PadKind {
    #[default]
    Xbox,
    GameCube,
}

impl PadKind {
    pub fn label(self, control: Control) -> &'static str {
        use Control::*;
        match (self, control) {
            (Self::GameCube, X) => "B",
            (Self::GameCube, B) => "X",
            (Self::GameCube, Rb) => "Z",
            (Self::GameCube, Lt) => "L",
            (Self::GameCube, Rt) => "R",
            (_, A) => "A", (_, B) => "B", (_, X) => "X", (_, Y) => "Y",
            (_, Lb) => "LB", (_, Rb) => "RB", (_, Lt) => "LT", (_, Rt) => "RT",
            (_, LeftStick) => "L3", (_, Start) => "Start", (_, Back) => "Back",
            (_, DpadUp) => "D-pad up", (_, DpadDown) => "D-pad down", (_, DpadLeft) => "D-pad left", (_, DpadRight) => "D-pad right",
            (_, LeftUp) => "Left stick up", (_, LeftDown) => "Left stick down", (_, LeftLeft) => "Left stick left", (_, LeftRight) => "Left stick right",
            (_, RightUp) => "Right stick up", (_, RightDown) => "Right stick down", (_, RightLeft) => "Right stick left", (_, RightRight) => "Right stick right",
        }
    }
}

/// A preset on a pad of `kind`: buttons keep their printed letters, so a
/// GameCube pad's B (west) is special and its X (east) jumps in melee.
pub fn pad_bindings(preset: PadPreset, triggers: TriggerShields, kind: PadKind) -> Vec<Binding> {
    let mut bindings = fighter_bindings_with(preset, triggers);
    if kind == PadKind::GameCube {
        for binding in &mut bindings {
            binding.control = match binding.control { Control::B => Control::X, Control::X => Control::B, control => control };
        }
    }
    bindings
}

pub fn fighter_bindings_with(preset: PadPreset, triggers: TriggerShields) -> Vec<Binding> {
    use Control::*;
    let (attack, special, jump, grab, short_hop, tilt) =
        (key("n"), key("u"), key("i"), key("o"), key("z"), key("p"));
    let buttons = match preset {
        PadPreset::Melee => vec![
            bind(A, "Attack", attack), bind(B, "Special", special), bind(X, "Jump", jump.clone()), bind(Y, "Jump", jump),
            bind(Rb, "Grab", grab), shield(Lt, triggers), shield(Rt, triggers),
        ],
        PadPreset::ZJump => vec![
            bind(A, "Attack", attack), bind(B, "Special", special), bind(X, "Grab", grab), bind(Y, "Jump", jump.clone()),
            bind(Rb, "Jump", jump), shield(Lt, triggers), shield(Rt, triggers),
        ],
        PadPreset::Tom => vec![
            bind(A, "Attack", attack), bind(B, "Grab", grab), bind(X, "Special", special), bind(Y, "Jump", jump),
            bind(Lb, "Short hop", short_hop), bind(Rb, "Meter", key("x")), shield(Lt, triggers), shield(Rt, triggers),
        ],
        PadPreset::Script => vec![
            bind(A, "Attack", attack), bind(X, "Special", special), bind(B, "Jump", jump.clone()), bind(Y, "Jump", jump),
            bind(LeftStick, "Short hop", short_hop), bind(Rb, "Grab", grab), bind(Lb, "Tilt", tilt),
            shield(Lt, triggers), shield(Rt, triggers),
        ],
    };
    buttons.into_iter().chain([
        bind(Start, "Pause", key("y")),
        bind(LeftLeft, "Move left", key("w")),
        bind(LeftRight, "Move right", key("r")),
        bind(LeftDown, "Crouch / fast fall", key("e")),
        bind(LeftUp, "Up (aim, up special)", key("space")),
        bind(RightUp, "Up smash", key("j")),
        bind(RightDown, "Down smash", key("h")),
        bind(RightLeft, "Smash left", key("b")),
        bind(RightRight, "Smash right", key("m")),
    ]).collect()
}

/// A map's menus driven by the pad: the left stick moves the pointer, as a
/// hand cursor does in Smash, and the face buttons click.
pub fn menu_pointer_bindings() -> Vec<Binding> {
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
        Profile::Map => s.map.as_deref().unwrap_or("Map"),
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
        (Profile::Map, None) => return row(label, format!("Waiting for {}", s.map.as_deref().unwrap_or("the map")), Light::Amber),
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

fn profile_label(s: &Snapshot) -> &str {
    match (s.profile, &s.map) {
        (Profile::Map, Some(title)) => title,
        (profile, _) => profile.label(),
    }
}

/// Plain-language status for a window. Product words only.
pub fn view(link: Link, s: &Snapshot) -> View {
    let rows = match link {
        Link::Off => [
            row("Controller", "Controller support is off", Light::Off),
            row("Warcraft III", "Not checked", Light::Off),
            row(profile_label(s), "Not checked", Light::Off),
        ],
        Link::Starting => [
            row("Controller", "Turning on controller support…", Light::Amber),
            row("Warcraft III", "Checking…", Light::Amber),
            row(profile_label(s), "Checking…", Light::Amber),
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
    fn presets_bind_their_buttons() {
        let pressed = |preset, control| fighter_bindings_for(preset).into_iter().filter(|b| b.control == control).map(|b| b.action).collect::<Vec<_>>().join("+");
        use Control::*;
        let table = [
            (PadPreset::Melee, [A, B, X, Y, Lb, Rb, Lt, Rt, LeftStick], ["Attack", "Special", "Jump", "Jump", "", "Grab", "Shield", "Shield", ""]),
            (PadPreset::ZJump, [A, B, X, Y, Lb, Rb, Lt, Rt, LeftStick], ["Attack", "Special", "Grab", "Jump", "", "Jump", "Shield", "Shield", ""]),
            (PadPreset::Tom, [A, B, X, Y, Lb, Rb, Lt, Rt, LeftStick], ["Attack", "Grab", "Special", "Jump", "Short hop", "Meter", "Shield", "Shield", ""]),
            (PadPreset::Script, [A, B, X, Y, Lb, Rb, Lt, Rt, LeftStick], ["Attack", "Jump", "Special", "Jump", "Tilt", "Grab", "Shield", "Shield", "Short hop"]),
        ];
        for (preset, controls, actions) in table {
            assert_eq!(controls.map(|control| pressed(preset, control)), actions.map(str::to_owned), "{}", preset.name());
            assert_eq!(PadPreset::parse(preset.name()), Ok(preset));
            let shields = fighter_bindings_for(preset).into_iter().filter(|b| b.action == "Shield").map(|b| (b.control, b.press)).collect::<Vec<_>>();
            assert_eq!(shields, [(Lt, key("q")), (Rt, key("7"))], "{}", preset.name());
        }
        assert_eq!(PadPreset::default(), PadPreset::Melee);
        assert!(PadPreset::parse("standard").is_err());
        let gamecube = pad_bindings(PadPreset::Melee, TriggerShields::default(), PadKind::GameCube);
        let by_label = |label: &str| gamecube.iter().filter(|b| PadKind::GameCube.label(b.control) == label).map(|b| b.action.as_str()).collect::<Vec<_>>();
        for (label, action) in [("A", "Attack"), ("B", "Special"), ("X", "Jump"), ("Y", "Jump"), ("Z", "Grab"), ("L", "Shield"), ("R", "Shield")] {
            assert_eq!(by_label(label), [action], "GameCube {label}");
        }
        assert_eq!(ClientMessage::PadPreset(PadPreset::ZJump).line(), "{\"pad_preset\":\"z-jump\"}\n");
        assert_eq!(ClientMessage::TapJump(true).line(), "{\"tap_jump\":true}\n");
    }
    use super::*;

    #[test]
    fn remaps_change_single_controls_and_round_trip() {
        let mut settings = ControllerSettings::default();
        let remaps = parse_remaps("lb=grab,rt=none,x=shield").unwrap();
        assert!(settings.apply(&ClientMessage::Remaps(remaps.clone())));
        let actions = |control| settings.bindings(PadKind::Xbox).into_iter().filter(|b| b.control == control).map(|b| (b.action, b.press)).collect::<Vec<_>>();
        assert_eq!(actions(Control::Lb), [("Grab".to_owned(), key("o"))]);
        assert_eq!(actions(Control::Rt), []);
        assert_eq!(actions(Control::X), [("Shield".to_owned(), key("q"))]);
        assert_eq!(actions(Control::A), [("Attack".to_owned(), key("n"))]);
        assert_eq!(remaps_arg(&settings.remaps), "x=shield,lb=grab,rt=none");
        assert_eq!(parse_remaps(&remaps_arg(&remaps)), Ok(remaps));
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(saved.contains("\"remaps\":{\"x\":\"shield\",\"lb\":\"grab\",\"rt\":\"none\"}"), "{saved}");
        assert_eq!(serde_json::from_str::<ControllerSettings>(&saved).unwrap(), settings);
        assert_eq!(serde_json::from_str::<ControllerSettings>("{\"pad_preset\":\"tom\"}").unwrap().remaps, Remaps::new());
        assert!(!settings.apply(&ClientMessage::parse("{\"remaps\":{\"left_up\":\"jump\"}}").unwrap()));
        assert!(parse_remaps("left_up=jump").is_err() && parse_remaps("lb").is_err() && parse_remaps("lb=dance").is_err());
    }

    fn playing() -> Snapshot {
        Snapshot {
            settings: ControllerSettings::default(),
            pad: Some(Pad {
                name: "Xbox One S pad".into(),
                id: "usb-Microsoft_Controller-event-joystick".into(),
            }),
            game: Some(Game { pid: 4242, window: true }),
            session: Some(Session {
                map: "Hero Arena".into(),
                phase: Phase::Match,
                player: Some(1),
            }),
            profile: Profile::Map,
            choice: ProfileChoice::Auto,
            map: Some("Hero Arena".into()),
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
        assert_eq!(v.rows[2].label, "Hero Arena");
        assert_eq!(v.overall, Light::Green);
    }

    #[test]
    fn waiting_states_are_amber_and_a_missing_pad_is_red() {
        let mut s = playing();
        s.session = None;
        assert_eq!(view(Link::Connected, &s).rows[2].text, "Waiting for Hero Arena");
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
    fn auto_follows_a_map_plugin_session() {
        assert_eq!(ProfileChoice::Auto.resolve(true), Profile::Map);
        assert_eq!(ProfileChoice::Auto.resolve(false), Profile::AnyMap);
        assert_eq!(ProfileChoice::Off.resolve(true), Profile::Off);
    }

    #[test]
    fn every_control_is_bound_at_most_once_per_profile() {
        for bindings in [fighter_bindings(), any_map_bindings()] {
            let mut seen = std::collections::BTreeSet::new();
            for b in &bindings {
                assert!(seen.insert(format!("{:?}", b.control)), "{:?} twice", b.control);
            }
        }
    }
}
