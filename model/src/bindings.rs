use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{RightStick, TriggerShield, TriggerShields};

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
        (_, TriggerShield::Light) => bind(control, "Light shield", key("a")),
    }
}

/// `bindings` with each remapped control's binding replaced by its move.
pub fn remap(bindings: Vec<Binding>, remaps: &Remaps, triggers: TriggerShields) -> Vec<Binding> {
    let kept = bindings.into_iter().filter(|binding| !remaps.contains_key(&binding.control));
    kept.chain(remaps.iter().filter_map(|(&control, step)| Some(match step {
        Move::Attack => bind(control, "Attack", key("n")),
        Move::Special => bind(control, "Special", key("u")),
        Move::Jump => bind(control, "Jump", key("z")),
        Move::Grab => bind(control, "Grab", key("o")),
        Move::Shield => shield(control, triggers),
        Move::Tilt => bind(control, "Tilt", key("p")),
        Move::ShortHop => bind(control, "Short hop", key("i")),
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
        (key("n"), key("u"), key("z"), key("o"), key("i"), key("p"));
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
