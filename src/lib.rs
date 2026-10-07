#![forbid(unsafe_code)]

pub mod output;
pub mod pad_ingress;
pub mod stick;
#[cfg(target_os = "linux")]
pub mod service;

pub use wc3_controller_model as model;

use sdl3::{
    event::Event,
    gamepad::{Axis, Button},
    joystick::JoystickId,
};
use std::collections::BTreeSet;

pub const TRIGGER_THRESHOLD: i16 = 4000;

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
}

impl Sample {
    pub fn neutral(&self) -> bool {
        stick::melee_stick(self.left_x, self.left_y) == (0, 0)
            && stick::c_stick(self.right_x, self.right_y) == (0, 0)
            && self.left_trigger <= TRIGGER_THRESHOLD
            && self.right_trigger <= TRIGGER_THRESHOLD
            && ![self.a, self.b, self.x, self.y, self.lb, self.rb, self.start].contains(&true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Attack,
    Special,
    Jump,
    Grab,
    Shield,
    LightShield,
    Walk,
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
    pub fn key(self) -> char {
        match self {
            Self::Attack => 'n',
            Self::Special => 'u',
            Self::Jump => 'i',
            Self::Grab => 'o',
            Self::Shield => 'q',
            Self::LightShield => '9',
            Self::Walk => 'p',
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
            // Ignore future SDL controls until Smashcraft defines a mapping.
            _ => return Vec::new(),
        }
        self.mapper.update(Some(&self.sample), true)
    }
}

/// One selected controller. Loss of eligibility or connection clears its actions;
/// held inputs cannot reactivate until a neutral sample arrives while eligible.
#[derive(Default)]
pub struct Mapper {
    held: BTreeSet<Action>,
    armed: bool,
    preset: model::PadPreset,
}

impl Mapper {
    pub fn new(preset: model::PadPreset) -> Self {
        Self { preset, ..Self::default() }
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
        // Union source states before diffing, so releasing one source never
        // releases an action another source still owns.
        let bindings = [
            (Action::Attack, sample.a),
            (Action::Special, sample.x),
            (Action::Jump, sample.y || if self.preset == model::PadPreset::ZJump { sample.rb } else { sample.b }),
            (Action::Grab, if self.preset == model::PadPreset::ZJump { sample.b } else { sample.rb }),
            (
                Action::Shield,
                sample.right_trigger > TRIGGER_THRESHOLD,
            ),
            (Action::LightShield, sample.left_trigger > TRIGGER_THRESHOLD),
            (Action::Walk, sample.lb),
            (Action::Start, sample.start),
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
mod tests {
    use super::*;
    use sdl3::joystick::JoystickId;
    fn tick(map: &mut Mapper, sample: &Sample) -> Vec<Transition> {
        map.update(Some(sample), true)
    }
    fn edge(action: Action, pressed: bool) -> Transition {
        Transition { action, pressed }
    }
    fn armed() -> Mapper {
        let mut map = Mapper::default();
        tick(&mut map, &Sample::default());
        map
    }

    #[test]
    fn z_jump_uses_rb_and_y_for_jump_and_b_for_grab() {
        let mut map = Mapper::new(model::PadPreset::ZJump);
        tick(&mut map, &Sample::default());
        let mut sample = Sample { rb: true, ..Sample::default() };
        assert_eq!(tick(&mut map, &sample), vec![edge(Action::Jump, true)]);
        sample.y = true;
        assert!(tick(&mut map, &sample).is_empty());
        sample.rb = false;
        assert!(tick(&mut map, &sample).is_empty());
        sample.b = true;
        assert_eq!(tick(&mut map, &sample), vec![edge(Action::Grab, true)]);
        sample.y = false;
        assert_eq!(tick(&mut map, &sample), vec![edge(Action::Jump, false)]);
    }

    #[test]
    fn light_and_full_shield_hold_independently() {
        let mut map = armed();
        let mut s = Sample {
            left_trigger: 20000,
            ..Sample::default()
        };
        assert_eq!(tick(&mut map, &s), vec![edge(Action::LightShield, true)]);
        s.right_trigger = 30000;
        assert_eq!(tick(&mut map, &s), vec![edge(Action::Shield, true)]);
        s.left_trigger = 0;
        assert_eq!(tick(&mut map, &s), vec![edge(Action::LightShield, false)]);
        s.right_trigger = 0;
        assert_eq!(tick(&mut map, &s), vec![edge(Action::Shield, false)]);
    }

    #[test]
    fn jump_buttons_share_one_held_jump_and_stick_up_is_only_up() {
        let mut map = armed();
        let mut s = Sample {
            b: true,
            y: true,
            left_y: -20000,
            ..Sample::default()
        };
        assert_eq!(
            tick(&mut map, &s),
            vec![edge(Action::Jump, true), edge(Action::Up, true)]
        );
        s.b = false;
        assert!(tick(&mut map, &s).is_empty());
        s.y = false;
        assert_eq!(tick(&mut map, &s), vec![edge(Action::Jump, false)]);
        s.left_y = 0;
        assert_eq!(tick(&mut map, &s), vec![edge(Action::Up, false)]);
        s.left_y = i16::MIN;
        assert_eq!(tick(&mut map, &s), vec![edge(Action::Up, true)]);
    }

    #[test]
    fn drifted_stick_inside_melee_deadzone_is_neutral_and_just_outside_moves() {
        let mut map = armed();
        let drift = Sample {
            left_x: 9_174,
            left_y: -9_174,
            ..Sample::default()
        };
        assert!(drift.neutral());
        assert!(tick(&mut map, &drift).is_empty());
        let outside = Sample {
            left_x: 9_175,
            ..Sample::default()
        };
        assert!(!outside.neutral());
        assert_eq!(tick(&mut map, &outside), vec![edge(Action::Right, true)]);
    }

    #[test]
    fn down_needs_melees_strong_threshold_while_other_directions_keep_the_deadzone() {
        let mut map = armed();
        let stick = |left_x, left_y| Sample {
            left_x,
            left_y,
            ..Sample::default()
        };
        assert!(tick(&mut map, &stick(0, 16_384)).is_empty());
        assert_eq!(
            tick(&mut map, &stick(0, 22_937)),
            vec![edge(Action::Down, true)]
        );
        assert_eq!(
            tick(&mut map, &stick(0, 16_384)),
            vec![edge(Action::Down, false)]
        );
        assert_eq!(
            tick(&mut map, &stick(16_384, 0)),
            vec![edge(Action::Right, true)]
        );
        assert_eq!(
            tick(&mut map, &stick(0, -16_384)),
            vec![edge(Action::Right, false), edge(Action::Up, true)]
        );
    }

    #[test]
    fn c_stick_presses_at_melees_smash_flick_thresholds() {
        let mut map = armed();
        let stick = |right_x, right_y| Sample {
            right_x,
            right_y,
            ..Sample::default()
        };
        // 0.5 and 0.7 sideways are below 0.8; 0.6 and 0.7 vertical straddle 0.6625.
        assert!(tick(&mut map, &stick(16_384, 0)).is_empty());
        assert!(tick(&mut map, &stick(22_937, 0)).is_empty());
        assert!(tick(&mut map, &stick(0, -19_660)).is_empty());
        assert_eq!(tick(&mut map, &stick(0, -22_937)), vec![edge(Action::CUp, true)]);
        assert_eq!(
            tick(&mut map, &stick(0, 22_937)),
            vec![edge(Action::CUp, false), edge(Action::CDown, true)]
        );
        assert_eq!(
            tick(&mut map, &stick(-27_000, 0)),
            vec![edge(Action::CDown, false), edge(Action::CLeft, true)]
        );
        assert!(stick(22_937, 0).neutral());
        assert!(!stick(27_000, 0).neutral());
    }

    #[test]
    fn focus_loss_and_disconnect_release_once_then_require_neutral() {
        for disconnect in [false, true] {
            let mut map = armed();
            let held = Sample {
                a: true,
                left_trigger: 20000,
                ..Sample::default()
            };
            tick(&mut map, &held);
            assert_eq!(
                map.update(if disconnect { None } else { Some(&held) }, disconnect),
                vec![edge(Action::Attack, false), edge(Action::LightShield, false)]
            );
            assert!(!map.armed());
            assert!(tick(&mut map, &held).is_empty());
            assert!(!map.armed());
            tick(&mut map, &Sample::default());
            assert!(map.armed());
            assert_eq!(
                tick(&mut map, &held),
                vec![edge(Action::Attack, true), edge(Action::LightShield, true)]
            );
        }
    }

    #[test]
    fn startup_held_input_cannot_arm_and_extreme_axis_is_safe() {
        let mut map = Mapper::default();
        let s = Sample {
            left_x: i16::MIN,
            ..Sample::default()
        };
        assert!(!s.neutral());
        assert!(tick(&mut map, &s).is_empty());
        assert!(!map.armed());
    }

    #[test]
    fn buttons_and_stick_directions_match_requested_keys() {
        let mut map = armed();
        let s = Sample {
            a: true,
            x: true,
            rb: true,
            lb: true,
            start: true,
            left_x: -20000,
            left_y: 22000,
            right_x: 26_500,
            ..Sample::default()
        };
        let keys: BTreeSet<_> = tick(&mut map, &s).iter().map(|t| t.action.key()).collect();
        assert_eq!(
            keys,
            ['n', 'u', 'o', 'p', 'y', 'w', 'e', 'm']
                .into_iter()
                .collect()
        );
        assert_eq!(Action::Right.key(), 'r');
        assert_eq!(Action::CLeft.key(), 'b');
        assert_eq!(Action::CDown.key(), 'h');
    }

    fn button(timestamp: u64, which: u32, button: Button, down: bool) -> Event {
        if down {
            Event::GamepadButtonDown {
                timestamp,
                which: JoystickId::from(which),
                button,
            }
        } else {
            Event::GamepadButtonUp {
                timestamp,
                which: JoystickId::from(which),
                button,
            }
        }
    }

    fn axis(timestamp: u64, which: u32, axis: Axis, value: i16) -> Event {
        Event::GamepadAxisMotion {
            timestamp,
            which: JoystickId::from(which),
            axis,
            value,
        }
    }

    fn capture(mapper: &mut EventMapper, event: &Event, selected: u32) -> Vec<Transition> {
        selected_input(event, JoystickId::from(selected))
            .map(|input| mapper.apply(input, true))
            .unwrap_or_default()
    }

    #[test]
    fn same_batch_button_down_and_up_remain_two_ordered_edges() {
        let mut mapper = EventMapper::default();
        mapper.resync(&Sample::default(), true);
        let down = button(10, 1, Button::South, true);
        let up = button(11, 1, Button::South, false);
        assert_eq!(
            selected_input(&down, JoystickId::from(1))
                .unwrap()
                .capture_ns,
            10
        );
        assert_eq!(
            capture(&mut mapper, &down, 1),
            vec![edge(Action::Attack, true)]
        );
        assert_eq!(
            capture(&mut mapper, &up, 1),
            vec![edge(Action::Attack, false)]
        );
    }

    #[test]
    fn axis_excursion_and_return_in_one_batch_remain_two_edges() {
        let mut mapper = EventMapper::default();
        mapper.resync(&Sample::default(), true);
        let out = axis(20, 1, Axis::LeftX, 20_000);
        let home = axis(21, 1, Axis::LeftX, 0);
        assert_eq!(
            capture(&mut mapper, &out, 1),
            vec![edge(Action::Right, true)]
        );
        assert_eq!(
            capture(&mut mapper, &home, 1),
            vec![edge(Action::Right, false)]
        );
    }

    #[test]
    fn unrelated_gamepad_events_do_not_enter_selected_history() {
        let mut mapper = EventMapper::default();
        mapper.resync(&Sample::default(), true);
        let event = button(30, 2, Button::South, true);
        assert!(selected_input(&event, JoystickId::from(1)).is_none());
        assert!(capture(&mut mapper, &event, 1).is_empty());
        let selected_event = button(31, 1, Button::South, true);
        assert_eq!(
            capture(&mut mapper, &selected_event, 1),
            vec![edge(Action::Attack, true)]
        );
    }

    #[test]
    fn overlapping_jump_and_shield_sources_keep_the_union_held() {
        let mut mapper = EventMapper::default();
        mapper.resync(&Sample::default(), true);
        assert_eq!(
            capture(&mut mapper, &button(1, 1, Button::East, true), 1),
            vec![edge(Action::Jump, true)]
        );
        assert!(capture(&mut mapper, &button(2, 1, Button::North, true), 1).is_empty());
        assert!(capture(&mut mapper, &button(3, 1, Button::East, false), 1).is_empty());
        assert_eq!(
            capture(&mut mapper, &button(4, 1, Button::North, false), 1),
            vec![edge(Action::Jump, false)]
        );
        assert_eq!(
            capture(&mut mapper, &axis(5, 1, Axis::TriggerLeft, 20_000), 1),
            vec![edge(Action::LightShield, true)]
        );
        assert_eq!(capture(&mut mapper, &axis(6, 1, Axis::TriggerRight, 20_000), 1), vec![edge(Action::Shield, true)]);
        assert_eq!(capture(&mut mapper, &axis(7, 1, Axis::TriggerLeft, 0), 1), vec![edge(Action::LightShield, false)]);
        assert_eq!(
            capture(&mut mapper, &axis(8, 1, Axis::TriggerRight, 0), 1),
            vec![edge(Action::Shield, false)]
        );
    }

    #[test]
    fn focus_loss_and_disconnect_require_a_fresh_neutral_baseline() {
        let mut mapper = EventMapper::default();
        mapper.resync(&Sample::default(), true);
        let press = button(1, 1, Button::South, true);
        assert_eq!(
            capture(&mut mapper, &press, 1),
            vec![edge(Action::Attack, true)]
        );
        assert_eq!(mapper.disarm(), vec![edge(Action::Attack, false)]);
        assert!(capture(&mut mapper, &button(2, 1, Button::South, false), 1).is_empty());
        mapper.resync(
            &Sample {
                a: true,
                ..Sample::default()
            },
            true,
        );
        assert!(!mapper.armed());
        assert!(capture(&mut mapper, &button(3, 1, Button::South, true), 1).is_empty());
        mapper.resync(&Sample::default(), true);
        assert!(mapper.armed());
        assert_eq!(
            capture(&mut mapper, &button(4, 1, Button::South, true), 1),
            vec![edge(Action::Attack, true)]
        );
        assert_eq!(mapper.disarm(), vec![edge(Action::Attack, false)]);
        assert!(capture(&mut mapper, &button(5, 1, Button::South, true), 1).is_empty());
    }
}
