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
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

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
            let key = match transition.action.key() { ' ' => "space".to_owned(), key => key.to_string() };
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

/// Keys through XTEST on an X11 display; the pointer and clicks through the
/// compositor's virtual pointer.
pub struct DesktopOutput {
    keys: enigo::Enigo,
    pointer: super::pointer::VirtualPointer,
}

impl DesktopOutput {
    pub fn new(display: &str) -> Result<Self, String> {
        let settings = enigo::Settings { x11_display: Some(display.to_owned()), release_keys_when_dropped: true, ..enigo::Settings::default() };
        Ok(Self {
            keys: enigo::Enigo::new(&settings).map_err(|e| e.to_string())?,
            pointer: super::pointer::VirtualPointer::new()?,
        })
    }
}

/// The enigo key for a binding's key name (model::Press::Key).
pub fn key_of(name: &str) -> Option<enigo::Key> {
    use enigo::Key;
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return c.is_ascii_alphanumeric().then(|| Key::Unicode(c.to_ascii_lowercase()));
    }
    Some(match name {
        "space" => Key::Space,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        "enter" => Key::Return,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "f1" => Key::F1, "f2" => Key::F2, "f3" => Key::F3, "f4" => Key::F4,
        "f5" => Key::F5, "f6" => Key::F6, "f7" => Key::F7, "f8" => Key::F8,
        "f9" => Key::F9, "f10" => Key::F10, "f11" => Key::F11, "f12" => Key::F12,
        "f13" => Key::F13, "f14" => Key::F14, "f15" => Key::F15, "f16" => Key::F16,
        "f17" => Key::F17, "f18" => Key::F18, "f19" => Key::F19, "f20" => Key::F20,
        "f21" => Key::F21, "f22" => Key::F22, "f23" => Key::F23, "f24" => Key::F24,
        "insert" => Key::Insert, "delete" => Key::Delete, "end" => Key::End, "home" => Key::Home,
        "pageup" => Key::PageUp, "pagedown" => Key::PageDown,
        _ => return None,
    })
}

impl Output for DesktopOutput {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        use enigo::Keyboard;
        let key = key_of(name).ok_or_else(|| format!("unknown key {name:?}"))?;
        self.keys.key(key, if down { enigo::Direction::Press } else { enigo::Direction::Release }).map_err(|e| e.to_string())
    }

    fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
        self.pointer.button(right, down)
    }

    fn pointer(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        self.pointer.motion(dx, dy)
    }

    fn cursor(&mut self, x: f64, y: f64, width: u32, height: u32) -> Result<(), String> {
        self.pointer.absolute(x, y, width, height)
    }
}

impl crate::pad_ingress::Output for DesktopOutput {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> { Output::key(self, name, down) }
    fn cursor(&mut self, x: f64, y: f64, width: u32, height: u32) -> Result<(), String> { Output::cursor(self, x, y, width, height) }
}

/// The game window's width in logical pixels, as niri lays it out.
pub fn niri_window_width(socket: &Path, window: u64) -> Option<f64> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(200))).ok()?;
    writeln!(stream, "\"Windows\"").ok()?;
    stream.shutdown(std::net::Shutdown::Write).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    let parsed: Value = serde_json::from_str(&reply).ok()?;
    parsed.pointer("/Ok/Windows")?.as_array()?.iter()
        .find(|entry| entry.get("id").and_then(Value::as_u64) == Some(window))?
        .pointer("/layout/window_size/0")?.as_f64()
}

