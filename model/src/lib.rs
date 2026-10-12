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
    RightStick(RightStick),
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
    fighter_bindings_with(preset, TriggerShields::default(), RightStick::default())
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

/// What a right-stick flick asks for; Tilt also holds Tilt (P), which turns the smash into a tilt or aerial.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightStick {
    #[default]
    Smash,
    Tilt,
}

impl RightStick {
    pub const ALL: [Self; 2] = [Self::Smash, Self::Tilt];
    pub fn name(self) -> &'static str { match self { Self::Smash => "smash", Self::Tilt => "tilt" } }
    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|mode| mode.name() == name).ok_or_else(|| "right stick needs smash or tilt".into())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControllerSettings {
    pub pad_preset: PadPreset,
    pub tap_jump: bool,
    pub triggers: TriggerShields,
    pub right_stick: RightStick,
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
            ClientMessage::RightStick(mode) => self.right_stick = *mode,
            ClientMessage::Remaps(remaps) if remaps.keys().all(|control| REMAPPABLE.contains(control)) => self.remaps = remaps.clone(),
            _ => {}
        }
        *self != before
    }

    /// The preset on a pad of `kind` with the remaps applied.
    pub fn bindings(&self, kind: PadKind) -> Vec<Binding> {
        remap(pad_bindings(self.pad_preset, self.triggers, self.right_stick, kind), &self.remaps, self.triggers)
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
        (Control::Rt, TriggerShield::Full) => bind(control, "Right shield", key("g")),
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
pub fn pad_bindings(preset: PadPreset, triggers: TriggerShields, right_stick: RightStick, kind: PadKind) -> Vec<Binding> {
    let mut bindings = fighter_bindings_with(preset, triggers, right_stick);
    if kind == PadKind::GameCube {
        for binding in &mut bindings {
            binding.control = match binding.control { Control::B => Control::X, Control::X => Control::B, control => control };
        }
    }
    bindings
}

pub fn fighter_bindings_with(preset: PadPreset, triggers: TriggerShields, right_stick: RightStick) -> Vec<Binding> {
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
    let right = |smash: &'static str, tilt: &'static str| match right_stick { RightStick::Smash => smash, RightStick::Tilt => tilt };
    buttons.into_iter().chain([
        bind(Start, "Pause", key("y")),
        bind(LeftLeft, "Move left", key("w")),
        bind(LeftRight, "Move right", key("r")),
        bind(LeftDown, "Crouch / fast fall", key("e")),
        bind(LeftUp, "Up (aim, up special)", key("space")),
        bind(RightUp, right("Up smash", "Up tilt / up air"), key("j")),
        bind(RightDown, right("Down smash", "Down tilt / down air"), key("h")),
        bind(RightLeft, right("Smash left", "Tilt left / air left"), key("b")),
        bind(RightRight, right("Smash right", "Tilt right / air right"), key("m")),
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
pub(crate) mod rng {
    pub struct Rng(u64);

    impl Rng {
        pub fn new(salt: u64) -> Self {
            Self(std::env::var("PROPERTY_SEED").ok().and_then(|seed| seed.parse().ok()).unwrap_or(0x5eed_u64) ^ salt)
        }
        pub fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        pub fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        pub fn pick<T: Clone>(&mut self, items: &[T]) -> T {
            items[self.below(items.len())].clone()
        }
        pub fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
    }

    pub fn cases() -> usize {
        std::env::var("PROPERTY_CASES").ok().and_then(|cases| cases.parse().ok()).unwrap_or(300)
    }
}

#[cfg(test)]
mod properties {
    use super::rng::{Rng, cases};
    use super::*;

    const CONTROLS: [Control; 23] = [
        Control::LeftStick, Control::A, Control::B, Control::X, Control::Y, Control::Lb, Control::Rb, Control::Lt, Control::Rt,
        Control::Start, Control::Back, Control::DpadUp, Control::DpadDown, Control::DpadLeft, Control::DpadRight,
        Control::LeftUp, Control::LeftDown, Control::LeftLeft, Control::LeftRight, Control::RightUp, Control::RightDown, Control::RightLeft, Control::RightRight,
    ];
    const PRESETS: [PadPreset; 4] = [PadPreset::Melee, PadPreset::ZJump, PadPreset::Tom, PadPreset::Script];
    const SHIELDS: [TriggerShield; 2] = [TriggerShield::Full, TriggerShield::Light];
    const KINDS: [PadKind; 2] = [PadKind::Xbox, PadKind::GameCube];

    fn right_bindings(preset: PadPreset, mode: RightStick, triggers: TriggerShields, kind: PadKind) -> Vec<Binding> {
        pad_bindings(preset, triggers, mode, kind).into_iter().filter(|b| matches!(b.control, Control::RightUp | Control::RightDown | Control::RightLeft | Control::RightRight)).collect()
    }

    fn remaps(rng: &mut Rng) -> Remaps {
        (0..rng.below(5)).map(|_| (rng.pick(&REMAPPABLE), rng.pick(&Move::ALL))).collect()
    }

    fn settings(rng: &mut Rng) -> ControllerSettings {
        ControllerSettings {
            pad_preset: rng.pick(&PRESETS),
            tap_jump: rng.chance(50),
            triggers: TriggerShields { left: rng.pick(&SHIELDS), right: rng.pick(&SHIELDS) },
            right_stick: rng.pick(&RightStick::ALL),
            remaps: remaps(rng),
        }
    }

    fn snapshot(rng: &mut Rng) -> Snapshot {
        let profile = rng.pick(&Profile::ALL);
        Snapshot {
            pad: rng.chance(70).then(|| Pad { name: "Pad".into(), id: "pad-1".into() }),
            game: rng.chance(70).then(|| Game { pid: 4242, window: rng.chance(70) }),
            session: rng.chance(60).then(|| Session {
                map: "Arena".into(),
                phase: rng.pick(&[Phase::Lobby, Phase::CharacterSelect, Phase::StageSelect, Phase::Match, Phase::Results]),
                player: rng.chance(50).then_some(2),
            }),
            profile,
            choice: rng.pick(&[ProfileChoice::Auto, ProfileChoice::Map, ProfileChoice::AnyMap, ProfileChoice::Off]),
            map: rng.chance(60).then(|| "Arena".into()),
            output: Output { running: rng.chance(80), ready: rng.chance(80), focused: rng.chance(70) },
            problem: rng.chance(15).then(|| "Something stopped.".into()),
            settings: settings(rng),
        }
    }

    #[test]
    fn the_view_follows_the_light_rules_for_every_generated_status() {
        let mut rng = Rng::new(1);
        for _ in 0..cases() * 4 {
            let s = snapshot(&mut rng);
            let link = rng.pick(&[Link::Off, Link::Starting, Link::Connected]);
            let v = view(link, &s);
            let lights = v.rows.each_ref().map(|r| r.light);
            let expected = match link {
                Link::Off => [Light::Off; 3],
                Link::Starting => [Light::Amber; 3],
                Link::Connected => {
                    let session_ready = s.profile != Profile::Off
                        && s.game.is_some()
                        && !(s.profile == Profile::Map && s.session.is_none())
                        && s.output.running && s.output.ready && s.output.focused;
                    [
                        if s.pad.is_some() { Light::Green } else { Light::Red },
                        if s.game.as_ref().is_some_and(|g| g.window) { Light::Green } else { Light::Amber },
                        if session_ready { Light::Green } else { Light::Amber },
                    ]
                }
            };
            assert_eq!(lights, expected, "{link:?} {s:?}");
            let problem = if link == Link::Connected { s.problem.clone() } else { None };
            assert_eq!(v.problem, problem);
            let overall = if problem.is_some() || lights.contains(&Light::Red) {
                Light::Red
            } else if lights.contains(&Light::Amber) {
                Light::Amber
            } else if lights.contains(&Light::Off) {
                Light::Off
            } else {
                Light::Green
            };
            assert_eq!(v.overall, overall, "{link:?} {s:?}");
            if s.profile == Profile::Map && let Some(title) = &s.map {
                assert_eq!(&v.rows[2].label, title);
            }
            for r in &v.rows {
                let t = format!("{} {}", r.label, r.text).to_lowercase();
                for word in ["epoch", "journal", "x11", "slot", "pid", "helper", "socket", "evdev", "service", "4242"] {
                    assert!(!t.contains(word), "{t:?} names {word}");
                }
            }
            let session = rng.chance(50);
            let choice = s.choice;
            let resolved = match choice {
                ProfileChoice::Auto => if session { Profile::Map } else { Profile::AnyMap },
                ProfileChoice::Map => Profile::Map,
                ProfileChoice::AnyMap => Profile::AnyMap,
                ProfileChoice::Off => Profile::Off,
            };
            assert_eq!(choice.resolve(session), resolved);
        }
    }

    #[test]
    fn settings_remaps_and_messages_round_trip() {
        let mut rng = Rng::new(2);
        for _ in 0..cases() {
            let mut settings = settings(&mut rng);
            let remaps = settings.remaps.clone();
            assert_eq!(parse_remaps(&remaps_arg(&remaps)), Ok(remaps.clone()));
            for preset in PRESETS {
                assert_eq!(PadPreset::parse(preset.name()), Ok(preset));
            }
            for shield in SHIELDS {
                assert_eq!(TriggerShield::parse(shield.name()), Ok(shield));
            }
            for mode in RightStick::ALL {
                assert_eq!(RightStick::parse(mode.name()), Ok(mode));
            }
            let saved = serde_json::to_value(&settings).unwrap();
            assert_eq!(serde_json::from_value::<ControllerSettings>(saved.clone()).unwrap(), settings);
            let mut partial = saved.as_object().unwrap().clone();
            let dropped = rng.pick(&["pad_preset", "tap_jump", "triggers", "right_stick", "remaps"]);
            partial.remove(dropped);
            let loaded: ControllerSettings = serde_json::from_value(serde_json::Value::Object(partial)).unwrap();
            let defaults = ControllerSettings::default();
            match dropped {
                "pad_preset" => assert_eq!(loaded.pad_preset, defaults.pad_preset),
                "tap_jump" => assert_eq!(loaded.tap_jump, defaults.tap_jump),
                "triggers" => assert_eq!(loaded.triggers, defaults.triggers),
                "right_stick" => assert_eq!(loaded.right_stick, RightStick::Smash),
                _ => assert_eq!(loaded.remaps, defaults.remaps),
            }

            let unremappable = rng.pick(&[Control::LeftUp, Control::Start, Control::DpadDown, Control::RightLeft]);
            let mut refused = remaps.clone();
            refused.insert(unremappable, rng.pick(&Move::ALL));
            let before = settings.clone();
            assert!(!settings.apply(&ClientMessage::Remaps(refused.clone())));
            assert_eq!(settings, before);
            assert!(parse_remaps(&remaps_arg(&refused)).is_err());
            let next = self::remaps(&mut rng);
            assert_eq!(settings.apply(&ClientMessage::Remaps(next.clone())), next != before.remaps);
            assert_eq!(settings.remaps, next);
            let tap = rng.chance(50);
            let changed = tap != settings.tap_jump;
            assert_eq!(settings.apply(&ClientMessage::TapJump(tap)), changed);
            assert_eq!(settings.tap_jump, tap);
            let mode = rng.pick(&RightStick::ALL);
            let changed = mode != settings.right_stick;
            assert_eq!(settings.apply(&ClientMessage::RightStick(mode)), changed);
            assert_eq!(settings.right_stick, mode);

            let kind = rng.pick(&KINDS);
            let plain = pad_bindings(settings.pad_preset, settings.triggers, settings.right_stick, kind);
            let bound = settings.bindings(kind);
            for control in CONTROLS {
                let of = |bindings: &[Binding]| bindings.iter().filter(|b| b.control == control).cloned().collect::<Vec<_>>();
                match settings.remaps.get(&control) {
                    None => assert_eq!(of(&bound), of(&plain), "{control:?}"),
                    Some(Move::None) => assert!(of(&bound).is_empty(), "{control:?}"),
                    Some(step) => {
                        let actions = of(&bound);
                        assert_eq!(actions.len(), 1, "{control:?}");
                        let preset_of = |action: &str| fighter_bindings_with(PadPreset::Script, settings.triggers, settings.right_stick).into_iter().chain(fighter_bindings_with(PadPreset::Tom, settings.triggers, settings.right_stick))
                            .find(|b| b.action == action).map(|b| b.press);
                        let expected = match step {
                            Move::Attack => preset_of("Attack"),
                            Move::Special => preset_of("Special"),
                            Move::Jump => preset_of("Jump"),
                            Move::Grab => preset_of("Grab"),
                            Move::Tilt => preset_of("Tilt"),
                            Move::ShortHop => preset_of("Short hop"),
                            Move::Meter => preset_of("Meter"),
                            Move::Shield | Move::None => None,
                        };
                        if let Some(press) = expected {
                            assert_eq!(actions[0].press, press, "{control:?} {step:?}");
                        }
                    }
                }
            }
            for bindings in [bound, any_map_bindings(), menu_pointer_bindings()] {
                let mut seen = std::collections::BTreeSet::new();
                for b in &bindings {
                    assert!(seen.insert(b.control), "{:?} bound twice", b.control);
                }
            }
            for preset in PRESETS {
                let triggers = TriggerShields { left: rng.pick(&SHIELDS), right: rng.pick(&SHIELDS) };
                let printed = |kind: PadKind| -> std::collections::BTreeSet<(&'static str, String)> {
                    pad_bindings(preset, triggers, settings.right_stick, kind).into_iter().map(|b| {
                        let label = match (kind, kind.label(b.control)) {
                            (PadKind::GameCube, "Z") => "RB",
                            (PadKind::GameCube, "L") => "LT",
                            (PadKind::GameCube, "R") => "RT",
                            (_, label) => label,
                        };
                        (label, b.action)
                    }).collect()
                };
                assert_eq!(printed(PadKind::GameCube), printed(PadKind::Xbox), "a GameCube pad keeps its printed letters in {}", preset.name());
                let right = |mode| right_bindings(preset, mode, triggers, kind);
                let (smash, tilt) = (right(RightStick::Smash), right(RightStick::Tilt));
                for mode in RightStick::ALL {
                    assert_eq!(right(mode), right_bindings(PadPreset::Melee, mode, triggers, kind), "the right stick ignores the preset in {}", preset.name());
                }
                assert_eq!(smash.len(), 4);
                for (s, t) in smash.iter().zip(&tilt) {
                    assert_eq!((s.control, &s.press), (t.control, &t.press), "only the label follows the right stick mode");
                    assert!(s.action.to_lowercase().contains("smash") && t.action.to_lowercase().contains("tilt"), "{s:?} {t:?}");
                }
            }

            let s = snapshot(&mut rng);
            let mut input = InputView { left: [rng.next() as i16, rng.next() as i16], right: [rng.next() as i16, 0], lt: rng.below(32768) as u16, rt: 0, buttons: 0 };
            let mut reference = 0u32;
            for _ in 0..6 {
                let button = rng.pick(&[Button::A, Button::B, Button::X, Button::Y, Button::Lb, Button::Rb, Button::Start, Button::Back, Button::LeftStick, Button::RightStick, Button::DpadUp, Button::DpadDown, Button::DpadLeft, Button::DpadRight]);
                let down = rng.chance(50);
                input.press(button, down);
                let bit = 1u32 << (button as u32);
                if down { reference |= bit } else { reference &= !bit }
                assert_eq!(input.buttons, reference);
                assert_eq!(input.pressed(button), down);
            }
            for message in [ServiceMessage::Status(s), ServiceMessage::Input(input)] {
                let line = message.line();
                assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "{line:?}");
                assert_eq!(ServiceMessage::parse(&line).unwrap(), message);
            }
            for message in [
                ClientMessage::Profile(rng.pick(&[ProfileChoice::Auto, ProfileChoice::Map, ProfileChoice::AnyMap, ProfileChoice::Off])),
                ClientMessage::PadPreset(rng.pick(&PRESETS)),
                ClientMessage::TapJump(rng.chance(50)),
                ClientMessage::RightStick(rng.pick(&RightStick::ALL)),
                ClientMessage::Remaps(self::remaps(&mut rng)),
                ClientMessage::Bindings(any_map_bindings()),
            ] {
                let line = message.line();
                assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "{line:?}");
                assert_eq!(ClientMessage::parse(&line).unwrap(), message);
            }
        }
    }
}
