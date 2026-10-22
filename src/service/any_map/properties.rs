use super::*;
use crate::rng::{Rng, cases};
use model::{Button, Control, Press};
use std::collections::BTreeSet;

#[derive(Default)]
struct Recorded(Vec<String>);

impl Output for Recorded {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        assert!(key_of(name).is_some(), "{name} has no output key");
        self.0.push(format!("{} {name}", if down { "down" } else { "up" }));
        Ok(())
    }
    fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
        self.0.push(format!("{} {}", if down { "down" } else { "up" }, if right { "right-click" } else { "left-click" }));
        Ok(())
    }
    fn pointer(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        self.0.push(format!("move {dx:.3},{dy:.3}"));
        Ok(())
    }
    fn cursor(&mut self, x: f64, y: f64, _: u32, _: u32) -> Result<(), String> {
        self.0.push(format!("cursor {x:.3},{y:.3}"));
        Ok(())
    }
}

impl pad_ingress::Output for Recorded {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> { Output::key(self, name, down) }
    fn cursor(&mut self, x: f64, y: f64, width: u32, height: u32) -> Result<(), String> { Output::cursor(self, x, y, width, height) }
}

fn reference_sample(input: &InputView) -> crate::Sample {
    let on = |button: Button| input.pressed(button);
    crate::Sample {
        left_x: input.left[0], left_y: input.left[1], right_x: input.right[0], right_y: input.right[1],
        left_trigger: input.lt as i16, right_trigger: input.rt as i16,
        a: on(Button::A), b: on(Button::B), x: on(Button::X), y: on(Button::Y), lb: on(Button::Lb), rb: on(Button::Rb),
        start: on(Button::Start), left_stick: on(Button::LeftStick),
    }
}

fn deliver(events: Vec<Event>, out: &mut Recorded) {
    for event in events {
        let (held, down) = match event { Event::Down(held) => (held, true), Event::Up(held) => (held, false) };
        match held {
            Held::Key(name) => Output::key(out, &name, down).unwrap(),
            Held::LeftClick => out.click(false, down).unwrap(),
            Held::RightClick => out.click(true, down).unwrap(),
        }
    }
}

fn deliver_keys(transitions: Vec<crate::Transition>, out: &mut Recorded) {
    for t in transitions {
        let key = t.action.key().to_string();
        Output::key(out, &key, t.pressed).unwrap();
    }
}

/// The menu pointer as specified: still inside 0.12 of full scale, then speed
/// ((deflection - 0.12) / 0.88)^1.7 of the window width per second along the stick.
fn menu_motion(curve: MenuCurve, [x, y]: [i16; 2], seconds: f32) -> (f64, f64) {
    let (x, y) = (f64::from(x) / 32767.0, f64::from(y) / 32767.0);
    let length = x.hypot(y);
    if length <= 0.12 { return (0.0, 0.0); }
    let speed = curve.full_speed * ((length.min(1.0) - 0.12) / 0.88).powf(1.7) * f64::from(seconds);
    (x / length * speed, y / length * speed)
}

enum Kind { AnyMap, Menu(bool), Keys(Route) }

fn input(rng: &mut Rng) -> InputView {
    if rng.chance(35) {
        return InputView::default();
    }
    let axis = |rng: &mut Rng| { let free = rng.next() as i16; rng.pick(&[0, 0, 3900, 9175, -9175, 16384, -23170, 32767, -32767, free]) };
    let mut view = InputView { left: [axis(rng), axis(rng)], right: [axis(rng), axis(rng)], lt: rng.pick(&[0, 0, 20000, 32767]), rt: rng.pick(&[0, 0, 32767]), buttons: 0 };
    for _ in 0..rng.below(3) {
        view.press(rng.pick(&[Button::A, Button::B, Button::X, Button::Y, Button::Lb, Button::Rb, Button::Start, Button::Back, Button::LeftStick, Button::DpadUp]), true);
    }
    view
}

