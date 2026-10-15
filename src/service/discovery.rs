use super::profile::{Game, Pad, Target};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, Window},
};

/// Keep held output while a controller briefly disappears during rediscovery.
const PAD_REDISCOVERY: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct PadDiscovery {
    pad: Option<Pad>,
    missing_since: Option<Instant>,
}

impl PadDiscovery {
    pub(super) fn update(&mut self, found: Option<Pad>, now: Instant) -> Option<Pad> {
        if let Some(pad) = found {
            self.pad = Some(pad);
            self.missing_since = None;
        } else if self.pad.is_some() {
            let since = *self.missing_since.get_or_insert(now);
            if now.duration_since(since) >= PAD_REDISCOVERY {
                self.pad = None;
            }
        }
        self.pad.clone()
    }
}

/// The newest Warcraft III process on `display`, with its window.
pub fn find_game(display: &str) -> Option<Game> {
    let (pid, birth, prefix) = warcraft_processes(Path::new("/proc"), display).into_iter().max_by_key(|(_, birth, _)| *birth)?;
    let documents = documents(&prefix)?;
    let window = x11_window(display, pid).ok().flatten()?;
    let niri_socket = niri_socket()?;
    let niri_window = niri_window(&niri_socket, pid, &window.1, &window.2).ok().flatten()?;
    Some(Game { pid, birth, documents, target: Target::Window { display: display.into(), x11_window: window.0, niri_window, niri_socket } })
}

/// Warcraft III.exe processes whose DISPLAY is `display`: PID, start ticks and Wine prefix.
pub fn warcraft_processes(proc_root: &Path, display: &str) -> Vec<(u32, u64, PathBuf)> {
    let Ok(entries) = fs::read_dir(proc_root) else { return Vec::new() };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        // The short name ("Warcraft III.ex") rules out every other process cheaply.
        if !fs::read_to_string(entry.path().join("comm")).is_ok_and(|comm| comm.to_ascii_lowercase().starts_with("warcraft iii")) {
            continue;
        }
        let Ok(command) = fs::read(entry.path().join("cmdline")) else { continue };
        let is_game = command.split(|b| *b == 0).any(|arg| {
            String::from_utf8_lossy(arg).rsplit(['/', '\\']).next().is_some_and(|name| name.eq_ignore_ascii_case("Warcraft III.exe"))
        });
        if !is_game {
            continue;
        }
        let Ok(environ) = fs::read(entry.path().join("environ")) else { continue };
        let variable = |key: &str| {
            environ.split(|b| *b == 0).find_map(|pair| {
                let pair = String::from_utf8_lossy(pair);
                pair.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')).map(str::to_owned)
            })
        };
        if variable("DISPLAY").as_deref() != Some(display) {
            continue;
        }
        let Some(prefix) = variable("WINEPREFIX") else { continue };
        let Some(birth) = process_birth(&entry.path()) else { continue };
        found.push((pid, birth, PathBuf::from(prefix)));
    }
    found
}

