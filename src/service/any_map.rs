//! Pad-driven desktop output: the Any map profile (the pad's state through
//! [`model::any_map::Mapper`] into keys, clicks and pointer motion) and the
//! Smashcraft menu pointer (the left stick moves the pointer with a Smash-like
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
/// Full-tilt speed in game-window heights a second. The fighter grid is 0.8
/// of the window's height wide (selectionGrid.ts: 0.48 of the 0.6-high UI), so
/// full tilt crosses it in 0.8 / 1.15 = 0.7 s.
pub const MENU_HEIGHTS_PER_SECOND: f64 = 1.15;

impl MenuCurve {
    pub fn for_window_height(height: f64) -> Self {
        Self { full_speed: height * MENU_HEIGHTS_PER_SECOND }
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

/// The menu pointer's clicks (model::smashcraft_menu_bindings); the stick is
/// the curve's, and the helper keeps Start.
pub fn menu_bindings() -> Vec<Binding> {
    model::smashcraft_menu_bindings().into_iter()
        .filter(|binding| matches!(binding.press, model::Press::LeftClick | model::Press::RightClick))
        .collect()
}

/// Turns pad states into output; no I/O of its own.
pub struct Driver {
    mapper: Mapper,
    focused: bool,
    menu: Option<MenuCurve>,
}

impl Driver {
    pub fn new(bindings: Vec<Binding>) -> Self {
        Self { mapper: Mapper::new(bindings), focused: false, menu: None }
    }

    /// The menu pointer: the left stick moves the pointer by `curve`, A and B click.
    pub fn menu(curve: MenuCurve) -> Self {
        Self { mapper: Mapper::new(menu_bindings()), focused: false, menu: Some(curve) }
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
        Self::deliver(self.mapper.release_all(), out)?;
        self.mapper = Mapper::new(bindings);
        Ok(())
    }

    pub fn release(&mut self, out: &mut dyn Output) -> Result<(), String> {
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
}

/// The game window's height in logical pixels, as niri lays it out.
pub fn niri_window_height(socket: &Path, window: u64) -> Option<f64> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(200))).ok()?;
    writeln!(stream, "\"Windows\"").ok()?;
    stream.shutdown(std::net::Shutdown::Write).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    let parsed: Value = serde_json::from_str(&reply).ok()?;
    parsed.pointer("/Ok/Windows")?.as_array()?.iter()
        .find(|entry| entry.get("id").and_then(Value::as_u64) == Some(window))?
        .pointer("/layout/window_size/1")?.as_f64()
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
    /// Smashcraft's menus: pointer and clicks only.
    Menu,
}

/// What the running output is fed.
pub enum Feed {
    Input(InputView),
    Bindings(Vec<Binding>),
}

/// The game window the output serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub display: String,
    pub niri_socket: PathBuf,
    pub niri_window: u64,
}

/// How often the pointer moves while a stick holds it, and focus is checked.
const TICK: Duration = Duration::from_millis(10);
const FOCUS_EVERY: Duration = Duration::from_millis(50);

/// Runs the profile for `window` until `stop` or the feed ends, releasing what
/// it holds at the end; `focused` mirrors the game window's focus.
pub fn spawn(window: Window, mode: Mode, feed: mpsc::Receiver<Feed>, stop: Arc<AtomicBool>, focused: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut out = match DesktopOutput::new(&window.display) {
            Ok(out) => out,
            Err(error) => {
                eprintln!("service: pad output on {} failed: {error}", window.display);
                return;
            }
        };
        let mut driver = match mode {
            Mode::AnyMap(bindings) => Driver::new(bindings),
            Mode::Menu => {
                let height = niri_window_height(&window.niri_socket, window.niri_window).unwrap_or(1440.0);
                Driver::menu(MenuCurve::for_window_height(height))
            }
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
                    let now = niri_focused(&window.niri_socket, window.niri_window);
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
                            driver.step(&input, is_focused, 0.0, &mut out)?;
                        }
                        Feed::Bindings(bindings) => driver.bind(bindings, &mut out)?,
                    }
                }
                let seconds = moved.elapsed().as_secs_f32();
                moved = Instant::now();
                driver.step(&input, is_focused, seconds, &mut out)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("service: pad output stopped: {error}");
        }
        let _ = driver.release(&mut out);
        focused.store(false, Ordering::Relaxed);
    })
}

