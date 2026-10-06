//! A fake Smashcraft session served by the real `wc3-journal --service`: a
//! virtual pad, a stand-in game folder whose menu files the test writes, and
//! the helper typing into a file. Needs a writable /dev/uinput.
#![cfg(target_os = "linux")]

use evdev::{AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, KeyCode, UinputAbsSetup, uinput::VirtualDevice};
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
    let mut builder = VirtualDevice::builder().unwrap().name("wc3 service test pad").with_keys(&keys).unwrap();
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

fn menu(dir: &Path, epoch: u32, phase: &str) {
    let body = format!("function PreloadFiles takes nothing returns nothing\n\tcall Preload( \"SMASHCRAFT JOURNAL MENU v=1 build=itest slot=0 epoch={epoch} phase={phase}\" )\nendfunction\n");
    let path = dir.join("smashcraft-journal-menu-itest-s0.txt");
    fs::write(path.with_extension("tmp"), body).unwrap();
    fs::rename(path.with_extension("tmp"), path).unwrap();
}

fn field(status: &Path, key: &str) -> Option<String> {
    fs::read_to_string(status).ok()?.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix('=').map(str::to_owned))
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

/// The serving helper's PID once it is ready.
fn serving(status: &Path) -> Option<String> {
    (field(status, "state")? == "serving").then(|| field(status, "helper_pid")).flatten()
}

#[test]
fn the_service_follows_a_fake_session_through_new_sessions_and_game_restarts() {
    let Some((_pad, node)) = virtual_pad() else {
        eprintln!("skipped: /dev/uinput is not writable, so no virtual pad");
        return;
    };
    // udev grants access to a new node a moment after it appears.
    until("access to the virtual pad", || fs::File::open(&node).ok());
    let root = std::env::temp_dir().join(format!("wc3-service-{}", std::process::id()));
    let (pads, documents, status) = (root.join("pads"), root.join("Warcraft III"), root.join("status.txt"));
    let data = documents.join("CustomMapData");
    fs::create_dir_all(&pads).unwrap();
    fs::create_dir_all(&data).unwrap();
    std::os::unix::fs::symlink(&node, pads.join("usb-Microsoft_Controller_TEST-event-joystick")).unwrap();
    fs::write(documents.join("game"), "1").unwrap();
    menu(&data, 0, "CHARACTER");

    let _service = Service(Command::new(env!("CARGO_BIN_EXE_wc3-journal"))
        .args(["--service", "--pads", pads.to_str().unwrap(), "--headless", documents.to_str().unwrap(), "--status", status.to_str().unwrap(), "--poll-ms", "50", "--interface", "off"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap());

    // No arguments about the session: it is read from the map's files.
    let first = until("a helper for the session", || serving(&status));
    assert_eq!(field(&status, "pad_device").unwrap(), node.display().to_string());
    assert!(field(&status, "session").unwrap().starts_with("itest/s0/"));

    // A later match of the same session keeps the helper.
    menu(&data, 2, "RESULT");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(serving(&status).as_deref(), Some(first.as_str()));

    // The map opened again: its epoch starts over and a fresh helper follows it.
    menu(&data, 0, "CHARACTER");
    let second = until("a helper for the new session", || serving(&status).filter(|pid| *pid != first));
    // The old helper is gone.
    until("the old helper to end", || (!Path::new(&format!("/proc/{first}")).exists()).then_some(()));

    // The new session's first match is adopted: the helper announces readiness.
    fs::write(data.join("smashcraft-journal-ready-itest-e1-p0.txt"),
        "function PreloadFiles takes nothing returns nothing\n\tcall Preload( \"SMASHCRAFT JOURNAL v=1 build=itest epoch=1 slot=0 delay=0 first_frame=1\" )\nendfunction\n").unwrap();
    until("the helper's readiness for epoch 1", || fs::read_to_string(documents.join("typed.txt")).ok().filter(|typed| typed.contains("|JR11;")));

    // Warcraft III restarted: the helper is replaced for the new game.
    fs::write(documents.join("game"), "2").unwrap();
    until("a helper for the restarted game", || serving(&status).filter(|pid| *pid != second));

    drop(_service);
    let _ = fs::remove_dir_all(root);
}
