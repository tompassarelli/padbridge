//! Pad-driven desktop output: the Any map profile (the pad's state through
//! [`model::any_map::Mapper`] into keys, clicks and pointer motion) and the
//! menu pointer for a map plug-in's menus (the left stick moves the pointer with a Smash-like
//! curve, A and B click). Output happens only while Warcraft III's window has
//! focus; losing focus releases everything held, and nothing is pressed again
//! until the pad returns to neutral. Keys go through XTEST on the game's X11
//! display (enigo); the pointer and clicks through the compositor's virtual
//! pointer (pointer.rs).

use crate::model::{
    self, Binding, InputView,
    any_map::{Event, Held, Mapper},
};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

mod desktop;
mod niri;

use super::pointer;
pub use desktop::{DesktopOutput, key_of};
pub use niri::{niri_window_width, niri_focused};

/// Where presses go.
pub trait Output {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String>;
    fn click(&mut self, right: bool, down: bool) -> Result<(), String>;
    /// Relative motion in logical pixels.
    fn pointer(&mut self, dx: f64, dy: f64) -> Result<(), String>;
    fn cursor(&mut self, _x: f64, _y: f64, _width: u32, _height: u32) -> Result<(), String> {
        Err("output has no absolute pointer".into())
    }
}

struct AnalogOutput<'a>(&'a mut dyn Output);
impl crate::pad_ingress::Output for AnalogOutput<'_> {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> { self.0.key(name, down) }
    fn cursor(&mut self, x: f64, y: f64, width: u32, height: u32) -> Result<(), String> { self.0.cursor(x, y, width, height) }
}

/// The menu pointer's feel: a small radial deadzone, then speed rising with
/// deflection to `full_speed` (logical pixels a second) at full tilt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuCurve {
    pub full_speed: f64,
}

/// Deflection (of full scale) below which the stick rests.
pub const MENU_DEADZONE: f64 = 0.12;
/// Speed grows with deflection to this power: fine aim near the centre.
pub const MENU_ACCELERATION: f64 = 1.7;
impl MenuCurve {
    pub fn for_window_width(width: f64) -> Self {
        Self { full_speed: width }
    }

    /// Pointer velocity for a stick position (SDL axes, y down positive).
    pub fn velocity(&self, [x, y]: [i16; 2]) -> (f64, f64) {
        let (x, y) = (f64::from(x) / 32767.0, f64::from(y) / 32767.0);
        let length = x.hypot(y);
        if length <= MENU_DEADZONE {
            return (0.0, 0.0);
        }
        let tilt = ((length.min(1.0) - MENU_DEADZONE) / (1.0 - MENU_DEADZONE)).powf(MENU_ACCELERATION);
        let speed = self.full_speed * tilt / length;
        (x * speed, y * speed)
    }
}

/// Sessions with a helper send Start through it; sessions on keys send it here.
pub fn menu_bindings(start_on_keys: bool) -> Vec<Binding> {
    model::menu_pointer_bindings().into_iter()
        .filter(|binding| matches!(binding.press, model::Press::LeftClick | model::Press::RightClick)
            || (start_on_keys && binding.control == model::Control::Start))
        .collect()
}

/// Turns pad states into output; no I/O of its own.
pub struct Driver {
    mapper: Mapper,
    focused: bool,
    menu: Option<MenuCurve>,
    /// The fighter layout on keys: this mapper replaces the bindings' one.
    keys: Option<crate::Mapper>,
    analog: crate::pad_ingress::Ingress,
}

impl Driver {
    pub fn new(bindings: Vec<Binding>) -> Self {
        Self { mapper: Mapper::new(bindings), focused: false, menu: None, keys: None, analog: crate::pad_ingress::Ingress::new(crate::pad_ingress::Route::Digital) }
    }

    /// The menu pointer: the left stick moves the pointer by `curve`, A and B click.
    pub fn menu(curve: MenuCurve, start_on_keys: bool) -> Self {
        Self { mapper: Mapper::new(menu_bindings(start_on_keys)), focused: false, menu: Some(curve), keys: None, analog: crate::pad_ingress::Ingress::new(crate::pad_ingress::Route::Digital) }
    }

    /// The fighter layout on keys. Focus loss releases every key, and nothing presses
    /// again until the pad returns to neutral while focused.
    pub fn keys(preset: model::PadPreset) -> Self {
        Self::keys_with_ingress(preset, crate::pad_ingress::Route::Digital)
    }

    pub fn keys_with_ingress(preset: model::PadPreset, route: crate::pad_ingress::Route) -> Self {
        Self { mapper: Mapper::new(Vec::new()), focused: false, menu: None, keys: Some(crate::Mapper::new(preset)), analog: crate::pad_ingress::Ingress::new(route) }
    }