pub(super) fn process_birth(process: &Path) -> Option<u64> {
    let stat = fs::read_to_string(process.join("stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

/// Documents/Warcraft III under the prefix's Wine user (Proton's is steamuser).
pub fn documents(prefix: &Path) -> Option<PathBuf> {
    let users = prefix.join("drive_c/users");
    let preferred = users.join("steamuser/Documents/Warcraft III");
    if preferred.is_dir() {
        return Some(preferred);
    }
    let mut found: Vec<PathBuf> = fs::read_dir(&users).ok()?.flatten()
        .map(|user| user.path().join("Documents/Warcraft III"))
        .filter(|path| path.is_dir())
        .collect();
    found.sort();
    found.into_iter().next()
}

/// The game's top-level X11 window: its id, title and class. Prefers one
/// titled "Warcraft III", then the largest.
fn x11_window(display: &str, pid: u32) -> Result<Option<(u32, String, String)>, String> {
    let (x11, screen) = x11rb::connect(Some(display)).map_err(|e| e.to_string())?;
    let root = x11.setup().roots[screen].root;
    let atom = |name: &[u8]| -> Result<u32, String> {
        Ok(x11.intern_atom(false, name).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.atom)
    };
    let (client_list, pid_atom, name_atom, class_atom) = (atom(b"_NET_CLIENT_LIST")?, atom(b"_NET_WM_PID")?, atom(b"_NET_WM_NAME")?, u32::from(AtomEnum::WM_CLASS));
    let mut windows: Vec<Window> = x11.get_property(false, root, client_list, AtomEnum::WINDOW, 0, 4096)
        .map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?
        .value32().map(Iterator::collect).unwrap_or_default();
    if windows.is_empty() {
        windows = x11.query_tree(root).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.children;
    }
    let text = |window: Window, property: u32| -> String {
        x11.get_property(false, window, property, AtomEnum::ANY, 0, 1024).ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
            .unwrap_or_default()
    };
    let mut best: Option<(bool, u32, (u32, String, String))> = None;
    for window in windows {
        let Ok(reply) = x11.get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1).map_err(|e| e.to_string())?.reply() else { continue };
        if reply.value32().and_then(|mut values| values.next()) != Some(pid) {
            continue;
        }
        let Ok(geometry) = x11.get_geometry(window).map_err(|e| e.to_string())?.reply() else { continue };
        let title = text(window, name_atom);
        // WM_CLASS is "instance\0class\0"; niri's app_id is the class.
        let class = text(window, class_atom).split('\0').nth(1).unwrap_or_default().to_owned();
        let rank = (title == "Warcraft III", u32::from(geometry.width) * u32::from(geometry.height));
        if best.as_ref().is_none_or(|(titled, area, _)| rank > (*titled, *area)) {
            best = Some((rank.0, rank.1, (window, title, class)));
        }
    }
    Ok(best.map(|(_, _, window)| window))
}

/// The compositor's IPC socket: NIRI_SOCKET, else the newest niri socket in XDG_RUNTIME_DIR.
pub fn niri_socket() -> Option<PathBuf> {
    if let Some(socket) = std::env::var_os("NIRI_SOCKET").map(PathBuf::from).filter(|path| path.exists()) {
        return Some(socket);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    fs::read_dir(runtime).ok()?.flatten()
        .filter(|entry| entry.file_name().to_str().is_some_and(|name| name.starts_with("niri.") && name.ends_with(".sock")))
        .max_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok())
        .map(|entry| entry.path())
}

fn niri_window(socket: &Path, pid: u32, title: &str, class: &str) -> Result<Option<u64>, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).map_err(|e| e.to_string())?;
    writeln!(stream, "\"Windows\"").map_err(|e| e.to_string())?;
    stream.shutdown(std::net::Shutdown::Write).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    let parsed: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    Ok(niri_game_window(parsed.get("Ok").and_then(|ok| ok.get("Windows")).unwrap_or(&Value::Null), pid, title, class))
}

/// The one niri window that is the game: its own PID, or (through
/// xwayland-satellite) the game window's exact title and class.
pub fn niri_game_window(windows: &Value, pid: u32, title: &str, class: &str) -> Option<u64> {
    let mut matching = windows.as_array()?.iter().filter(|window| {
        window.get("pid").and_then(Value::as_u64) == Some(u64::from(pid))
            || (!title.is_empty()
                && window.get("title").and_then(Value::as_str) == Some(title)
                && window.get("app_id").and_then(Value::as_str) == Some(class))
    });
    let window = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    window.get("id").and_then(Value::as_u64)
}

