//! The Any map profile: turns pad state into key, click and pointer output by
//! configurable [`Binding`]s. Pure; the service delivers its events through its
//! focus gate and keyboard output, and calls [`Mapper::release_all`] on focus
//! loss or disconnect.

use crate::{Binding, Button, Control, InputView, Press};
use std::collections::BTreeSet;

/// A stick axis counts outside 0.28 of full scale, as the Smashcraft layout's sticks do.
pub const STICK_DEADZONE: i16 = 9175;
/// A trigger counts when pulled strictly beyond this.
pub const TRIGGER_THRESHOLD: u16 = 4000;
/// Pointer speed at full stick, in pixels per second.
pub const POINTER_SPEED: f32 = 1400.0;

/// A held output: a key by name, or a mouse button.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Held {
    Key(String),
    LeftClick,
    RightClick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Down(Held),
    Up(Held),
}

pub fn active(input: &InputView, control: Control) -> bool {
    let [lx, ly] = input.left;
    let [rx, ry] = input.right;
    let button = |b: Button| input.pressed(b);
    match control {
        Control::LeftStick => button(Button::LeftStick),
        Control::A => button(Button::A),
        Control::B => button(Button::B),
        Control::X => button(Button::X),
        Control::Y => button(Button::Y),
        Control::Lb => button(Button::Lb),
        Control::Rb => button(Button::Rb),
        Control::Start => button(Button::Start),
        Control::Back => button(Button::Back),
        Control::DpadUp => button(Button::DpadUp),
        Control::DpadDown => button(Button::DpadDown),
        Control::DpadLeft => button(Button::DpadLeft),
        Control::DpadRight => button(Button::DpadRight),
        Control::Lt => input.lt > TRIGGER_THRESHOLD,
        Control::Rt => input.rt > TRIGGER_THRESHOLD,
        Control::LeftUp => ly <= -STICK_DEADZONE,
        Control::LeftDown => ly >= STICK_DEADZONE,
        Control::LeftLeft => lx <= -STICK_DEADZONE,
        Control::LeftRight => lx >= STICK_DEADZONE,
        Control::RightUp => ry <= -STICK_DEADZONE,
        Control::RightDown => ry >= STICK_DEADZONE,
        Control::RightLeft => rx <= -STICK_DEADZONE,
        Control::RightRight => rx >= STICK_DEADZONE,
    }
}

fn held(press: &Press) -> Option<Held> {
    match press {
        Press::Key(name) => Some(Held::Key(name.clone())),
        Press::LeftClick => Some(Held::LeftClick),
        Press::RightClick => Some(Held::RightClick),
        Press::Pointer => None,
    }
}

/// Edge mapper. Controls sharing an output hold it until the last one releases.
/// After construction or [`Mapper::release_all`] it waits for every bound
/// control to return to neutral before pressing anything, so a press held
/// across focus loss is never replayed.
#[derive(Debug, Clone)]
pub struct Mapper {
    bindings: Vec<Binding>,
    held: BTreeSet<Held>,
    armed: bool,
}

