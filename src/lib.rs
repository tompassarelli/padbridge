// Unsafe code is confined to the Windows/macOS foreground adapters in focus/ and pad_kind's SDL query.
#![deny(unsafe_code)]

pub mod focus;
pub mod output;
pub mod pad_ingress;
pub mod stick;
#[cfg(target_os = "linux")]
pub mod service;
#[cfg(target_os = "linux")]
pub mod wlr;

pub use padbridge_model as model;

use sdl3::{
    event::Event,
    gamepad::{Axis, Button},
    joystick::JoystickId,
};
use std::{collections::{BTreeMap, BTreeSet}, time::{Duration, Instant}};

pub const TRIGGER_THRESHOLD: i16 = 4000;

/// The pad's family by `SDL_GetGamepadType`; the sdl3 wrapper reads GameCube as unknown.
#[allow(unsafe_code)]
pub fn pad_kind(id: JoystickId) -> model::PadKind {
    use sdl3::sys::gamepad::{SDL_GamepadType, SDL_GetGamepadTypeForID};
    // SAFETY: a plain query by instance ID; SDL returns UNKNOWN for an absent pad.
    if unsafe { SDL_GetGamepadTypeForID(id.into()) } == SDL_GamepadType::GAMECUBE { model::PadKind::GameCube } else { model::PadKind::Xbox }
}

/// SDL-normalized input, not original device packet values. Buttons use Xbox labels.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sample {
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    pub left_trigger: i16,
    pub right_trigger: i16,
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
    pub lb: bool,
    pub rb: bool,
    pub start: bool,
    pub left_stick: bool,
}

