//! The Any map profile: turns pad state into key, click and pointer output by
//! configurable [`Binding`]s. Pure; the service delivers its events through its
//! focus gate and keyboard output, and calls [`Mapper::release_all`] on focus
//! loss or disconnect.

use crate::{Binding, Button, Control, InputView, Press};
use std::collections::BTreeSet;

/// A stick axis counts outside 0.28 of full scale, as the fighter layout's sticks do.
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
mod properties {
    use super::*;
    use crate::rng::{Rng, cases};

    const CONTROLS: [Control; 23] = [
        Control::LeftStick, Control::A, Control::B, Control::X, Control::Y, Control::Lb, Control::Rb, Control::Lt, Control::Rt,
        Control::Start, Control::Back, Control::DpadUp, Control::DpadDown, Control::DpadLeft, Control::DpadRight,
        Control::LeftUp, Control::LeftDown, Control::LeftLeft, Control::LeftRight, Control::RightUp, Control::RightDown, Control::RightLeft, Control::RightRight,
    ];
    const BUTTONS: [(Control, Button); 13] = [
        (Control::LeftStick, Button::LeftStick), (Control::A, Button::A), (Control::B, Button::B), (Control::X, Button::X), (Control::Y, Button::Y),
        (Control::Lb, Button::Lb), (Control::Rb, Button::Rb), (Control::Start, Button::Start), (Control::Back, Button::Back),
        (Control::DpadUp, Button::DpadUp), (Control::DpadDown, Button::DpadDown), (Control::DpadLeft, Button::DpadLeft), (Control::DpadRight, Button::DpadRight),
    ];
    // README "Any map": a trigger counts beyond 4000, a stick direction at 0.28 of full scale (9175).
    const TRIGGER: u16 = 4000;
    const DEADZONE: i32 = 9175;

    fn reference_active(input: &InputView, control: Control) -> bool {
        let [lx, ly] = input.left.map(i32::from);
        let [rx, ry] = input.right.map(i32::from);
        if let Some((_, button)) = BUTTONS.iter().find(|(c, _)| *c == control) {
            return input.buttons & (1 << (*button as u32)) != 0;
        }
        match control {
            Control::Lt => input.lt > TRIGGER,
            Control::Rt => input.rt > TRIGGER,
            Control::LeftUp => ly <= -DEADZONE,
            Control::LeftDown => ly >= DEADZONE,
            Control::LeftLeft => lx <= -DEADZONE,
            Control::LeftRight => lx >= DEADZONE,
            Control::RightUp => ry <= -DEADZONE,
            Control::RightDown => ry >= DEADZONE,
            Control::RightLeft => rx <= -DEADZONE,
            Control::RightRight => rx >= DEADZONE,
            _ => unreachable!(),
        }
    }

    fn reference_pointer(bindings: &[Binding], input: &InputView, seconds: f32) -> (i32, i32) {
        let on = |c: Control| bindings.iter().any(|b| b.control == c && b.press == Press::Pointer);
        let scale = |v: i16, negative: Control, positive: Control| -> f64 {
            let v = f64::from(v);
            let bound = if v < 0.0 { on(negative) } else { on(positive) };
            if !bound || v.abs() < DEADZONE as f64 { 0.0 } else { v.signum() * ((v.abs() - DEADZONE as f64) / (32767.0 - DEADZONE as f64)).min(1.0) }
        };
        let x = scale(input.left[0], Control::LeftLeft, Control::LeftRight) + scale(input.right[0], Control::RightLeft, Control::RightRight);
        let y = scale(input.left[1], Control::LeftUp, Control::LeftDown) + scale(input.right[1], Control::RightUp, Control::RightDown);
        let step = 1400.0 * f64::from(seconds);
        ((x * step).round() as i32, (y * step).round() as i32)
    }

    fn axis(rng: &mut Rng) -> i16 {
        let free = rng.next() as i16;
        rng.pick(&[0, 0, 9174, -9174, 9175, -9175, 20000, -20000, 32767, -32767, i16::MIN, free])
    }

    fn input(rng: &mut Rng) -> InputView {
        if rng.chance(35) {
            return InputView { left: [rng.pick(&[0, 9174, -9174]), 0], ..InputView::default() };
        }
        let mut view = InputView {
            left: [axis(rng), axis(rng)],
            right: [axis(rng), axis(rng)],
            lt: { let free = rng.below(32768) as u16; rng.pick(&[0, 4000, 4001, 32767, free]) },
            rt: rng.pick(&[0, 4000, 4001, 32767]),
            buttons: 0,
        };
        for _ in 0..rng.below(3) {
            view.press(rng.pick(&BUTTONS).1, true);
        }
        view
    }

    #[test]
    fn the_mapper_agrees_with_a_reference_over_generated_bindings_and_pads() {
        let mut rng = Rng::new(3);
        let presses = [Press::Key("q".into()), Press::Key("w".into()), Press::LeftClick, Press::RightClick, Press::Pointer];
        for _ in 0..cases() {
            let bindings: Vec<Binding> = (0..1 + rng.below(6))
                .map(|_| Binding { control: rng.pick(&CONTROLS), action: String::new(), press: rng.pick(&presses) })
                .collect();
            let mut mapper = Mapper::new(bindings.clone());
            let (mut armed, mut held) = (false, BTreeSet::<Held>::new());
            for _ in 0..16 {
                if rng.chance(10) {
                    assert_eq!(mapper.release_all(), held.iter().cloned().map(Event::Up).collect::<Vec<_>>());
                    held.clear();
                    armed = false;
                    continue;
                }
                let input = input(&mut rng);
                let events = mapper.update(&input);
                let active: Vec<&Binding> = bindings.iter().filter(|b| reference_active(&input, b.control)).collect();
                if !armed {
                    if !active.is_empty() {
                        assert!(events.is_empty(), "pressed before neutral: {events:?}");
                        continue;
                    }
                    armed = true;
                }
                let want: BTreeSet<Held> = active.iter().filter_map(|b| match &b.press {
                    Press::Key(name) => Some(Held::Key(name.clone())),
                    Press::LeftClick => Some(Held::LeftClick),
                    Press::RightClick => Some(Held::RightClick),
                    Press::Pointer => None,
                }).collect();
                let mut expected: Vec<Event> = held.difference(&want).cloned().map(Event::Up).collect();
                expected.extend(want.difference(&held).cloned().map(Event::Down));
                assert_eq!(events, expected, "{bindings:?} {input:?}");
                held = want;
                assert_eq!(mapper.pointer(&input, 0.01), reference_pointer(&bindings, &input, 0.01), "{bindings:?} {input:?}");
            }
        }
    }
}