impl Mapper {
    pub fn new(bindings: Vec<Binding>) -> Self {
        Self {
            bindings,
            held: BTreeSet::new(),
            armed: false,
        }
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Output transitions for this pad state, releases first.
    pub fn update(&mut self, input: &InputView) -> Vec<Event> {
        let want: BTreeSet<Held> = self
            .bindings
            .iter()
            .filter(|b| active(input, b.control))
            .filter_map(|b| held(&b.press))
            .collect();
        if !self.armed {
            let neutral = self.bindings.iter().all(|b| !active(input, b.control));
            if !neutral {
                return Vec::new();
            }
            self.armed = true;
        }
        let mut events: Vec<Event> = self.held.difference(&want).cloned().map(Event::Up).collect();
        events.extend(want.difference(&self.held).cloned().map(Event::Down));
        self.held = want;
        events
    }

    /// Releases everything this mapper pressed and requires neutral again.
    pub fn release_all(&mut self) -> Vec<Event> {
        self.armed = false;
        std::mem::take(&mut self.held).into_iter().map(Event::Up).collect()
    }

    /// Pointer movement over `seconds` from sticks bound to the pointer.
    /// The deadzone is removed and the remainder rescaled, so motion starts at zero.
    pub fn pointer(&self, input: &InputView, seconds: f32) -> (i32, i32) {
        let on = |c: Control| self.bindings.iter().any(|b| b.control == c && b.press == Press::Pointer);
        let mut total = (0.0f32, 0.0f32);
        for (stick, [x, y]) in [(0, input.left), (1, input.right)] {
            let [up, down, left, right] = if stick == 0 {
                [Control::LeftUp, Control::LeftDown, Control::LeftLeft, Control::LeftRight]
            } else {
                [Control::RightUp, Control::RightDown, Control::RightLeft, Control::RightRight]
            };
            let scale = |v: i16, neg: bool, pos: bool| -> f32 {
                let v = v as f32;
                let dz = STICK_DEADZONE as f32;
                let on = if v < 0.0 { neg } else { pos };
                if !on || v.abs() < dz {
                    0.0
                } else {
                    v.signum() * ((v.abs() - dz) / (32767.0 - dz)).min(1.0)
                }
            };
            total.0 += scale(x, on(left), on(right));
            total.1 += scale(y, on(up), on(down));
        }
        let step = POINTER_SPEED * seconds;
        ((total.0 * step).round() as i32, (total.1 * step).round() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::any_map_bindings;

    fn key(name: &str) -> Held {
        Held::Key(name.into())
    }

    fn armed() -> Mapper {
        let mut m = Mapper::new(any_map_bindings());
        assert!(m.update(&InputView::default()).is_empty());
        m
    }

    #[test]
    fn a_button_press_and_release_are_one_key_down_and_up() {
        let mut m = armed();
        let mut pad = InputView::default();
        pad.press(Button::X, true);
        assert_eq!(m.update(&pad), vec![Event::Down(key("q"))]);
        assert!(m.update(&pad).is_empty());
        pad.press(Button::X, false);
        assert_eq!(m.update(&pad), vec![Event::Up(key("q"))]);
        pad.press(Button::A, true);
        assert_eq!(m.update(&pad), vec![Event::Down(Held::LeftClick)]);
    }

    #[test]
    fn triggers_press_past_their_threshold_and_sticks_press_arrows_past_the_deadzone() {
        let mut m = armed();
        let mut pad = InputView { rt: 4000, left: [-9174, 0], ..InputView::default() };
        assert!(m.update(&pad).is_empty());
        pad.rt = 4001;
        pad.left = [-9175, 20000];
        assert_eq!(
            m.update(&pad),
            vec![Event::Down(key("down")), Event::Down(key("e")), Event::Down(key("left"))]
        );
    }

    #[test]
    fn shared_outputs_hold_until_the_last_control_releases() {
        let mut bindings = any_map_bindings();
        bindings.retain(|b| b.control != Control::Y);
        bindings.push(Binding { control: Control::Y, action: "Ability 1".into(), press: Press::Key("q".into()) });
        let mut m = Mapper::new(bindings);
        let mut pad = InputView::default();
        m.update(&pad);
        pad.press(Button::X, true);
        pad.press(Button::Y, true);
        assert_eq!(m.update(&pad), vec![Event::Down(key("q"))]);
        pad.press(Button::X, false);
        assert!(m.update(&pad).is_empty());
        pad.press(Button::Y, false);
        assert_eq!(m.update(&pad), vec![Event::Up(key("q"))]);
    }

    #[test]
    fn release_all_lets_go_and_waits_for_neutral_before_pressing_again() {
        let mut m = armed();
        let mut pad = InputView::default();
        pad.press(Button::Y, true);
        m.update(&pad);
        assert_eq!(m.release_all(), vec![Event::Up(key("w"))]);
        assert!(m.update(&pad).is_empty(), "a held press is not replayed");
        pad.press(Button::Y, false);
        assert!(m.update(&pad).is_empty());
        pad.press(Button::Y, true);
        assert_eq!(m.update(&pad), vec![Event::Down(key("w"))]);
    }

    #[test]
    fn a_new_mapper_ignores_a_press_held_since_before_it_started() {
        let mut m = Mapper::new(any_map_bindings());
        let mut pad = InputView::default();
        pad.press(Button::X, true);
        assert!(m.update(&pad).is_empty());
    }

    #[test]
    fn the_right_stick_moves_the_pointer_from_zero_at_the_deadzone() {
        let m = armed();
        let pad = InputView { right: [9175, -32767], ..InputView::default() };
        assert_eq!(m.pointer(&pad, 0.01), (0, -14));
        let pad = InputView { left: [32767, 0], ..InputView::default() };
        assert_eq!(m.pointer(&pad, 0.01), (0, 0), "the left stick is the camera");
    }
}
