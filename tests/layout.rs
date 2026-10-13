//! The README's sample: a key layout file for an ordinary map (Blizzard's
//! melee map Echo Isles, examples/echo-isles/layout.json) served by the real
//! `padbridge --service` with no map plug-in, a virtual pad and a
//! stand-in game that records its presses. Needs a writable /dev/uinput; without
//! one it fails rather than passing unrun.
#![cfg(target_os = "linux")]

use evdev::{AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, EventType, InputEvent, KeyCode, UinputAbsSetup, uinput::VirtualDevice};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Service(Child);

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn virtual_pad() -> Option<(VirtualDevice, PathBuf)> {
    if fs::OpenOptions::new().write(true).open("/dev/uinput").is_err() {
        return None;
    }
    let mut keys = AttributeSet::<KeyCode>::new();
    for key in [KeyCode::BTN_SOUTH, KeyCode::BTN_EAST, KeyCode::BTN_NORTH, KeyCode::BTN_WEST, KeyCode::BTN_TL, KeyCode::BTN_TR, KeyCode::BTN_START, KeyCode::BTN_SELECT] {
        keys.insert(key);
    }
    let mut builder = VirtualDevice::builder().unwrap().name("padbridge layout test pad").with_keys(&keys).unwrap();
    for axis in [Abs::ABS_X, Abs::ABS_Y, Abs::ABS_RX, Abs::ABS_RY] {
        builder = builder.with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, -32768, 32767, 16, 128, 0))).unwrap();
    }
    for axis in [Abs::ABS_Z, Abs::ABS_RZ] {
        builder = builder.with_absolute_axis(&UinputAbsSetup::new(axis, AbsInfo::new(0, 0, 1023, 0, 0, 0))).unwrap();
    }
    let mut pad = builder.build().unwrap();
    let node = pad.enumerate_dev_nodes_blocking().unwrap().find_map(Result::ok).unwrap();
    Some((pad, node))
}

fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn presses(file: &Path) -> Vec<String> {
    fs::read_to_string(file).unwrap_or_default().lines().filter(|line| !line.starts_with("pointer ")).map(str::to_owned).collect()
}

#[test]
fn a_layout_file_plays_an_ordinary_map_without_a_plugin() {
    let (mut pad, node) = virtual_pad().expect("could not run: /dev/uinput is not writable, so there is no virtual pad");
    until("access to the virtual pad", || fs::File::open(&node).ok());
    let root = std::env::temp_dir().join(format!("padbridge-layout-{}", std::process::id()));
    let (pads, documents) = (root.join("pads"), root.join("Warcraft III"));
    fs::create_dir_all(&pads).unwrap();
    fs::create_dir_all(&documents).unwrap();
    std::os::unix::fs::symlink(&node, pads.join("usb-Microsoft_Controller_TEST-event-joystick")).unwrap();
    fs::write(documents.join("game"), "1").unwrap();
    let layout = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/echo-isles/layout.json");
    let service = Service(Command::new(env!("CARGO_BIN_EXE_padbridge"))
        .args(["--service", "--layout", layout.to_str().unwrap(), "--pads", pads.to_str().unwrap(),
            "--headless", documents.to_str().unwrap(), "--status", root.join("status.txt").to_str().unwrap(),
            "--settings", root.join("settings.json").to_str().unwrap(), "--interface", "off", "--poll-ms", "20"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap());
    let pressed = documents.join("pressed.txt");
    until("the Any map output", || pressed.exists().then_some(()));
    let device = fs::canonicalize(&node).unwrap();
    let fds = PathBuf::from(format!("/proc/{}/fd", service.0.id()));
    until("the service to open the virtual pad", || {
        fs::read_dir(&fds).ok()?.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|target| target == device)).then_some(())
    });
    let mut emit = |kind: EventType, code: u16, value: i32| {
        let before = presses(&pressed).len();
        pad.emit(&[InputEvent::new(kind.0, code, value)]).unwrap();
        until("the stand-in game to record the press", || (presses(&pressed).len() > before).then_some(()));
    };
    for (kind, code, high) in [
        (EventType::KEY, KeyCode::BTN_NORTH.0, 1),
        (EventType::ABSOLUTE, Abs::ABS_RZ.0, 1023),
        (EventType::KEY, KeyCode::BTN_SOUTH.0, 1),
        (EventType::KEY, KeyCode::BTN_TL.0, 1),
        (EventType::ABSOLUTE, Abs::ABS_X.0, -32768),
    ] {
        emit(kind, code, high);
        emit(kind, code, 0);
    }
    // X hero ability 1, RT workers, A select, LB ultimate, left stick camera.
    let expected = ["down q", "up q", "down 2", "up 2", "click left down", "click left up", "down r", "up r", "down left", "up left"];
    until("the layout's presses", || (presses(&pressed).len() >= expected.len()).then_some(()));
    assert_eq!(presses(&pressed), expected);
    let _ = fs::remove_dir_all(root);
}