/// The default layout for melee Warcraft III.
pub fn default_bindings() -> Vec<Binding> {
    model::any_map_bindings()
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Button, Control, Press};

    /// A fake output device: records what reaches it.
    #[derive(Default)]
    struct Recorded(Vec<String>);

    impl Output for Recorded {
        fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
            self.0.push(format!("{} {name}", if down { "down" } else { "up" }));
            Ok(())
        }
        fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
            self.0.push(format!("{} {}", if down { "down" } else { "up" }, if right { "right-click" } else { "left-click" }));
            Ok(())
        }
        fn pointer(&mut self, dx: f64, dy: f64) -> Result<(), String> {
            self.0.push(format!("move {dx:.0},{dy:.0}"));
            Ok(())
        }
    }

    fn take(out: &mut Recorded) -> Vec<String> {
        std::mem::take(&mut out.0)
    }

    #[test]
    fn the_default_layout_plays_melee_warcraft() {
        let (mut driver, mut out) = (Driver::new(default_bindings()), Recorded::default());
        let mut pad = InputView::default();
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        pad.press(Button::A, true);
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        pad.press(Button::A, false);
        pad.press(Button::B, true);
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        assert_eq!(take(&mut out), ["down left-click", "up left-click", "down right-click"]);
        pad.press(Button::B, false);
        pad.press(Button::Lb, true);
        pad.left = [0, -32767];
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        assert_eq!(take(&mut out), ["up right-click", "down 1", "down up"]);
        // The right stick moves the pointer; the left stick never does.
        pad.right = [32767, 0];
        driver.step(&pad, true, 0.01, &mut out).unwrap();
        assert_eq!(take(&mut out), ["move 14,0"]);
    }

    #[test]
    fn focus_loss_releases_everything_and_needs_neutral_again() {
        let (mut driver, mut out) = (Driver::new(default_bindings()), Recorded::default());
        let mut pad = InputView::default();
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        pad.press(Button::Rb, true);
        pad.right = [32767, 0];
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        assert_eq!(take(&mut out), ["down 2"]);
        driver.step(&pad, false, 0.01, &mut out).unwrap();
        assert_eq!(take(&mut out), ["up 2"]);
        // Unfocused: nothing at all, the pointer included.
        driver.step(&pad, false, 0.01, &mut out).unwrap();
        assert!(take(&mut out).is_empty());
        // Back in focus with the button still held: it isn't replayed.
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        assert!(take(&mut out).is_empty());
        pad = InputView::default();
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        pad.press(Button::Rb, true);
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        assert_eq!(take(&mut out), ["down 2"]);
    }

    #[test]
    fn new_bindings_release_the_old_ones_first() {
        let (mut driver, mut out) = (Driver::new(default_bindings()), Recorded::default());
        let mut pad = InputView::default();
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        pad.press(Button::X, true);
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        driver.bind(vec![Binding { control: Control::X, action: "Hold".into(), press: Press::Key("h".into()) }], &mut out).unwrap();
        assert_eq!(take(&mut out), ["down q", "up q"]);
        pad = InputView::default();
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        pad.press(Button::X, true);
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        assert_eq!(take(&mut out), ["down h"]);
    }

    #[test]
    fn in_smashcraft_menus_the_left_stick_moves_the_pointer_by_deflection_and_a_clicks() {
        let curve = MenuCurve::for_window_height(1440.0);
        let (mut driver, mut out) = (Driver::menu(curve), Recorded::default());
        let mut pad = InputView::default();
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        // Inside the small deadzone: still.
        pad.left = [3900, 0];
        driver.step(&pad, true, 0.1, &mut out).unwrap();
        assert!(take(&mut out).is_empty());
        // Half tilt is well under half speed; full tilt crosses the grid
        // (0.8 of the height) in about 0.7 s.
        pad.left = [16384, 0];
        driver.step(&pad, true, 0.1, &mut out).unwrap();
        pad.left = [32767, 0];
        driver.step(&pad, true, 0.1, &mut out).unwrap();
        assert_eq!(take(&mut out), ["move 40,0", "move 166,0"]);
        let crossing = 0.8 * 1440.0 / curve.velocity([32767, 0]).0;
        assert!((0.6..=0.8).contains(&crossing), "{crossing}");
        // Diagonals keep their direction; up is negative y.
        let (vx, vy) = curve.velocity([-23170, -23170]);
        assert!(vx < 0.0 && (vx - vy).abs() < 1e-9);
        // A clicks; B is the right button; the face buttons press no keys.
        pad.left = [0, 0];
        pad.press(Button::A, true);
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        pad.press(Button::A, false);
        pad.press(Button::B, true);
        pad.press(Button::X, true);
        driver.step(&pad, true, 0.0, &mut out).unwrap();
        assert_eq!(take(&mut out), ["down left-click", "up left-click", "down right-click"]);
        // Unfocused, nothing moves.
        pad.left = [32767, 0];
        driver.step(&pad, false, 0.1, &mut out).unwrap();
        assert_eq!(take(&mut out), ["up right-click"]);
    }

    #[test]
    fn every_default_key_has_an_output_key() {
        for binding in default_bindings() {
            if let Press::Key(name) = &binding.press {
                assert!(key_of(name).is_some(), "{name}");
            }
        }
        assert_eq!(key_of("Q"), Some(enigo::Key::Unicode('q')));
        assert_eq!(key_of("f10"), Some(enigo::Key::F10));
        assert_eq!(key_of("shift"), None);
    }
}
