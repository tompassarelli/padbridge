//! The service's local interface: TCP on 127.0.0.1:47631 (the same on every
//! OS), one JSON object per line, in the types of [`crate::model`]. A window
//! that connects gets the current `{"status":...}` at once and then every
//! change, and `{"input":...}` for every change of the pad's state. It may send
//! `{"profile":"auto"|"smashcraft"|"any_map"|"off"}` and `{"bindings":[...]}`.
//! Binding the port is also what makes the service single-instance.

use crate::model::{Button, ClientMessage, InputView, ServiceMessage, Snapshot};
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

pub const ADDRESS: &str = "127.0.0.1:47631";

#[derive(Default)]
struct Shared {
    clients: Vec<TcpStream>,
    status: String,
}

/// The listening interface; cheap to clone into the pad watcher.
#[derive(Clone)]
pub struct Interface {
    shared: Arc<Mutex<Shared>>,
    commands: Arc<Mutex<mpsc::Receiver<ClientMessage>>>,
    pub address: SocketAddr,
}

impl Interface {
    /// Listens on `address`; fails when another service already does.
    pub fn listen(address: &str) -> Result<Self, String> {
        let listener = TcpListener::bind(address).map_err(|error| format!("listen on {address}: {error}"))?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (send, commands) = mpsc::channel();
        let accepting = Arc::clone(&shared);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_nodelay(true);
                // A window that stops reading is dropped rather than stalling the service.
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                let Ok(reader) = stream.try_clone() else { continue };
                let mut shared = accepting.lock().unwrap();
                let mut writer = stream;
                if !shared.status.is_empty() && writer.write_all(shared.status.as_bytes()).is_err() {
                    continue;
                }
                shared.clients.push(writer);
                drop(shared);
                let send = send.clone();
                thread::spawn(move || {
                    for line in BufReader::new(reader).lines().map_while(Result::ok) {
                        match ClientMessage::parse(&line) {
                            Ok(message) => {
                                if send.send(message).is_err() {
                                    break;
                                }
                            }
                            Err(error) => eprintln!("service: ignored a window's message: {error}"),
                        }
                    }
                });
            }
        });
        Ok(Self { shared, commands: Arc::new(Mutex::new(commands)), address: local })
    }

    fn broadcast(shared: &mut Shared, line: &str) {
        shared.clients.retain_mut(|client| client.write_all(line.as_bytes()).is_ok());
    }

    /// Sends the snapshot to every window when it changed.
    pub fn status(&self, snapshot: &Snapshot) {
        let line = ServiceMessage::Status(snapshot.clone()).line();
        let mut shared = self.shared.lock().unwrap();
        if shared.status != line {
            Self::broadcast(&mut shared, &line);
            shared.status = line;
        }
    }

    pub fn input(&self, view: &InputView) {
        let line = ServiceMessage::Input(*view).line();
        Self::broadcast(&mut self.shared.lock().unwrap(), &line);
    }

    /// What windows asked for since the last call.
    pub fn commands(&self) -> Vec<ClientMessage> {
        self.commands.lock().unwrap().try_iter().collect()
    }
}