#[test]
fn the_driver_gates_presses_on_focus_and_agrees_with_its_mappers() {
    let mut rng = Rng::new(6);
    let mut clock = Instant::now();
    let grid = pad_ingress::CursorGrid { x: 10.0, y: 20.0, width: 127.0, height: 254.0, screen_width: 1920, screen_height: 1080 };
    for _ in 0..cases() {
        let kind = match rng.below(4) {
            0 => Kind::AnyMap,
            1 => Kind::Menu(rng.chance(50)),
            _ => Kind::Keys(rng.pick(&[Route::Digital, Route::Keys, Route::Cursor(grid)])),
        };
        let preset = rng.pick(&[model::PadPreset::Melee, model::PadPreset::ZJump, model::PadPreset::Tom]);
        let curve = MenuCurve::for_window_width(2560.0);
        let mut driver = match kind {
            Kind::AnyMap => Driver::new(default_bindings()),
            Kind::Menu(start) => Driver::menu(curve, start),
            Kind::Keys(route) => Driver::keys_with_ingress(preset, route),
        };
        let mut shadow = Mapper::new(default_bindings());
        let mut keys = crate::Mapper::new(preset);
        let mut analog = Ingress::new(match kind { Kind::Keys(route) => route, _ => Route::Digital });
        let (mut was_focused, mut armed, mut menu_held) = (false, false, BTreeSet::new());
        let mut down = BTreeSet::new();
        for _ in 0..30 {
            let (mut out, mut expected) = (Recorded::default(), Recorded::default());
            let roll = rng.below(100);
            let focused = rng.chance(80);
            if roll < 8 {
                driver.release(&mut out).unwrap();
                match kind {
                    Kind::Keys(_) => { deliver_keys(keys.update(None, false), &mut expected); analog.release(&mut expected).unwrap(); }
                    Kind::AnyMap => deliver(shadow.release_all(), &mut expected),
                    Kind::Menu(_) => expected.0.extend(std::mem::take(&mut menu_held).into_iter().map(|held| format!("up {held}"))),
                }
                let mut got = out.0.clone();
                got.sort();
                expected.0.sort();
                assert_eq!(got, expected.0);
                armed = false;
            } else if roll < 14 && matches!(kind, Kind::AnyMap) {
                let mut bindings = default_bindings();
                bindings.retain(|_| rng.chance(70));
                if rng.chance(50) {
                    bindings.retain(|b| b.control != Control::X);
                    bindings.push(Binding { control: Control::X, action: "Hold".into(), press: Press::Key("h".into()) });
                }
                driver.bind(bindings.clone(), &mut out).unwrap();
                deliver(shadow.release_all(), &mut expected);
                shadow = Mapper::new(bindings);
                assert_eq!(out.0, expected.0);
            } else {
                let input = input(&mut rng);
                let seconds = rng.pick(&[0.0, 0.0, 0.01, 0.1]);
                driver.step(&input, focused, seconds, { clock += crate::MIN_HOLD; clock }, &mut out).unwrap();
                match kind {
                    Kind::Keys(_) => {
                        let sample = reference_sample(&input);
                        deliver_keys(keys.update(Some(&sample), focused), &mut expected);
                        analog.update(&sample, focused && keys.armed(), &mut expected).unwrap();
                        assert_eq!(out.0, expected.0);
                    }
                    Kind::AnyMap => {
                        if focused {
                            deliver(shadow.update(&input), &mut expected);
                            if seconds > 0.0 {
                                let (dx, dy) = shadow.pointer(&input, seconds);
                                if (dx, dy) != (0, 0) { expected.pointer(f64::from(dx), f64::from(dy)).unwrap(); }
                            }
                        } else if was_focused {
                            deliver(shadow.release_all(), &mut expected);
                        }
                        assert_eq!(out.0, expected.0);
                    }
                    Kind::Menu(start_on_keys) => {
                        if !focused {
                            if was_focused {
                                for held in std::mem::take(&mut menu_held) { expected.0.push(format!("up {held}")); }
                            }
                            armed = false;
                        } else {
                            let mut want = BTreeSet::new();
                            if input.pressed(Button::A) { want.insert("left-click".to_owned()); }
                            if input.pressed(Button::B) { want.insert("right-click".to_owned()); }
                            if start_on_keys && input.pressed(Button::Start) { want.insert("y".to_owned()); }
                            if !armed && want.is_empty() { armed = true; }
                            if armed {
                                let mut edges: Vec<String> = menu_held.difference(&want).map(|h| format!("up {h}")).collect();
                                edges.extend(want.difference(&menu_held).map(|h| format!("down {h}")));
                                let mut got: Vec<String> = out.0.iter().filter(|e| !e.starts_with("move")).cloned().collect();
                                got.sort();
                                edges.sort();
                                assert_eq!(got, edges, "{input:?}");
                                menu_held = want;
                            } else {
                                assert!(out.0.iter().all(|e| e.starts_with("move")), "{:?}", out.0);
                            }
                            let (dx, dy) = if seconds > 0.0 { menu_motion(curve, input.left, seconds) } else { (0.0, 0.0) };
                            let moved: Vec<&String> = out.0.iter().filter(|e| e.starts_with("move")).collect();
                            if (dx, dy) == (0.0, 0.0) {
                                assert!(moved.is_empty(), "{input:?} {moved:?}");
                            } else {
                                assert_eq!(moved, [&format!("move {dx:.3},{dy:.3}")], "{input:?}");
                            }
                            expected.0 = out.0.clone();
                        }
                        if !focused { assert_eq!(out.0, expected.0); }
                    }
                }
                was_focused = focused;
            }
            for event in &out.0 {
                if let Some(name) = event.strip_prefix("down ") {
                    assert!(down.insert(name.to_owned()), "{name} pressed twice");
                } else if let Some(name) = event.strip_prefix("up ") {
                    assert!(down.remove(name), "{name} released while up");
                } else {
                    assert!(was_focused, "{event} while unfocused");
                }
            }
            if !was_focused || roll < 8 {
                assert!(down.is_empty(), "held while unfocused or released: {down:?}");
            }
        }
    }
}
