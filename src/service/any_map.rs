//! The Any map profile's delivery: the pad's state through
//! [`model::any_map::Mapper`] into key, click and pointer output, only while
//! Warcraft III's window has focus. Losing focus releases everything held, and
//! nothing is pressed again until the pad returns to neutral. Output goes
//! through XTEST on the game's X11 display (enigo).

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
    fn pointer(&mut self, dx: i32, dy: i32) -> Result<(), String>;
}

/// Turns pad states into output; no I/O of its own.
pub struct Driver {
    mapper: Mapper,
    focused: bool,
}

impl Driver {
    pub fn new(bindings: Vec<Binding>) -> Self {
        Self { mapper: Mapper::new(bindings), focused: false }
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
            let (dx, dy) = self.mapper.pointer(input, seconds);
            if (dx, dy) != (0, 0) {
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

/// XTEST output on an X11 display.
pub struct XOutput(enigo::Enigo);

impl XOutput {
    pub fn new(display: &str) -> Result<Self, String> {
        let settings = enigo::Settings { x11_display: Some(display.to_owned()), release_keys_when_dropped: true, ..enigo::Settings::default() };
        enigo::Enigo::new(&settings).map(Self).map_err(|e| e.to_string())
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

impl Output for XOutput {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        use enigo::Keyboard;
        let key = key_of(name).ok_or_else(|| format!("unknown key {name:?}"))?;
        self.0.key(key, if down { enigo::Direction::Press } else { enigo::Direction::Release }).map_err(|e| e.to_string())
    }

    fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
        use enigo::Mouse;
        let button = if right { enigo::Button::Right } else { enigo::Button::Left };
        self.0.button(button, if down { enigo::Direction::Press } else { enigo::Direction::Release }).map_err(|e| e.to_string())
    }

    fn pointer(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        use enigo::Mouse;
        self.0.move_mouse(dx, dy, enigo::Coordinate::Rel).map_err(|e| e.to_string())
    }
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

/// What the running profile is fed.
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
pub fn spawn(window: Window, bindings: Vec<Binding>, feed: mpsc::Receiver<Feed>, stop: Arc<AtomicBool>, focused: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut out = match XOutput::new(&window.display) {
            Ok(out) => out,
            Err(error) => {
                eprintln!("service: Any map output on {} failed: {error}", window.display);
                return;
            }
        };
        let mut driver = Driver::new(bindings);
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
                        eprintln!("service: Any map {}", if now { "focused" } else { "unfocused: released" });
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
            eprintln!("service: Any map output stopped: {error}");
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
        fn pointer(&mut self, dx: i32, dy: i32) -> Result<(), String> {
            self.0.push(format!("move {dx},{dy}"));
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
