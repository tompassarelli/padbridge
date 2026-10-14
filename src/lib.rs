// Unsafe code is confined to the Windows/macOS foreground adapters in focus.rs and pad_kind's SDL query.
#![deny(unsafe_code)]

pub mod focus;
pub mod output;
pub mod pad_ingress;
pub mod stick;
#[cfg(target_os = "linux")]
mod niri;
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
use std::collections::BTreeSet;

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
pub enum Action {
    Attack,
    Special,
    Jump,
    ShortHop,
    Grab,
    Shield,
    RightShield,
    LightShield,
    Walk,
    Meter,
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

impl Action {
    const BUTTONS: [Self; 11] = [Self::Attack, Self::Special, Self::Jump, Self::ShortHop, Self::Grab, Self::Shield, Self::RightShield, Self::LightShield, Self::Walk, Self::Meter, Self::Start];

    fn of_press(press: &model::Press) -> Option<Self> {
        let model::Press::Key(name) = press else { return None };
        Self::BUTTONS.into_iter().find(|action| name.chars().eq([action.key()]))
    }

    pub fn key(self) -> char {
        match self {
            Self::Attack => 'n',
            Self::Special => 'u',
            Self::Jump => 'i',
            Self::ShortHop => 'z',
            Self::Grab => 'o',
            Self::Shield => 'q',
            Self::RightShield => 'g',
            Self::LightShield => 't',
            Self::Walk => 'p',
            Self::Meter => 'x',
            Self::Start => 'y',
            Self::Left => 'w',
            Self::Right => 'r',
            Self::Down => 'e',
            Self::Up => ' ',
            Self::CLeft => 'b',
            Self::CRight => 'm',
            Self::CUp => 'j',
            Self::CDown => 'h',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub action: Action,
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
    held: BTreeSet<Action>,
    armed: bool,
    preset: model::PadPreset,
    tap_jump: bool,
    triggers: model::TriggerShields,
    right_stick: model::RightStick,
    kind: model::PadKind,
    remaps: model::Remaps,
    buttons: Vec<(model::Control, Action)>,
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
        self.buttons = bindings.iter().filter_map(|binding| Some((binding.control, Action::of_press(&binding.press)?))).collect();
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
        let buttons: BTreeSet<Action> = self.buttons.iter().filter(|(control, _)| down(*control)).map(|&(_, action)| action).collect();
        let stick_jump = stick::tap_jump(left_y, self.tap_jump, buttons.contains(&Action::Walk), [Action::Shield, Action::RightShield, Action::LightShield].iter().any(|shield| buttons.contains(shield)));
        // Union source states before diffing, so releasing one source never
        // releases an action another source still owns.
        let right_stick_tilts = self.right_stick == model::RightStick::Tilt && (right_x != 0 || right_y != 0);
        let bindings = [
            (Action::Jump, stick_jump),
            (Action::Walk, right_stick_tilts),
            (Action::Left, left_x < 0),
            (Action::Right, left_x > 0),
            (Action::Down, stick::stick_down(left_y)),
            (Action::Up, left_y < 0),
            (Action::CLeft, right_x < 0),
            (Action::CRight, right_x > 0),
            (Action::CUp, right_y < 0),
            (Action::CDown, right_y > 0),
        ];
        self.replace(
            bindings
                .into_iter()
                .filter_map(|(action, active)| active.then_some(action))
                .chain(buttons)
                .collect(),
        )
    }

    fn replace(&mut self, next: BTreeSet<Action>) -> Vec<Transition> {
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
    use model::{Control as Pad, Move, PadKind, PadPreset, RightStick, TriggerShield, TriggerShields};

    const PRESETS: [PadPreset; 4] = [PadPreset::Melee, PadPreset::ZJump, PadPreset::Tom, PadPreset::Script];
    const SHIELDS: [TriggerShield; 2] = [TriggerShield::Full, TriggerShield::Light];
    const REMAPPABLE: [Pad; 9] = [Pad::A, Pad::B, Pad::X, Pad::Y, Pad::Lb, Pad::Rb, Pad::Lt, Pad::Rt, Pad::LeftStick];

    struct Config {
        preset: PadPreset,
        tap_jump: bool,
        triggers: TriggerShields,
        right_stick: RightStick,
        kind: PadKind,
        remaps: model::Remaps,
    }

    /// The README's fighter layout per preset, by Xbox position.
    fn reference_buttons(config: &Config) -> Vec<(Pad, Action)> {
        use Action::*;
        let shield = |control: Pad| {
            let mode = match control { Pad::Lt => config.triggers.left, Pad::Rt => config.triggers.right, _ => TriggerShield::Full };
            match (control, mode) {
                (_, TriggerShield::Light) => LightShield,
                (Pad::Rt, _) => RightShield,
                _ => Shield,
            }
        };
        let mut table: Vec<(Pad, Action)> = match config.preset {
            PadPreset::Melee => vec![(Pad::A, Attack), (Pad::B, Special), (Pad::X, Jump), (Pad::Y, Jump), (Pad::Rb, Grab)],
            PadPreset::ZJump => vec![(Pad::A, Attack), (Pad::B, Special), (Pad::X, Grab), (Pad::Y, Jump), (Pad::Rb, Jump)],
            PadPreset::Tom => vec![(Pad::A, Attack), (Pad::B, Grab), (Pad::X, Special), (Pad::Y, Jump), (Pad::Lb, ShortHop), (Pad::Rb, Meter)],
            PadPreset::Script => vec![(Pad::A, Attack), (Pad::X, Special), (Pad::B, Jump), (Pad::Y, Jump), (Pad::LeftStick, ShortHop), (Pad::Rb, Grab), (Pad::Lb, Walk)],
        };
        table.extend([(Pad::Lt, shield(Pad::Lt)), (Pad::Rt, shield(Pad::Rt)), (Pad::Start, Start)]);
        if config.kind == PadKind::GameCube {
            for (control, _) in &mut table {
                *control = match *control { Pad::B => Pad::X, Pad::X => Pad::B, other => other };
            }
        }
        table.retain(|(control, _)| !config.remaps.contains_key(control));
        table.extend(config.remaps.iter().filter_map(|(&control, step)| Some((control, match step {
            Move::Attack => Attack,
            Move::Special => Special,
            Move::Jump => Jump,
            Move::Grab => Grab,
            Move::Shield => shield(control),
            Move::Tilt => Walk,
            Move::ShortHop => ShortHop,
            Move::Meter => Meter,
            Move::None => return None,
        }))));
        table
    }

    fn reference_neutral(s: &Sample) -> bool {
        stick::melee_stick(s.left_x, s.left_y) == (0, 0)
            && stick::c_stick(s.right_x, s.right_y) == (0, 0)
            && s.left_trigger <= 4000
            && s.right_trigger <= 4000
            && !(s.a || s.b || s.x || s.y || s.lb || s.rb || s.start || s.left_stick)
    }

    /// What the fighter layout holds for an armed pad: buttons by the table, the
    /// left stick through Melee's clamp, deadzone and down threshold, the right
    /// stick at the C-stick flick thresholds, and stick jump above 0.6625 up
    /// (capped at 0.65 while tilt and shield are both held).
    fn reference_held(config: &Config, s: &Sample) -> BTreeSet<Action> {
        let down = |control: Pad| match control {
            Pad::A => s.a, Pad::B => s.b, Pad::X => s.x, Pad::Y => s.y, Pad::Lb => s.lb, Pad::Rb => s.rb,
            Pad::Start => s.start, Pad::LeftStick => s.left_stick,
            Pad::Lt => s.left_trigger > 4000, Pad::Rt => s.right_trigger > 4000,
            _ => false,
        };
        let mut held: BTreeSet<Action> = reference_buttons(config).into_iter().filter(|(c, _)| down(*c)).map(|(_, a)| a).collect();
        let (lx, ly) = stick::melee_stick(s.left_x, s.left_y);
        let (rx, ry) = stick::c_stick(s.right_x, s.right_y);
        let tilt = held.contains(&Action::Walk);
        let shield = [Action::Shield, Action::RightShield, Action::LightShield].iter().any(|a| held.contains(a));
        let up = -f32::from(ly) / 32_767.0;
        let up = if tilt && shield { up.min(0.65) } else { up };
        let directions = [
            (Action::Jump, config.tap_jump && up > 0.6625),
            (Action::Walk, config.right_stick == RightStick::Tilt && (rx, ry) != (0, 0)),
            (Action::Left, lx < 0),
            (Action::Right, lx > 0),
            (Action::Down, stick::stick_down(ly)),
            (Action::Up, ly < 0),
            (Action::CLeft, rx < 0),
            (Action::CRight, rx > 0),
            (Action::CUp, ry < 0),
            (Action::CDown, ry > 0),
        ];
        held.extend(directions.into_iter().filter(|(_, on)| *on).map(|(a, _)| a));
        held
    }

    fn diff(held: &BTreeSet<Action>, next: &BTreeSet<Action>) -> Vec<Transition> {
        let mut edges: Vec<Transition> = held.difference(next).map(|&action| Transition { action, pressed: false }).collect();
        edges.extend(next.difference(held).map(|&action| Transition { action, pressed: true }));
        edges
    }

    fn axis(rng: &mut Rng) -> i16 {
        let free = rng.next() as i16;
        rng.pick(&[0, 0, 0, 9174, -9174, 9175, 16384, -16384, 19660, -19660, 21708, -21708, 21709, -21709, 22937, -22937, 26500, -26500, 32767, i16::MIN, free])
    }

    fn trigger(rng: &mut Rng) -> i16 {
        rng.pick(&[0, 0, 4000, 4001, 20000, 32767])
    }

    fn sample(rng: &mut Rng) -> Sample {
        if rng.chance(40) {
            return Sample { left_x: rng.pick(&[0, 9174, -9174]), right_x: rng.pick(&[0, 16384, 22937]), right_y: rng.pick(&[0, 22937]), ..Sample::default() };
        }
        if rng.chance(25) {
            return Sample { left_y: rng.pick(&[-32767, -22500]), left_trigger: 20000, lb: rng.chance(70), right_x: rng.pick(&[0, 32767]), ..Sample::default() };
        }
        Sample {
            left_x: axis(rng), left_y: axis(rng), right_x: axis(rng), right_y: axis(rng),
            left_trigger: trigger(rng), right_trigger: trigger(rng),
            a: rng.chance(20), b: rng.chance(20), x: rng.chance(20), y: rng.chance(20),
            lb: rng.chance(20), rb: rng.chance(20), start: rng.chance(10), left_stick: rng.chance(10),
        }
    }

    fn key_name(action: Action) -> String {
        match action.key() { ' ' => "space".to_owned(), key => key.to_string() }
    }

    #[test]
    fn the_fighter_mapper_agrees_with_the_layout_reference_over_generated_sdl_events() {
        let mut rng = Rng::new(5);
        let axes = [Axis::LeftX, Axis::LeftY, Axis::RightX, Axis::RightY, Axis::TriggerLeft, Axis::TriggerRight];
        let buttons = [Button::South, Button::East, Button::West, Button::North, Button::LeftShoulder, Button::RightShoulder, Button::Start, Button::LeftStick, Button::Back, Button::DPadUp];
        for _ in 0..cases() {
            let preset = rng.pick(&PRESETS);
            let mut config = Config { preset, tap_jump: false, triggers: TriggerShields::default(), right_stick: RightStick::Smash, kind: PadKind::Xbox, remaps: Default::default() };
            let mut mapper = EventMapper::new(preset);
            config.tap_jump = rng.chance(50);
            mapper.set_tap_jump(config.tap_jump);
            let (mut sample, mut armed, mut held) = (Sample::default(), false, BTreeSet::new());
            for step in 0..40u64 {
                let roll = rng.below(100);
                let edges = if roll < 55 {
                    let which = if rng.chance(90) { 1 } else { 2 };
                    let event = if rng.chance(45) {
                        let axis = rng.pick(&axes);
                        let value = if matches!(axis, Axis::TriggerLeft | Axis::TriggerRight) { trigger(&mut rng) } else { self::axis(&mut rng) };
                        Event::GamepadAxisMotion { timestamp: step, which: JoystickId::from(which), axis, value }
                    } else {
                        let button = rng.pick(&buttons);
                        if rng.chance(50) {
                            Event::GamepadButtonDown { timestamp: step, which: JoystickId::from(which), button }
                        } else {
                            Event::GamepadButtonUp { timestamp: step, which: JoystickId::from(which), button }
                        }
                    };
                    let Some(input) = selected_input(&event, JoystickId::from(1)) else {
                        assert_eq!(which, 2, "{event:?} from the selected pad was dropped");
                        continue;
                    };
                    assert_eq!(which, 1, "{event:?} from another pad was selected");
                    assert_eq!(input.capture_ns, step);
                    let eligible = rng.chance(95);
                    let edges = mapper.apply(input, eligible);
                    if !eligible || !armed {
                        sample = Sample::default();
                        armed = false;
                        assert_eq!(edges, diff(&held, &BTreeSet::new()));
                        held.clear();
                        continue;
                    }
                    let field = match (input.control, input.value) {
                        (Control::Axis(Axis::LeftX), InputValue::Axis(v)) => { sample.left_x = v; true }
                        (Control::Axis(Axis::LeftY), InputValue::Axis(v)) => { sample.left_y = v; true }
                        (Control::Axis(Axis::RightX), InputValue::Axis(v)) => { sample.right_x = v; true }
                        (Control::Axis(Axis::RightY), InputValue::Axis(v)) => { sample.right_y = v; true }
                        (Control::Axis(Axis::TriggerLeft), InputValue::Axis(v)) => { sample.left_trigger = v; true }
                        (Control::Axis(Axis::TriggerRight), InputValue::Axis(v)) => { sample.right_trigger = v; true }
                        (Control::Button(Button::South), InputValue::Button(v)) => { sample.a = v; true }
                        (Control::Button(Button::East), InputValue::Button(v)) => { sample.b = v; true }
                        (Control::Button(Button::West), InputValue::Button(v)) => { sample.x = v; true }
                        (Control::Button(Button::North), InputValue::Button(v)) => { sample.y = v; true }
                        (Control::Button(Button::LeftShoulder), InputValue::Button(v)) => { sample.lb = v; true }
                        (Control::Button(Button::RightShoulder), InputValue::Button(v)) => { sample.rb = v; true }
                        (Control::Button(Button::Start), InputValue::Button(v)) => { sample.start = v; true }
                        (Control::Button(Button::LeftStick), InputValue::Button(v)) => { sample.left_stick = v; true }
                        _ => false,
                    };
                    if !field {
                        assert!(edges.is_empty(), "an unmapped control pressed {edges:?}");
                        continue;
                    }
                    edges
                } else if roll < 72 {
                    sample = if rng.chance(50) { Sample::default() } else { self::sample(&mut rng) };
                    let eligible = rng.chance(85);
                    let edges = mapper.resync(&sample, eligible);
                    if !eligible {
                        armed = false;
                        assert_eq!(edges, diff(&held, &BTreeSet::new()));
                        held.clear();
                        continue;
                    }
                    if !armed {
                        armed = reference_neutral(&sample);
                        assert!(edges.is_empty(), "pressed while arming: {edges:?}");
                        continue;
                    }
                    edges
                } else if roll < 82 {
                    sample = Sample::default();
                    armed = false;
                    assert_eq!(mapper.disarm(), diff(&held, &BTreeSet::new()));
                    held.clear();
                    continue;
                } else {
                    match rng.below(5) {
                        0 => { config.tap_jump = rng.chance(50); mapper.set_tap_jump(config.tap_jump) }
                        3 => { config.right_stick = rng.pick(&RightStick::ALL); mapper.set_right_stick(config.right_stick) }
                        1 => { config.triggers = TriggerShields { left: rng.pick(&SHIELDS), right: rng.pick(&SHIELDS) }; mapper.set_trigger_shields(config.triggers) }
                        2 => { config.kind = rng.pick(&[PadKind::Xbox, PadKind::GameCube]); mapper.set_pad_kind(config.kind) }
                        _ => {
                            config.remaps = (0..rng.below(4)).map(|_| (rng.pick(&REMAPPABLE), rng.pick(&Move::ALL))).collect();
                            mapper.set_remaps(config.remaps.clone())
                        }
                    }
                    continue;
                };
                let next = reference_held(&config, &sample);
                assert_eq!(edges, diff(&held, &next), "{:?} {:?} {:?} {:?} {sample:?}", config.preset, config.kind, config.triggers, config.remaps);
                held = next;
                assert_eq!(mapper.armed(), armed);
                assert_eq!(mapper.sample(), &sample);
            }
        }

        // The keys pressed are the keys the window shows for the same controls.
        let mut keys = BTreeSet::new();
        let all = [Action::Attack, Action::Special, Action::Jump, Action::ShortHop, Action::Grab, Action::Shield, Action::RightShield, Action::LightShield, Action::Walk, Action::Meter,
            Action::Start, Action::Left, Action::Right, Action::Down, Action::Up, Action::CLeft, Action::CRight, Action::CUp, Action::CDown];
        for action in all {
            assert!(keys.insert(action.key()), "{action:?} shares its key");
        }
        let shown = |control: Pad| model::fighter_bindings().into_iter().find(|b| b.control == control).map(|b| b.press);
        for (control, action) in [(Pad::LeftLeft, Action::Left), (Pad::LeftRight, Action::Right), (Pad::LeftDown, Action::Down), (Pad::LeftUp, Action::Up),
            (Pad::RightUp, Action::CUp), (Pad::RightDown, Action::CDown), (Pad::RightLeft, Action::CLeft), (Pad::RightRight, Action::CRight), (Pad::Start, Action::Start)] {
            assert_eq!(shown(control), Some(model::Press::Key(key_name(action))), "{control:?}");
        }
        let named: BTreeSet<String> = all.into_iter().map(key_name).collect();
        for preset in PRESETS {
            for left in SHIELDS {
                for right in SHIELDS {
                    for binding in RightStick::ALL.into_iter().flat_map(|mode| model::fighter_bindings_with(preset, TriggerShields { left, right }, mode)) {
                        let model::Press::Key(key) = binding.press else { panic!("{binding:?}") };
                        assert!(named.contains(&key), "{} {key}", preset.name());
                    }
                }
            }
        }
    }
}