/// The controller among `dir`'s joystick links, then its event nodes (Bluetooth
/// need not have a by-id link). Prefers an Xbox pad.
pub fn find_pad(dir: &Path) -> Option<Pad> {
    let mut links: Vec<(bool, String)> = fs::read_dir(dir).ok().into_iter().flatten().flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with("-event-joystick"))
        .map(|name| {
            let lower = name.to_ascii_lowercase();
            (!(lower.contains("microsoft") || lower.contains("xbox")), name)
        })
        .collect();
    links.sort();
    if let Some(pad) = links.into_iter().find_map(|(_, name)| {
        let link = dir.join(&name);
        let device = fs::canonicalize(&link).ok()?;
        let node = device.file_name()?.to_str()?.to_owned();
        let pad_name = fs::read_to_string(format!("/sys/class/input/{node}/device/name"))
            .map(|name| name.trim().to_owned())
            .unwrap_or(name);
        Some(Pad { link, device, name: pad_name })
    }) {
        return Some(pad);
    }
    let events = if dir.file_name().is_some_and(|name| name == "by-id") { dir.parent()? } else { dir };
    let mut pads: Vec<(bool, Pad)> = fs::read_dir(events).ok()?.flatten().filter_map(|entry| {
        let name = entry.file_name();
        let number = name.to_str()?.strip_prefix("event")?;
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) { return None; }
        let device = fs::canonicalize(entry.path()).ok()?;
        let pad = evdev::Device::open(&device).ok()?;
        if !pad.supported_keys().is_some_and(|keys| keys.contains(evdev::KeyCode::BTN_SOUTH))
            || !pad.supported_absolute_axes().is_some_and(|axes| axes.contains(evdev::AbsoluteAxisCode::ABS_X) && axes.contains(evdev::AbsoluteAxisCode::ABS_Y)) {
            return None;
        }
        let name = pad.name().unwrap_or("Controller").to_owned();
        let lower = name.to_ascii_lowercase();
        let xbox = pad.input_id().vendor() == 0x045e || lower.contains("microsoft") || lower.contains("xbox");
        Some((!xbox, Pad { link: entry.path(), device, name }))
    }).collect();
    pads.sort_by(|(a, first), (b, second)| (a, &first.link).cmp(&(b, &second.link)));
    pads.into_iter().next().map(|(_, pad)| pad)
}