/// Folds one kernel event into the pad's displayed state. Axis ranges map to
/// SDL's; `positional` pads (Sony) report face buttons by position, Xbox-style
/// drivers by label (smashcraft:companion/README.md, "Xbox mapping").
pub fn apply_event(view: &mut InputView, ranges: &[(i32, i32); 0x12], positional: bool, kind: u16, code: u16, value: i32) {
    const KEY: u16 = 1;
    const ABS: u16 = 3;
    let scale = |code: u16, value: i32| -> i32 {
        let (min, max) = ranges.get(usize::from(code)).copied().unwrap_or((0, 0));
        if max <= min { return 0; }
        (i64::from(value - min) * 65535 / i64::from(max - min) - 32768) as i32
    };
    match kind {
        KEY => {
            let button = match code {
                0x130 => Button::A,
                0x131 => Button::B,
                0x133 => if positional { Button::Y } else { Button::X },
                0x134 => if positional { Button::X } else { Button::Y },
                0x136 => Button::Lb,
                0x137 => Button::Rb,
                0x13a => Button::Back,
                0x13b => Button::Start,
                0x13d => Button::LeftStick,
                0x13e => Button::RightStick,
                _ => return,
            };
            view.press(button, value != 0);
        }
        ABS => {
            let axis = |value: i32| value.clamp(-32768, 32767) as i16;
            let trigger = |value: i32| ((value + 32768) / 2).clamp(0, 32767) as u16;
            match code {
                0x00 => view.left[0] = axis(scale(code, value)),
                0x01 => view.left[1] = axis(scale(code, value)),
                0x03 => view.right[0] = axis(scale(code, value)),
                0x04 => view.right[1] = axis(scale(code, value)),
                0x02 => view.lt = trigger(scale(code, value)),
                0x05 => view.rt = trigger(scale(code, value)),
                0x10 => {
                    view.press(Button::DpadLeft, value < 0);
                    view.press(Button::DpadRight, value > 0);
                }
                0x11 => {
                    view.press(Button::DpadUp, value < 0);
                    view.press(Button::DpadDown, value > 0);
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Streams the pad's state to `send` until it is unplugged; never grabs it,
/// so the helper and the game see the same events.
pub fn watch_pad(device: &Path, mut send: impl FnMut(&InputView)) -> Result<(), String> {
    let mut pad = evdev::Device::open(device).map_err(|e| format!("open {}: {e}", device.display()))?;
    let mut ranges = [(0, 0); 0x12];
    let mut view = InputView::default();
    if let Ok(axes) = pad.get_absinfo() {
        for (code, info) in axes {
            if let Some(range) = ranges.get_mut(usize::from(code.0)) {
                *range = (info.minimum(), info.maximum());
            }
        }
        for (code, info) in pad.get_absinfo().map_err(|e| e.to_string())? {
            apply_event(&mut view, &ranges, false, 3, code.0, info.value());
        }
    }
    let positional = pad.input_id().vendor() == 0x054c;
    send(&view);
    loop {
        let mut changed = false;
        for event in pad.fetch_events().map_err(|e| e.to_string())? {
            let before = view;
            apply_event(&mut view, &ranges, positional, event.event_type().0, event.code(), event.value());
            changed |= view != before;
            // SYN_REPORT closes one consistent state.
            if event.event_type().0 == 0 && event.code() == 0 && changed {
                send(&view);
                changed = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Profile, ProfileChoice};

    #[test]
    fn a_window_gets_the_status_at_once_and_every_change_and_can_choose_a_profile() {
        let interface = Interface::listen("127.0.0.1:0").unwrap();
        // A second service on the same address is refused.
        assert!(Interface::listen(&interface.address.to_string()).is_err());
        let first = Snapshot { profile: Profile::Smashcraft, ..Snapshot::default() };
        interface.status(&first);
        let stream = TcpStream::connect(interface.address).unwrap();
        let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
        assert_eq!(ServiceMessage::parse(&lines.next().unwrap().unwrap()).unwrap(), ServiceMessage::Status(first.clone()));
        let second = Snapshot { profile: Profile::Off, choice: ProfileChoice::Off, ..first };
        interface.status(&second);
        interface.status(&second);
        let mut view = InputView::default();
        view.press(Button::A, true);
        interface.input(&view);
        assert_eq!(ServiceMessage::parse(&lines.next().unwrap().unwrap()).unwrap(), ServiceMessage::Status(second));
        // An unchanged status is not sent again.
        assert_eq!(ServiceMessage::parse(&lines.next().unwrap().unwrap()).unwrap(), ServiceMessage::Input(view));
        (&stream).write_all(ClientMessage::Profile(ProfileChoice::AnyMap).line().as_bytes()).unwrap();
        (&stream).write_all(b"{\"nonsense\":1}\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while got.is_empty() && std::time::Instant::now() < deadline {
            got = interface.commands();
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(got, vec![ClientMessage::Profile(ProfileChoice::AnyMap)]);
    }

    #[test]
    fn pad_events_become_the_displayed_state() {
        let mut ranges = [(0, 0); 0x12];
        for axis in [0, 1, 3, 4] {
            ranges[axis] = (-32768, 32767);
        }
        ranges[2] = (0, 1023);
        ranges[5] = (0, 1023);
        ranges[0x10] = (-1, 1);
        let mut view = InputView::default();
        apply_event(&mut view, &ranges, false, 3, 1, -32768);
        apply_event(&mut view, &ranges, false, 3, 5, 1023);
        apply_event(&mut view, &ranges, false, 1, 0x133, 1);
        apply_event(&mut view, &ranges, false, 3, 0x10, -1);
        assert_eq!(view.left, [0, -32768]);
        assert_eq!(view.rt, 32767);
        assert!(view.pressed(Button::X) && view.pressed(Button::DpadLeft));
        apply_event(&mut view, &ranges, false, 1, 0x133, 0);
        apply_event(&mut view, &ranges, true, 1, 0x133, 1);
        assert!(!view.pressed(Button::X) && view.pressed(Button::Y));
        apply_event(&mut view, &ranges, false, 3, 2, 0);
        assert_eq!(view.lt, 0);
    }
}