impl Sample {
    pub fn neutral(&self) -> bool {
        stick::melee_stick(self.left_x, self.left_y) == (0, 0)
            && stick::c_stick(self.right_x, self.right_y) == (0, 0)
            && self.left_trigger <= TRIGGER_THRESHOLD
            && self.right_trigger <= TRIGGER_THRESHOLD
            && ![self.a, self.b, self.x, self.y, self.lb, self.rb, self.start, self.left_stick].contains(&true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyOrClick {
    Attack,
    Special,
    Jump,
    ShortHop,
    Grab,
    Shield,
    RightShield,
    LightShield,
    Walk,
    Strong,
    Start,
    Left,
    Right,
    Down,
    Up,
    CLeft,
    CRight,
    CUp,
    CDown,
}

impl KeyOrClick {
    const BUTTONS: [Self; 11] = [Self::Attack, Self::Special, Self::Jump, Self::ShortHop, Self::Grab, Self::Shield, Self::RightShield, Self::LightShield, Self::Walk, Self::Strong, Self::Start];

    fn of_press(press: &model::Press) -> Option<Self> {
        let model::Press::Key(name) = press else { return None };
        Self::BUTTONS.into_iter().find(|action| name.chars().eq([action.key()]))
    }

    pub fn opposite(self) -> Option<Self> {
        match self {
            Self::Left => Some(Self::Right),
            Self::Right => Some(Self::Left),
            Self::Down => Some(Self::Up),
            Self::Up => Some(Self::Down),
            Self::CLeft => Some(Self::CRight),
            Self::CRight => Some(Self::CLeft),
            Self::CUp => Some(Self::CDown),
            Self::CDown => Some(Self::CUp),
            _ => None,
        }
    }

    pub fn key(self) -> char {
        use model::pad_key;
        match self {
            Self::Attack => pad_key::ATTACK,
            Self::Special => pad_key::SPECIAL,
            Self::Jump => pad_key::JUMP,
            Self::ShortHop => pad_key::SHORT_HOP,
            Self::Grab => pad_key::GRAB,
            Self::Shield => pad_key::SHIELD,
            Self::RightShield => pad_key::RIGHT_SHIELD,
            Self::LightShield => pad_key::LIGHT_SHIELD,
            Self::Walk => pad_key::TILT,
            Self::Strong => pad_key::STRONG,
            Self::Start => pad_key::PAUSE,
            Self::Left => pad_key::LEFT,
            Self::Right => pad_key::RIGHT,
            Self::Down => pad_key::DOWN,
            Self::Up => pad_key::UP,
            Self::CLeft => pad_key::C_LEFT,
            Self::CRight => pad_key::C_RIGHT,
            Self::CUp => pad_key::C_UP,
            Self::CDown => pad_key::C_DOWN,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub action: KeyOrClick,
    pub pressed: bool,
}

/// One mapped SDL Gamepad event, retaining SDL's own event timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturedInput {
    pub capture_ns: u64,
    pub control: Control,
    pub value: InputValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Axis(Axis),
    Button(Button),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputValue {
    Axis(i16),
    Button(bool),
}

/// Select only mapped gamepad input from the configured SDL instance ID.
/// Other pads and non-input notifications remain outside the logical history.
pub fn selected_input(event: &Event, selected: JoystickId) -> Option<CapturedInput> {
    match event {
        Event::GamepadAxisMotion {
            timestamp,
            which,
            axis,
            value,
        } if *which == selected => Some(CapturedInput {
            capture_ns: *timestamp,
            control: Control::Axis(*axis),
            value: InputValue::Axis(*value),
        }),
        Event::GamepadButtonDown {
            timestamp,
            which,
            button,
        } if *which == selected => Some(CapturedInput {
            capture_ns: *timestamp,
            control: Control::Button(*button),
            value: InputValue::Button(true),
        }),
        Event::GamepadButtonUp {
            timestamp,
            which,
            button,
        } if *which == selected => Some(CapturedInput {
            capture_ns: *timestamp,
            control: Control::Button(*button),
            value: InputValue::Button(false),
        }),
        _ => None,
    }
}

/// Per-event normalized state. Applying each queue item separately preserves
/// short button taps and axis excursions even when SDL returns them in a batch.
#[derive(Default)]
pub struct EventMapper {
    sample: Sample,
    mapper: Mapper,
}

impl EventMapper {
    pub fn set_trigger_shields(&mut self, triggers: model::TriggerShields) {
        self.mapper.set_trigger_shields(triggers);
    }
    pub fn set_tap_jump(&mut self, enabled: bool) {
        self.mapper.set_tap_jump(enabled);
    }
    pub fn set_right_stick(&mut self, mode: model::RightStick) {
        self.mapper.set_right_stick(mode);
    }
    pub fn set_pad_kind(&mut self, kind: model::PadKind) {
        self.mapper.set_pad_kind(kind);
    }
    pub fn set_remaps(&mut self, remaps: model::Remaps) {
        self.mapper.set_remaps(remaps);
    }
    pub fn new(preset: model::PadPreset) -> Self {
        Self { mapper: Mapper::new(preset), ..Self::default() }
    }
    /// Establish a fresh baseline without emitting presses. This is used after
    /// startup, focus loss, disconnect, or remapping; callers should pass the
    /// current SDL state and accept input only after the mapper arms neutrally.
    pub fn resync(&mut self, sample: &Sample, eligible: bool) -> Vec<Transition> {
        self.sample = sample.clone();
        self.mapper.update(Some(&self.sample), eligible)
    }

    pub fn disarm(&mut self) -> Vec<Transition> {
        self.sample = Sample::default();
        self.mapper.update(None, false)
    }

    pub fn armed(&self) -> bool {
        self.mapper.armed()
    }

    pub fn sample(&self) -> &Sample {
        &self.sample
    }

    pub fn apply(&mut self, event: CapturedInput, eligible: bool) -> Vec<Transition> {
        if !eligible || !self.mapper.armed() {
            return self.disarm();
        }
        match (event.control, event.value) {
            (Control::Axis(Axis::LeftX), InputValue::Axis(value)) => self.sample.left_x = value,
            (Control::Axis(Axis::LeftY), InputValue::Axis(value)) => self.sample.left_y = value,
            (Control::Axis(Axis::RightX), InputValue::Axis(value)) => self.sample.right_x = value,
            (Control::Axis(Axis::RightY), InputValue::Axis(value)) => self.sample.right_y = value,
            (Control::Axis(Axis::TriggerLeft), InputValue::Axis(value)) => {
                self.sample.left_trigger = value
            }
            (Control::Axis(Axis::TriggerRight), InputValue::Axis(value)) => {
                self.sample.right_trigger = value
            }
            (Control::Button(Button::South), InputValue::Button(value)) => self.sample.a = value,
            (Control::Button(Button::East), InputValue::Button(value)) => self.sample.b = value,
            (Control::Button(Button::West), InputValue::Button(value)) => self.sample.x = value,
            (Control::Button(Button::North), InputValue::Button(value)) => self.sample.y = value,
            (Control::Button(Button::LeftShoulder), InputValue::Button(value)) => {
                self.sample.lb = value
            }
            (Control::Button(Button::RightShoulder), InputValue::Button(value)) => {
                self.sample.rb = value
            }
            (Control::Button(Button::Start), InputValue::Button(value)) => {
                self.sample.start = value
            }
            (Control::Button(Button::LeftStick), InputValue::Button(value)) => self.sample.left_stick = value,
            // Ignore future SDL controls until the fighter layout defines a mapping.
            _ => return Vec::new(),
        }
        self.mapper.update(Some(&self.sample), true)
    }
}

/// One selected controller. Loss of eligibility or connection clears its actions;
/// held inputs cannot reactivate until a neutral sample arrives while eligible.
pub struct Mapper {
    held: BTreeSet<KeyOrClick>,
    armed: bool,
    preset: model::PadPreset,
    tap_jump: bool,
    triggers: model::TriggerShields,
    right_stick: model::RightStick,
    kind: model::PadKind,
    remaps: model::Remaps,
    buttons: Vec<(model::Control, KeyOrClick)>,
}

impl Default for Mapper {
    fn default() -> Self {
        Self::new(model::PadPreset::default())
    }
}

impl Mapper {
    pub fn set_trigger_shields(&mut self, triggers: model::TriggerShields) {
        self.triggers = triggers;
        self.rebind();
    }
    pub fn set_pad_kind(&mut self, kind: model::PadKind) {
        self.kind = kind;
        self.rebind();
    }
    pub fn set_remaps(&mut self, remaps: model::Remaps) {
        self.remaps = remaps;
        self.rebind();
    }
    pub fn set_tap_jump(&mut self, enabled: bool) {
        self.tap_jump = enabled;
    }
    pub fn set_right_stick(&mut self, mode: model::RightStick) {
        self.right_stick = mode;
        self.rebind();
    }
    pub fn new(preset: model::PadPreset) -> Self {
        let mut mapper = Self { held: BTreeSet::new(), armed: false, preset, tap_jump: false, triggers: Default::default(), right_stick: Default::default(), kind: Default::default(), remaps: Default::default(), buttons: Vec::new() };
        mapper.rebind();
        mapper
    }
    fn rebind(&mut self) {
        let bindings = model::remap(model::pad_bindings(self.preset, self.triggers, self.right_stick, self.kind), &self.remaps, self.triggers);
        self.buttons = bindings.iter().filter_map(|binding| Some((binding.control, KeyOrClick::of_press(&binding.press)?))).collect();
    }
    pub fn armed(&self) -> bool {
        self.armed
    }

    pub fn update(&mut self, sample: Option<&Sample>, eligible: bool) -> Vec<Transition> {
        let Some(sample) = sample.filter(|_| eligible) else {
            self.armed = false;
            return self.replace(BTreeSet::new());
        };
        if !self.armed {
            self.armed = sample.neutral();
            return Vec::new();
        }
        let (left_x, left_y) = stick::melee_stick(sample.left_x, sample.left_y);
        let (right_x, right_y) = stick::c_stick(sample.right_x, sample.right_y);
        let down = |control| match control {
            model::Control::A => sample.a,
            model::Control::B => sample.b,
            model::Control::X => sample.x,
            model::Control::Y => sample.y,
            model::Control::Lb => sample.lb,
            model::Control::Rb => sample.rb,
            model::Control::Start => sample.start,
            model::Control::LeftStick => sample.left_stick,
            model::Control::Lt => sample.left_trigger > TRIGGER_THRESHOLD,
            model::Control::Rt => sample.right_trigger > TRIGGER_THRESHOLD,
            _ => false,
        };
        let buttons: BTreeSet<KeyOrClick> = self.buttons.iter().filter(|(control, _)| down(*control)).map(|&(_, action)| action).collect();
        let stick_jump = stick::tap_jump(left_y, self.tap_jump, buttons.contains(&KeyOrClick::Walk), [KeyOrClick::Shield, KeyOrClick::RightShield, KeyOrClick::LightShield].iter().any(|shield| buttons.contains(shield)));
        // Union source states before diffing, so releasing one source never
        // releases an action another source still owns.
        let right_stick_tilts = self.right_stick == model::RightStick::Tilt && (right_x != 0 || right_y != 0);
        let bindings = [
            (KeyOrClick::Jump, stick_jump),
            (KeyOrClick::Walk, right_stick_tilts),
            (KeyOrClick::Left, left_x < 0),
            (KeyOrClick::Right, left_x > 0),
            (KeyOrClick::Down, stick::stick_down(left_y)),
            (KeyOrClick::Up, left_y < 0),
            (KeyOrClick::CLeft, right_x < 0),
            (KeyOrClick::CRight, right_x > 0),
            (KeyOrClick::CUp, right_y < 0),
            (KeyOrClick::CDown, right_y > 0),
        ];
        self.replace(
            bindings
                .into_iter()
                .filter_map(|(action, active)| active.then_some(action))
                .chain(buttons)
                .collect(),
        )
    }

    fn replace(&mut self, next: BTreeSet<KeyOrClick>) -> Vec<Transition> {
        let mut changes: Vec<_> = self
            .held
            .difference(&next)
            .map(|&action| Transition {
                action,
                pressed: false,
            })
            .collect();
        changes.extend(next.difference(&self.held).map(|&action| Transition {
            action,
            pressed: true,
        }));
        self.held = next;
        changes
    }
}

pub const MIN_HOLD: Duration = Duration::from_micros(33_333);

#[derive(Debug, Default)]
pub struct Hold {
    down: BTreeMap<KeyOrClick, Instant>,
    releasing: BTreeSet<KeyOrClick>,
}

impl Hold {
    pub fn apply(&mut self, transitions: Vec<Transition>, now: Instant) -> Vec<Transition> {
        let mut out = self.due(now);
        for transition in transitions {
            let action = transition.action;
            if transition.pressed {
                if let Some(opposite) = action.opposite().filter(|opposite| self.releasing.remove(opposite)) {
                    self.down.remove(&opposite);
                    out.push(Transition { action: opposite, pressed: false });
                }
                self.down.insert(action, now);
                if !self.releasing.remove(&action) {
                    out.push(transition);
                }
            } else if self.down.get(&action).is_some_and(|&at| now < at + MIN_HOLD) {
                self.releasing.insert(action);
            } else {
                self.down.remove(&action);
                out.push(transition);
            }
        }
        out
    }

    pub fn due(&mut self, now: Instant) -> Vec<Transition> {
        let due: Vec<KeyOrClick> = self.releasing.iter().copied().filter(|action| self.down.get(action).is_none_or(|&at| now >= at + MIN_HOLD)).collect();
        due.into_iter()
            .map(|action| {
                self.releasing.remove(&action);
                self.down.remove(&action);
                Transition { action, pressed: false }
            })
            .collect()
    }

    pub fn next_due(&self) -> Option<Instant> {
        self.releasing.iter().filter_map(|action| self.down.get(action)).min().map(|&at| at + MIN_HOLD)
    }

    pub fn release(&mut self) -> Vec<Transition> {
        self.down.clear();
        std::mem::take(&mut self.releasing).into_iter().map(|action| Transition { action, pressed: false }).collect()
    }
}

#[cfg(test)]
pub(crate) mod rng;
#[cfg(test)]
mod properties;