    /// The fighter layout on keys with the player's settings.
    pub fn keys_for(settings: &model::ControllerSettings, route: crate::pad_ingress::Route) -> Self {
        let mut driver = Self::keys_with_ingress(settings.pad_preset, route);
        if let Some(mapper) = &mut driver.keys {
            mapper.set_tap_jump(settings.tap_jump);
            mapper.set_right_stick(settings.right_stick);
            mapper.set_trigger_shields(settings.triggers);
            mapper.set_remaps(settings.remaps.clone());
        }
        driver
    }

    fn deliver_keys(transitions: Vec<crate::Transition>, out: &mut dyn Output) -> Result<(), String> {
        for transition in transitions {
            let key = transition.action.key().to_string();
            out.key(&key, transition.pressed)?;
        }
        Ok(())
    }

    fn deliver(events: Vec<Event>, out: &mut dyn Output) -> Result<(), String> {
        for event in events {
            let (held, down) = match event {
                Event::Down(held) => (held, true),
                Event::Up(held) => (held, false),
            };
            match held {
                Held::Key(name) => out.key(&name, down)?,
                Held::LeftClick => out.click(false, down)?,
                Held::RightClick => out.click(true, down)?,
            }
        }
        Ok(())
    }

    /// One pad state. `seconds` since the last pointer step moves the pointer;
    /// 0 changes only presses.
    pub fn step(&mut self, input: &InputView, focused: bool, seconds: f32, out: &mut dyn Output) -> Result<(), String> {
        if let Some(keys) = &mut self.keys {
            self.focused = focused;
            let sample = sample_of(input);
            Self::deliver_keys(keys.update(Some(&sample), focused), out)?;
            return self.analog.update(&sample, focused && keys.armed(), &mut AnalogOutput(out));
        }
        if !focused {
            if self.focused {
                Self::deliver(self.mapper.release_all(), out)?;
            }
            self.focused = false;
            return Ok(());
        }
        self.focused = true;
        Self::deliver(self.mapper.update(input), out)?;
        if seconds > 0.0 {
            let (dx, dy) = match self.menu {
                Some(curve) => {
                    let (vx, vy) = curve.velocity(input.left);
                    (vx * f64::from(seconds), vy * f64::from(seconds))
                }
                None => {
                    let (dx, dy) = self.mapper.pointer(input, seconds);
                    (f64::from(dx), f64::from(dy))
                }
            };
            if (dx, dy) != (0.0, 0.0) {
                out.pointer(dx, dy)?;
            }
        }
        Ok(())
    }

    /// New bindings: what the old ones held is released first.
    pub fn bind(&mut self, bindings: Vec<Binding>, out: &mut dyn Output) -> Result<(), String> {
        if self.keys.is_some() {
            return Ok(());
        }
        Self::deliver(self.mapper.release_all(), out)?;
        self.mapper = Mapper::new(bindings);
        Ok(())
    }

    pub fn release(&mut self, out: &mut dyn Output) -> Result<(), String> {
        if let Some(keys) = &mut self.keys {
            Self::deliver_keys(keys.update(None, false), out)?;
            return self.analog.release(&mut AnalogOutput(out));
        }
        Self::deliver(self.mapper.release_all(), out)
    }
}

/// What the output does with the pad.
#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    AnyMap(Vec<Binding>),
    /// The map plug-in's menus: pointer and clicks only.
    Menu { start_on_keys: bool },
    /// A map session on keys: the controller-as-keys mapper of
    /// `padbridge --emit` ([`crate::Mapper`]), the fighter layout
    /// (README, "Fighter layout").
    Keys(model::ControllerSettings),
}

/// Which output runs, for the service to compare and log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    AnyMap,
    Menu { start_on_keys: bool },
    Keys,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::AnyMap => "Any map",
            Kind::Menu { .. } => "menu pointer",
            Kind::Keys => "fighter keys",
        }
    }
}

/// The service's pad state as the controller-as-keys mapper reads it (SDL's axes and trigger range).
pub fn sample_of(input: &InputView) -> crate::Sample {
    let button = |button: model::Button| input.buttons & (1 << (button as u32)) != 0;
    let trigger = |value: u16| value.min(i16::MAX as u16) as i16;
    crate::Sample {
        left_x: input.left[0],
        left_y: input.left[1],
        right_x: input.right[0],
        right_y: input.right[1],
        left_trigger: trigger(input.lt),
        right_trigger: trigger(input.rt),
        a: button(model::Button::A),
        b: button(model::Button::B),
        x: button(model::Button::X),
        y: button(model::Button::Y),
        lb: button(model::Button::Lb),
        rb: button(model::Button::Rb),
        start: button(model::Button::Start),
        left_stick: button(model::Button::LeftStick),
    }
}

/// What the running output is fed.
pub enum Feed {
    Input(InputView),
    Bindings(Vec<Binding>),
}

