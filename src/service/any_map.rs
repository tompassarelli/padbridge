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
mod file;
mod niri;

use super::pointer;
use crate::pad_ingress::{self, Ingress, Route};
use file::FileOutput;
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
impl pad_ingress::Output for AnalogOutput<'_> {
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
    hold: crate::Hold,
    analog: Ingress,
}

impl Driver {
    pub fn new(bindings: Vec<Binding>) -> Self {
        Self { mapper: Mapper::new(bindings), focused: false, menu: None, keys: None, hold: crate::Hold::default(), analog: Ingress::new(Route::Digital) }
    }

    /// The menu pointer: the left stick moves the pointer by `curve`, A and B click.
    pub fn menu(curve: MenuCurve, start_on_keys: bool) -> Self {
        Self { mapper: Mapper::new(menu_bindings(start_on_keys)), focused: false, menu: Some(curve), keys: None, hold: crate::Hold::default(), analog: Ingress::new(Route::Digital) }
    }

    /// The fighter layout on keys. Focus loss releases every key, and nothing presses
    /// again until the pad returns to neutral while focused.
    pub fn keys(preset: model::PadPreset) -> Self {
        Self::keys_with_ingress(preset, Route::Digital)
    }

    pub fn keys_with_ingress(preset: model::PadPreset, route: Route) -> Self {
        Self { mapper: Mapper::new(Vec::new()), focused: false, menu: None, keys: Some(crate::Mapper::new(preset)), hold: crate::Hold::default(), analog: Ingress::new(route) }
    }

    /// The fighter layout on keys with the player's settings.
    pub fn keys_for(settings: &model::ControllerSettings, route: Route) -> Self {
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
    pub fn step(&mut self, input: &InputView, focused: bool, seconds: f32, now: Instant, out: &mut dyn Output) -> Result<(), String> {
        if let Some(keys) = &mut self.keys {
            self.focused = focused;
            let sample = sample_of(input);
            let transitions = keys.update(Some(&sample), focused);
            let transitions = if focused { self.hold.apply(transitions, now) } else { self.hold.release().into_iter().chain(transitions).collect() };
            Self::deliver_keys(transitions, out)?;
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

    pub fn next_release(&self) -> Option<Instant> {
        self.hold.next_due()
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
            Self::deliver_keys(self.hold.release().into_iter().chain(keys.update(None, false)).collect(), out)?;
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
            Mode::Keys(settings) => match Route::from_env() {
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
                let wait = driver.next_release().map_or(TICK, |due| due.saturating_duration_since(Instant::now()).min(TICK));
                match feed.recv_timeout(wait) {
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
                            driver.step(&input, is_focused, 0.0, Instant::now(), out)?;
                        }
                        Feed::Bindings(bindings) => driver.bind(bindings, out)?,
                    }
                }
                let seconds = moved.elapsed().as_secs_f32();
                moved = Instant::now();
                driver.step(&input, is_focused, seconds, Instant::now(), out)?;
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
mod properties;

#[cfg(test)]
mod hold;
