//! The fighter layout's settings through the real `wc3-controller --service`:
//! a window's choices take effect live and survive a restart.
#![cfg(target_os = "linux")]

use std::{
    fs,
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

#[test]
fn layout_tap_jump_and_trigger_choices_take_effect_live_and_survive_service_restart() {
    use std::{io::{BufRead, BufReader, Write}, net::{TcpListener, TcpStream}};
    use wc3_controller::model::{ClientMessage, ControllerSettings, PadPreset, ServiceMessage, TriggerShield, TriggerShields};
    let root = std::env::temp_dir().join(format!("wc3-controller-settings-{}", std::process::id()));
    let documents = root.join("game");
    let pads = root.join("pads");
    fs::create_dir_all(&documents).unwrap();
    fs::create_dir_all(&pads).unwrap();
    let path = root.join("controller.json");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let start = || Service(Command::new(env!("CARGO_BIN_EXE_wc3-controller"))
        .args(["--service", "--pads", pads.to_str().unwrap(), "--headless", documents.to_str().unwrap(),
            "--settings", path.to_str().unwrap(), "--status", root.join("status.txt").to_str().unwrap(),
            "--interface", &address.to_string(), "--poll-ms", "10"])
        .stdin(Stdio::null()).spawn().unwrap());
    let connect = || {
        let stream = until("isolated service interface", || TcpStream::connect(address).ok());
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        stream
    };
    let read = |stream: &TcpStream| {
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
        let ServiceMessage::Status(snapshot) = ServiceMessage::parse(&line).unwrap() else { panic!("expected settings snapshot") };
        snapshot.settings
    };
    let service = start();
    let mut stream = connect();
    assert_eq!(read(&stream), ControllerSettings::default());
    let selected = ControllerSettings {
        pad_preset: PadPreset::ZJump, tap_jump: true,
        triggers: TriggerShields { left: TriggerShield::Light, right: TriggerShield::Full },
    };
    for message in [ClientMessage::PadPreset(selected.pad_preset), ClientMessage::TapJump(selected.tap_jump), ClientMessage::TriggerShields(selected.triggers)] {
        stream.write_all(message.line().as_bytes()).unwrap();
    }
    until("all three settings saved in one file", || fs::read_to_string(&path).ok()
        .and_then(|text| serde_json::from_str::<ControllerSettings>(&text).ok()).filter(|saved| *saved == selected));
    assert_eq!(read(&stream), selected);
    drop(stream);
    drop(service);
    let service = start();
    let stream = connect();
    assert_eq!(read(&stream), selected);
    drop(stream);
    drop(service);
    fs::remove_dir_all(root).unwrap();
}