pub(super) fn headless_game(documents: &Path) -> Option<Game> {
    let name = fs::read_to_string(documents.join("game")).ok()?;
    let birth = name.trim().parse().ok()?;
    Some(Game { pid: 0, birth, documents: documents.into(), target: Target::Headless { text_out: documents.join("typed.txt") } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model, rng::{Rng, cases}, service::interface};
    use std::{sync::mpsc, thread};

    #[test]
    fn bluetooth_event_without_by_id_is_found_and_reopened_with_held_buttons() {
        use evdev::{AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, BusType, InputId, InputEvent, KeyCode, UinputAbsSetup, uinput::VirtualDevice};
        if fs::OpenOptions::new().write(true).open("/dev/uinput").is_err() {
            eprintln!("skipped: /dev/uinput is not writable, so no virtual Bluetooth pad");
            return;
        }
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::BTN_SOUTH);
        let mut virtual_pad = VirtualDevice::builder().unwrap().name("Xbox Bluetooth service test")
            .input_id(InputId::new(BusType::BUS_BLUETOOTH, 0x045e, 0x0b13, 1))
            .with_keys(&keys).unwrap()
            .with_absolute_axis(&UinputAbsSetup::new(Abs::ABS_X, AbsInfo::new(0, -32768, 32767, 0, 0, 0))).unwrap()
            .with_absolute_axis(&UinputAbsSetup::new(Abs::ABS_Y, AbsInfo::new(0, -32768, 32767, 0, 0, 0))).unwrap()
            .build().unwrap();
        let node = virtual_pad.enumerate_dev_nodes_blocking().unwrap().find_map(Result::ok).unwrap();
        let dir = std::env::temp_dir().join(format!("service-bluetooth-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&node, dir.join("event9")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let found = loop {
            if let Some(pad) = find_pad(&dir.join("by-id")) { break pad; }
            assert!(Instant::now() < deadline, "Bluetooth event node was not discovered");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(found.device, node);
        assert_eq!(found.name, "Xbox Bluetooth service test");
        virtual_pad.emit(&[InputEvent::new(1, KeyCode::BTN_SOUTH.0, 1)]).unwrap();
        let (send, receive) = mpsc::channel();
        let watching = thread::spawn(move || interface::watch_pad(&found.device, |view| { let _ = send.send(*view); }));
        let held = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(held.pressed(model::Button::A), "reopening must report the held button, not neutral");
        drop(virtual_pad);
        assert!(watching.join().unwrap().is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn discovery_finds_exactly_the_matching_games_windows_and_pads() {
        let mut rng = Rng::new(10);
        let root = std::env::temp_dir().join(format!("service-discovery-{}", std::process::id()));
        for case in 0..cases() / 10 {
            let proc_root = root.join(format!("proc-{case}"));
            fs::create_dir_all(&proc_root).unwrap();
            let mut expected = Vec::new();
            for pid in 10..10 + rng.below(5) as u32 {
                let dir = proc_root.join(pid.to_string());
                fs::create_dir_all(&dir).unwrap();
                let (comm, comm_ok) = rng.pick(&[("Warcraft III.ex\n", true), ("WARCRAFT III.EX\n", true), ("bash\n", false), ("Warcraft\n", false)]);
                let (command, command_ok) = rng.pick(&[
                    ("C:\\Program Files (x86)\\Warcraft III\\_retail_\\x86_64\\Warcraft III.exe", true),
                    ("/opt/wine/warcraft iii.EXE", true),
                    ("C:\\Games\\Warcraft III.exe\\launcher.exe", false),
                    ("/usr/bin/bash", false),
                ]);
                let display = rng.pick(&[Some(":0"), Some(":1"), Some(":0.0"), None]);
                let prefix = rng.chance(85);
                let start = 100 + rng.below(1000) as u64;
                fs::write(dir.join("comm"), comm).unwrap();
                fs::write(dir.join("cmdline"), format!("{command}\0-launch\0")).unwrap();
                let mut environ = "HOME=/home/player\0".to_owned();
                if let Some(display) = display { environ += &format!("DISPLAY={display}\0"); }
                if prefix { environ += &format!("WINEPREFIX=/prefix/{pid}/\0"); }
                fs::write(dir.join("environ"), environ).unwrap();
                fs::write(dir.join("stat"), format!("{pid} (Warcraft III) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 {start} 20")).unwrap();
                if comm_ok && command_ok && display == Some(":0") && prefix {
                    expected.push((pid, start, PathBuf::from(format!("/prefix/{pid}/"))));
                }
            }
            fs::create_dir_all(proc_root.join("self")).unwrap();
            let mut found = warcraft_processes(&proc_root, ":0");
            found.sort();
            assert_eq!(found, expected);

            let pads = root.join(format!("pads-{case}"));
            fs::create_dir_all(&pads).unwrap();
            let target = pads.join("event9");
            fs::write(&target, "").unwrap();
            let mut links: Vec<(bool, String)> = Vec::new();
            for (name, xbox) in [("usb-Generic_Pad-event-joystick", false), ("usb-Microsoft_Controller_1-event-joystick", true),
                ("usb-8BitDo_XBOX_Wireless-event-joystick", true), ("usb-Microsoft_Controller_1-joystick", true), ("usb-Sony_Pad-event-joystick", false)] {
                if rng.chance(50) {
                    let dangling = rng.chance(15);
                    std::os::unix::fs::symlink(if dangling { pads.join("gone") } else { target.clone() }, pads.join(name)).unwrap();
                    if !dangling && name.ends_with("-event-joystick") {
                        links.push((!xbox, name.to_owned()));
                    }
                }
            }
            links.sort();
            let found = find_pad(&pads);
            match links.first() {
                Some((_, name)) => {
                    let found = found.expect("a linked pad");
                    assert_eq!(found.link, pads.join(name));
                    assert_eq!(found.device, fs::canonicalize(&target).unwrap());
                }
                None => assert_eq!(found, None),
            }

            let candidates = [
                (38u64, 3140u64, "Steam", "steam"),
                (92, 3140, "Warcraft III", "steam_app_1"),
                (93, 3140, "Warcraft III", "steam_app_1"),
                (4, 58013, "Chrome", "google-chrome"),
                (5, 777, "Warcraft III", "other"),
            ];
            let chosen: Vec<_> = candidates.iter().filter(|_| rng.chance(50)).collect();
            let windows = serde_json::Value::Array(chosen.iter().map(|(id, pid, title, app)| serde_json::json!({"id": id, "pid": pid, "title": title, "app_id": app})).collect());
            let pid = rng.pick(&[777u32, 58013, 1]);
            let (title, class) = rng.pick(&[("Warcraft III", "steam_app_1"), ("", ""), ("Warcraft III", "other")]);
            let matching: Vec<u64> = chosen.iter()
                .filter(|(_, p, t, a)| *p == u64::from(pid) || (!title.is_empty() && *t == title && *a == class))
                .map(|(id, ..)| *id).collect();
            assert_eq!(niri_game_window(&windows, pid, title, class), (matching.len() == 1).then(|| matching[0]), "{windows} {pid} {title} {class}");
        }
        fs::remove_dir_all(root).unwrap();
    }
}