/// Whether niri's focused window is `window`; any failure reads as not focused.
pub fn niri_focused(socket: &Path, window: u64) -> bool {
    let ask = || -> Result<bool, String> {
        let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(Duration::from_millis(50))).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(Duration::from_millis(50))).map_err(|e| e.to_string())?;
        writeln!(stream, "\"FocusedWindow\"").map_err(|e| e.to_string())?;
        stream.shutdown(std::net::Shutdown::Write).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
        let parsed: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
        Ok(parsed.pointer("/Ok/FocusedWindow/id").and_then(Value::as_u64) == Some(window))
    };
    ask().unwrap_or(false)
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
mod contract {
    use super::*;
    use model::{Button, Control, Move, PadKind, PadPreset, Press, TriggerShield, TriggerShields};
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(serde::Deserialize)]
    struct Contract {
        /// The model's action label to the Smashcraft action its key must reach.
        map_actions: BTreeMap<String, String>,
    }

    // Smashcraft's Action order and key saves: smashcraft:ts/src/game/input/actions.ts and keyBindings.ts
    // (presetBindings("standard"), decodeBindings, upgradeDefaults); Y starts the match through its own trigger in shell.ts.
    const ACTIONS: [&str; 18] = ["moveLeft", "moveRight", "moveDown", "moveUp", "jump", "attack", "special", "grab", "leftTrigger",
        "rightTrigger", "smashLeft", "smashRight", "smashUp", "smashDown", "walk", "lightShield", "shortHop", "meter"];

    fn code(key: &str) -> u8 {
        if key == "space" { 32 } else { key.to_ascii_uppercase().as_bytes()[0] }
    }

    fn standard() -> Vec<[u8; 2]> {
        vec![[b'W', 0], [b'R', 0], [b'E', 0], [32, 0], [b'Z', b'8'], [b'N', 0], [b'U', 0], [b'O', 0], [b'Q', 0], [b'7', b'G'],
            [b'B', 191], [b'M', 0], [b'J', 0], [b'H', 0], [b'P', 0], [b'A', 0], [b'I', 0], [b'X', 0]]
    }

    fn saved(file: &str) -> Vec<[u8; 2]> {
        let start = file.find("\"K").unwrap() + 1;
        let version = &file[start..start + 2];
        let digits: String = file[start + 2..].chars().take_while(char::is_ascii_digit).collect();
        let mut keys = vec![[0u8; 2]; ACTIONS.len()];
        for (index, chunk) in digits.as_bytes().chunks(3).enumerate() {
            let key: u8 = std::str::from_utf8(chunk).unwrap().parse().unwrap();
            if key != b'Y' { keys[index / 2][index % 2] = key; }
        }
        let action = |name: &str| ACTIONS.iter().position(|a| *a == name).unwrap();
        let free = |keys: &[[u8; 2]], candidates: &[u8]| candidates.iter().all(|c| !keys.iter().flatten().any(|k| k == c));
        if keys[action("lightShield")][1] == 0 && free(&keys, b"T") { keys[action("lightShield")][1] = b'T'; }
        if keys[action("meter")][0] == 0 && free(&keys, b"X") { keys[action("meter")][0] = b'X'; }
        if keys[action("rightTrigger")][1] == 0 && free(&keys, b"G") { keys[action("rightTrigger")][1] = b'G'; }
        if keys[action("shortHop")][0] == 0 && free(&keys, b"Z") { keys[action("shortHop")][0] = b'Z'; }
        if version == "K1" && free(&keys, b"P") { keys[action("walk")][0] = b'P'; }
        if keys[action("grab")][0] == b'L' && free(&keys, b"O") { keys[action("grab")][0] = b'O'; }
        if keys[action("walk")][0] == 186 && free(&keys, b"P") { keys[action("walk")][0] = b'P'; }
        if keys[action("moveLeft")][0] == b'S' && keys[action("moveRight")][0] == b'F' && keys[action("moveDown")][0] == b'D'
            && keys[action("leftTrigger")][0] == b'A' && free(&keys, b"QWER") {
            for (name, key) in [("moveLeft", b'W'), ("moveRight", b'R'), ("moveDown", b'E'), ("leftTrigger", b'Q')] { keys[action(name)][0] = key; }
        }
        if keys[action("jump")][0] == b'I' && keys[action("shortHop")][0] == b'Z' {
            keys[action("jump")][0] = b'Z';
            keys[action("shortHop")][0] = b'I';
        }
        if [0, b'9', b'0'].contains(&keys[action("lightShield")][0]) && keys[action("lightShield")][1] == b'T' && free(&keys, b"A") {
            keys[action("lightShield")] = [b'A', 0];
        }
        keys
    }

    fn bound(keys: &[[u8; 2]], key: &str) -> Option<&'static str> {
        if key == "y" { return Some("start"); }
        keys.iter().position(|slots| slots.contains(&code(key))).map(|index| ACTIONS[index])
    }

    fn key(press: &Press) -> Option<String> {
        match press {
            Press::Key(name) => Some(name.clone()),
            Press::LeftClick => Some("left-click".into()),
            Press::RightClick => Some("right-click".into()),
            Press::Pointer => None,
        }
    }

    const PRESETS: [PadPreset; 3] = PadPreset::ALL;
    const SHIELDS: [TriggerShield; 2] = [TriggerShield::Full, TriggerShield::Light];

    #[test]
    fn every_preset_key_reaches_its_labelled_action_in_the_maps_defaults_and_toms_saved_bindings() {
        let contract: Contract = serde_json::from_str(include_str!("../../contract/bindings.json")).unwrap();
        let sets = [("the map's standard keys", standard()), ("tom-saved-bindings.pld", saved(include_str!("../../contract/tom-saved-bindings.pld")))];
        for (set, keys) in &sets {
            for preset in PRESETS {
                for (left, right) in SHIELDS.into_iter().flat_map(|left| SHIELDS.map(|right| (left, right))) {
                    for binding in model::RightStick::ALL.into_iter().flat_map(|mode| model::fighter_bindings_with(preset, TriggerShields { left, right }, mode)) {
                        let Press::Key(sent) = &binding.press else { continue };
                        let wanted = contract.map_actions.get(&binding.action)
                            .unwrap_or_else(|| panic!("{} {:?} is labelled {:?}, which the contract leaves out", preset.name(), binding.control, binding.action));
                        let reached = bound(keys, sent);
                        let ok = match wanted.as_str() { "shield" => matches!(reached, Some("leftTrigger" | "rightTrigger")), action => reached == Some(action) };
                        assert!(ok, "{} {:?} {:?} sends {sent:?}, which {set} binds to {reached:?}", preset.name(), binding.control, binding.action);
                    }
                }
            }
        }
    }

    #[derive(Default)]
    struct Recorded {
        held: BTreeSet<String>,
        moved: bool,
    }

    impl Output for Recorded {
        fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
            self.press(name.to_owned(), down);
            Ok(())
        }
        fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
            self.press(if right { "right-click" } else { "left-click" }.to_owned(), down);
            Ok(())
        }
        fn pointer(&mut self, _: f64, _: f64) -> Result<(), String> {
            self.moved = true;
            Ok(())
        }
    }

    impl Recorded {
        fn press(&mut self, name: String, down: bool) {
            if down {
                assert!(self.held.insert(name.clone()), "{name} pressed twice");
            } else {
                assert!(self.held.remove(&name), "{name} released while up");
            }
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    const BUTTONS: [(Control, Button); 9] = [
        (Control::A, Button::A), (Control::B, Button::B), (Control::X, Button::X), (Control::Y, Button::Y), (Control::Lb, Button::Lb),
        (Control::Rb, Button::Rb), (Control::Start, Button::Start), (Control::Back, Button::Back), (Control::LeftStick, Button::LeftStick),
    ];
    const LEFT: [Control; 4] = [Control::LeftLeft, Control::LeftRight, Control::LeftUp, Control::LeftDown];
    const RIGHT: [Control; 4] = [Control::RightLeft, Control::RightRight, Control::RightUp, Control::RightDown];

    /// A pad with some controls clearly held: buttons, triggers past their threshold, and each stick neutral or tilted to one side.
    fn generate(rng: &mut Rng) -> (InputView, BTreeSet<Control>) {
        let mut input = InputView::default();
        let mut held = BTreeSet::new();
        for (control, button) in BUTTONS {
            if rng.below(4) == 0 {
                input.press(button, true);
                held.insert(control);
            }
        }
        for (control, value) in [(Control::Lt, &mut input.lt), (Control::Rt, &mut input.rt)] {
            *value = if rng.below(3) == 0 { held.insert(control); 4001 + rng.below(28767) as u16 } else { rng.below(4001) as u16 };
        }
        for (directions, stick) in [(LEFT, &mut input.left), (RIGHT, &mut input.right)] {
            let pick = rng.below(6) as usize;
            let Some(&direction) = directions.get(pick) else { continue };
            let tilt = 29491 + rng.below(3277) as i16;
            *stick = match pick { 0 => [-tilt, 0], 1 => [tilt, 0], 2 => [0, -tilt], _ => [0, tilt] };
            held.insert(direction);
        }
        (input, held)
    }

    #[test]
    fn the_service_presses_the_models_bindings_on_every_screen() {
        let mut rng = Rng(std::env::var("PROPERTY_SEED").ok().and_then(|seed| seed.parse().ok()).unwrap_or(0xc0ffee));
        for preset in PRESETS {
            let triggers = TriggerShields { left: SHIELDS[rng.below(2) as usize], right: SHIELDS[rng.below(2) as usize] };
            let right_stick = model::RightStick::ALL[rng.below(2) as usize];
            let settings = model::ControllerSettings { pad_preset: preset, triggers, right_stick, ..Default::default() };
            let tilt = model::remap(Vec::new(), &[(Control::Lb, Move::Tilt)].into(), triggers).remove(0).press;
            let fight = settings.bindings(PadKind::Xbox);
            let menus = menu_bindings(true);
            for (screen, table) in [("match", &fight), ("menus", &menus)] {
                let mut driver = match screen {
                    "match" => Driver::keys_for(&settings, crate::pad_ingress::Route::Digital),
                    _ => Driver::menu(MenuCurve::for_window_width(2560.0), true),
                };
                let mut out = Recorded::default();
                driver.step(&InputView::default(), true, 0.0, &mut out).unwrap();
                for _ in 0..400 {
                    let (input, controls) = generate(&mut rng);
                    out.moved = false;
                    driver.step(&input, true, 0.01, &mut out).unwrap();
                    let mut presses: Vec<&Press> = table.iter().filter(|b| controls.contains(&b.control)).map(|b| &b.press).collect();
                    if screen == "match" && right_stick == model::RightStick::Tilt && controls.iter().any(|c| RIGHT.contains(c)) {
                        presses.push(&tilt);
                    }
                    let keys: BTreeSet<String> = presses.iter().filter_map(|p| key(p)).collect();
                    let pointer = screen == "menus" && controls.iter().any(|c| LEFT.contains(c));
                    assert_eq!(out.held, keys, "{} {screen}: holding {controls:?}", preset.name());
                    assert_eq!(out.moved, pointer, "{} {screen}: pointer while holding {controls:?}", preset.name());
                    driver.step(&InputView::default(), true, 0.0, &mut out).unwrap();
                    assert!(out.held.is_empty(), "{} {screen}: {:?} still held at neutral", preset.name(), out.held);
                }
            }
        }
        let shields: Vec<_> = model::fighter_bindings_with(PadPreset::Tom, TriggerShields::default(), model::RightStick::default()).into_iter()
            .filter(|b| matches!(b.control, Control::Lt | Control::Rt)).map(|b| b.action).collect();
        assert_eq!(shields, ["Shield", "Right shield"], "LT and RT both shield");
        for preset in [PadPreset::Melee, PadPreset::ZJump] {
            let settings = model::ControllerSettings { pad_preset: preset, ..Default::default() };
            let mut driver = Driver::keys_for(&settings, crate::pad_ingress::Route::Digital);
            let mut out = Recorded::default();
            for input in [InputView::default(), InputView { lt: 20000, ..Default::default() }, InputView { lt: 20000, rt: 20000, ..Default::default() }] {
                driver.step(&input, true, 0.0, &mut out).unwrap();
            }
            assert_eq!(out.held, ["g", "q"].map(String::from).into(), "{}: RT pressed while LT is held", preset.name());
        }
        assert!(menu_bindings(true).iter().any(|b| b.control == Control::Start && b.press == Press::Key("y".into())), "Start presses Y in menus");
    }
}

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
            let key = match t.action.key() { ' ' => "space".to_owned(), key => key.to_string() };
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