/// The game window the output serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Window {
    /// Warcraft III's window: presses go to its X11 display while niri focuses it.
    Niri { display: String, niri_socket: PathBuf, niri_window: u64 },
    /// A stand-in game with no window, always focused: each press is appended
    /// to this file as a line (`down q`, `up q`, `click left down`, `pointer DX DY`).
    File(PathBuf),
}

/// Presses written as lines, for a stand-in game.
struct FileOutput(fs::File);

impl Output for FileOutput {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        writeln!(self.0, "{} {name}", if down { "down" } else { "up" }).map_err(|error| error.to_string())
    }
    fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
        writeln!(self.0, "click {} {}", if right { "right" } else { "left" }, if down { "down" } else { "up" }).map_err(|error| error.to_string())
    }
    fn pointer(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        writeln!(self.0, "pointer {dx:.0} {dy:.0}").map_err(|error| error.to_string())
    }
}

/// How often the pointer moves while a stick holds it, and focus is checked.
const TICK: Duration = Duration::from_millis(10);
const FOCUS_EVERY: Duration = Duration::from_millis(50);

/// Runs the profile for `window` until `stop` or the feed ends, releasing what
/// it holds at the end; `focused` mirrors the game window's focus.
pub fn spawn(window: Window, mode: Mode, feed: mpsc::Receiver<Feed>, stop: Arc<AtomicBool>, focused: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let opened: Result<Box<dyn Output>, String> = match &window {
            Window::Niri { display, .. } => DesktopOutput::new(display).map(|out| Box::new(out) as Box<dyn Output>),
            Window::File(path) => fs::OpenOptions::new().create(true).append(true).open(path)
                .map(|file| Box::new(FileOutput(file)) as Box<dyn Output>).map_err(|error| format!("{}: {error}", path.display())),
        };
        let mut out = match opened {
            Ok(out) => out,
            Err(error) => {
                eprintln!("service: pad output failed: {error}");
                return;
            }
        };
        let out = out.as_mut();
        let mut driver = match mode {
            Mode::AnyMap(bindings) => Driver::new(bindings),
            Mode::Menu { start_on_keys } => {
                let width = match &window {
                    Window::Niri { niri_socket, niri_window, .. } => niri_window_width(niri_socket, *niri_window).unwrap_or(2560.0),
                    Window::File(_) => 2560.0,
                };
                Driver::menu(MenuCurve::for_window_width(width), start_on_keys)
            }
            Mode::Keys(settings) => match crate::pad_ingress::Route::from_env() {
                Ok(route) => Driver::keys_for(&settings, route),
                Err(error) => { eprintln!("service: {error}"); return; }
            },
        };
        let mut input = InputView::default();
        let (mut checked, mut is_focused) = (Instant::now() - FOCUS_EVERY, false);
        let mut moved = Instant::now();
        let result = (|| -> Result<(), String> {
            while !stop.load(Ordering::Relaxed) {
                let mut fresh = Vec::new();
                match feed.recv_timeout(TICK) {
                    Ok(item) => fresh.push(item),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                fresh.extend(feed.try_iter());
                if checked.elapsed() >= FOCUS_EVERY {
                    let now = match &window {
                        Window::Niri { niri_socket, niri_window, .. } => niri_focused(niri_socket, *niri_window),
                        Window::File(_) => true,
                    };
                    if now != is_focused {
                        eprintln!("service: pad output {}", if now { "focused" } else { "unfocused: released" });
                    }
                    is_focused = now;
                    focused.store(now, Ordering::Relaxed);
                    checked = Instant::now();
                }
                // Every state, so a tap between ticks still presses and releases.
                for item in fresh {
                    match item {
                        Feed::Input(view) => {
                            input = view;
                            driver.step(&input, is_focused, 0.0, out)?;
                        }
                        Feed::Bindings(bindings) => driver.bind(bindings, out)?,
                    }
                }
                let seconds = moved.elapsed().as_secs_f32();
                moved = Instant::now();
                driver.step(&input, is_focused, seconds, out)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("service: pad output stopped: {error}");
        }
        let _ = driver.release(out);
        focused.store(false, Ordering::Relaxed);
    })
}

/// The default layout for melee Warcraft III.
pub fn default_bindings() -> Vec<Binding> {
    model::any_map_bindings()
}

#[cfg(test)]
mod contract;

#[cfg(test)]
mod properties {
    use super::*;
    use crate::pad_ingress::{Ingress, Route};
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

    impl crate::pad_ingress::Output for Recorded {
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
        let grid = crate::pad_ingress::CursorGrid { x: 10.0, y: 20.0, width: 127.0, height: 254.0, screen_width: 1920, screen_height: 1080 };
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
                    driver.step(&input, focused, seconds, &mut out).unwrap();
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
}
