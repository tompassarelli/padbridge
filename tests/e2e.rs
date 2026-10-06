//! Windows/macOS end-to-end check of the real helper (`--features e2e`, needs a
//! desktop session). wc3-controller drives an SDL virtual gamepad and must type
//! the #18 layout only into a focused stand-in whose executable is named like
//! Warcraft III. Stand-ins record what their windows receive; the operating
//! system's key state shows whether the helper released what it pressed, since
//! SDL hides key-ups in a window that lost focus or never saw the press.
#![cfg(all(feature = "e2e", any(windows, target_os = "macos")))]

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const HELPER: &str = env!("CARGO_BIN_EXE_wc3-controller");
const STANDIN: &str = env!("CARGO_BIN_EXE_wc3-standin");
const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(400);
const SETTLE: Duration = Duration::from_millis(150);

type Lines = Arc<Mutex<Vec<String>>>;

struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines,
    stderr: Lines,
}

fn collect(stream: impl std::io::Read + Send + 'static) -> Lines {
    let lines = Lines::default();
    let sink = Arc::clone(&lines);
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    lines
}

impl Process {
    fn spawn(program: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("start {}: {e}", program.display()));
        Self {
            stdin: child.stdin.take(),
            stdout: collect(child.stdout.take().unwrap()),
            stderr: collect(child.stderr.take().unwrap()),
            child,
        }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("process input is open");
        writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .expect("send command");
    }

    fn stdout(&self) -> Vec<String> {
        self.stdout.lock().unwrap().clone()
    }

    fn stderr(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Standin {
    process: Process,
    log: PathBuf,
    pid: u32,
}

impl Standin {
    /// The copy's file name is the identity the helper's foreground adapter checks.
    fn launch(dir: &Path, name: &str) -> Self {
        let program = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(STANDIN, &program).expect("copy stand-in");
        let log = dir.join(format!("{name}.log"));
        let process = Process::spawn(&program, &[name, log.to_str().unwrap()]);
        let pid = process.child.id();
        let standin = Self { process, log, pid };
        let start = Instant::now();
        while !standin.entries().iter().any(|(kind, _)| kind == "ready") {
            assert!(
                start.elapsed() < WAIT,
                "{name} did not open: {:?}",
                standin.process.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
        standin
    }

    fn entries(&self) -> Vec<(String, String)> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t').skip(1);
                Some((
                    fields.next()?.to_owned(),
                    fields.next().unwrap_or("").to_owned(),
                ))
            })
            .collect()
    }

    fn keys(&self) -> Vec<(String, String)> {
        self.entries()
            .into_iter()
            .filter(|(kind, _)| kind == "key_down" || kind == "key_up")
            .collect()
    }

    fn focused(&self) -> bool {
        self.entries()
            .iter()
            .rev()
            .find(|(kind, _)| kind == "focus_gained" || kind == "focus_lost")
            .is_some_and(|(kind, _)| kind == "focus_gained")
    }
}

#[cfg(windows)]
mod os {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    pub fn key_down(key: char) -> bool {
        // Letter virtual-key codes are the uppercase ASCII values.
        // SAFETY: GetAsyncKeyState only reads the desktop's key state.
        (unsafe { GetAsyncKeyState(key.to_ascii_uppercase() as i32) } as u16) & 0x8000 != 0
    }

    /// Stand-ins raise themselves; SDL_FORCE_RAISEWINDOW overrides the
    /// foreground lock a background request would otherwise meet.
    pub fn activate(_pid: u32) {}
}

#[cfg(target_os = "macos")]
mod os {
    use std::ffi::{c_char, c_void};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
        fn AXUIElementSetAttributeValue(
            element: *const c_void,
            attribute: *const c_void,
            value: *const c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: *const c_void;
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    const HID_SYSTEM_STATE: i32 = 1;
    const UTF8: u32 = 0x0800_0100;

    pub fn key_down(key: char) -> bool {
        // ANSI virtual key codes from HIToolbox Events.h.
        let code = match key {
            'i' => 0x22,
            'n' => 0x2D,
            'o' => 0x1F,
            'q' => 0x0C,
            'u' => 0x20,
            _ => panic!("no key code for {key}"),
        };
        // SAFETY: reads the HID system's key state; no pointers involved.
        unsafe { CGEventSourceKeyState(HID_SYSTEM_STATE, code) }
    }

    /// Since macOS 14 an app cannot take activation for itself; the
    /// accessibility frontmost attribute moves it the way a user switch does.
    pub fn activate(pid: u32) {
        // SAFETY: each created CoreFoundation object is released once; the
        // attribute name is a NUL-terminated literal.
        let result = unsafe {
            let app = AXUIElementCreateApplication(pid as i32);
            let attribute =
                CFStringCreateWithCString(std::ptr::null(), c"AXFrontmost".as_ptr(), UTF8);
            let result = AXUIElementSetAttributeValue(app, attribute, kCFBooleanTrue);
            CFRelease(attribute);
            CFRelease(app);
            result
        };
        eprintln!("ax_frontmost pid={pid} result={result}");
    }
}

struct Harness {
    helper: Process,
    game: Standin,
    other: Standin,
}

fn key(kind: &str, name: &str) -> (String, String) {
    (kind.into(), name.into())
}

fn down(name: &str) -> (String, String) {
    key("key_down", name)
}

fn up(name: &str) -> (String, String) {
    key("key_up", name)
}

impl Harness {
    fn report(&self) -> String {
        format!(
            "\n--- helper stderr ---\n{}\n--- helper stdout ---\n{}\n--- Warcraft III stand-in ---\n{}\n--- other stand-in ---\n{}",
            self.helper.stderr().join("\n"),
            self.helper.stdout().join("\n"),
            std::fs::read_to_string(&self.game.log).unwrap_or_default(),
            std::fs::read_to_string(&self.other.log).unwrap_or_default(),
        )
    }

    fn wait(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(
                start.elapsed() < WAIT,
                "timed out waiting for {what}{}",
                self.report()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn events(&self) -> usize {
        self.helper
            .stdout()
            .iter()
            .filter(|line| line.starts_with("event\t"))
            .count()
    }

    fn eligible(&self) -> Option<bool> {
        self.helper.stderr().iter().rev().find_map(|line| {
            line.split_once("game-eligible=")
                .map(|(_, value)| value == "true")
        })
    }

    fn os_key(&self, key: char, expected: bool, why: &str) {
        self.wait(
            &format!(
                "OS key {key} {} ({why})",
                if expected { "down" } else { "up" }
            ),
            |_| os::key_down(key) == expected,
        );
    }

    /// Send one pad command and require exactly these new key events in the game
    /// window and none in the other window.
    fn step(&mut self, command: &str, expected: &[(String, String)]) {
        let (game, other, events) = (
            self.game.keys().len(),
            self.other.keys().len(),
            self.events(),
        );
        self.helper.send(command);
        self.wait(&format!("helper to observe {command:?}"), |h| {
            h.events() > events
        });
        if expected.is_empty() {
            thread::sleep(QUIET);
        } else {
            self.wait(&format!("game keys for {command:?}"), |h| {
                h.game.keys().len() >= game + expected.len()
            });
            thread::sleep(SETTLE);
        }
        let keys = self.game.keys();
        let received = &keys[game..];
        assert_eq!(
            received,
            expected,
            "game keys after {command:?}{}",
            self.report()
        );
        assert!(
            self.other.keys().len() == other,
            "other window received keys after {command:?}{}",
            self.report()
        );
    }

    fn focus_game(&mut self, game: bool) {
        let (target, label) = if game {
            (&mut self.game, "game")
        } else {
            (&mut self.other, "other")
        };
        os::activate(target.pid);
        target.process.send("raise");
        self.wait(&format!("{label} window focus"), |h| {
            h.game.focused() == game && h.other.focused() != game
        });
    }
}

#[test]
fn helper_types_the_layout_only_into_the_focused_game() {
    let dir = std::env::temp_dir().join(format!("wc3-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let game = Standin::launch(&dir, "Warcraft III");
    let other = Standin::launch(&dir, "Other");
    let helper = Process::spawn(
        Path::new(HELPER),
        &["--virtual-pad", "--emit", "--watch-seconds", "600"],
    );
    let mut h = Harness {
        helper,
        game,
        other,
    };
    h.focus_game(true);
    h.wait("helper eligibility", |h| h.eligible() == Some(true));
    assert!(
        h.helper
            .stderr()
            .iter()
            .any(|line| line.contains("gamepads=1"))
            && h.helper
                .stderr()
                .iter()
                .any(|line| line.contains("Smashcraft virtual pad")),
        "helper did not select the virtual pad{}",
        h.report()
    );

    // #18 layout: A attack, X special, B/Y jump, RB grab, either trigger
    // shield, LB walk, stick directions (stick-up is only up), Start pause.
    for (press, release, keys) in [
        ("button a 1", "button a 0", vec!["n"]),
        ("button x 1", "button x 0", vec!["u"]),
        ("button b 1", "button b 0", vec!["i"]),
        ("button y 1", "button y 0", vec!["i"]),
        ("axis lefty -32768", "axis lefty 0", vec!["space"]),
        (
            "button rightshoulder 1",
            "button rightshoulder 0",
            vec!["o"],
        ),
        (
            "axis lefttrigger 32767",
            "axis lefttrigger -32768",
            vec!["q"],
        ),
        (
            "axis righttrigger 32767",
            "axis righttrigger -32768",
            vec!["q"],
        ),
        ("button leftshoulder 1", "button leftshoulder 0", vec!["p"]),
        ("axis leftx -32768", "axis leftx 0", vec!["w"]),
        ("axis leftx 32767", "axis leftx 0", vec!["r"]),
        ("axis lefty 32767", "axis lefty 0", vec!["e"]),
        ("button start 1", "button start 0", vec!["y"]),
    ] {
        h.step(press, &keys.iter().map(|k| down(k)).collect::<Vec<_>>());
        h.step(release, &keys.iter().map(|k| up(k)).collect::<Vec<_>>());
    }

    // Both ordered overlaps of the jump buttons keep one held jump.
    let sources = [("button b 1", "button b 0"), ("button y 1", "button y 0")];
    for (first, second) in [(0, 1), (1, 0)] {
        let (first, second) = (sources[first], sources[second]);
        h.step(first.0, &[down("i")]);
        h.step(second.0, &[]);
        h.step(first.1, &[]);
        h.step(second.1, &[up("i")]);
    }
    h.step("axis lefttrigger 32767", &[down("q")]);
    h.step("axis righttrigger 32767", &[]);
    h.step("axis lefttrigger -32768", &[]);
    h.step("axis righttrigger -32768", &[up("q")]);

    // Focus loss releases held keys at the OS and suppresses later presses.
    h.step("button a 1", &[down("n")]);
    h.step("axis lefttrigger 32767", &[down("q")]);
    h.os_key('n', true, "held while focused");
    h.os_key('q', true, "held while focused");
    h.focus_game(false);
    h.wait("helper focus loss", |h| h.eligible() == Some(false));
    h.os_key('n', false, "released on focus loss");
    h.os_key('q', false, "released on focus loss");
    h.step("button x 1", &[]);
    assert!(
        !os::key_down('u'),
        "unfocused press reached the OS{}",
        h.report()
    );
    h.step("button x 0", &[]);
    h.step("button a 0", &[]);

    // Back in the game, a control held through the switch must return to
    // neutral before anything is typed again.
    h.focus_game(true);
    h.wait("helper focus return", |h| h.eligible() == Some(true));
    h.step("button x 1", &[]);
    h.step("button x 0", &[]);
    h.step("axis lefttrigger -32768", &[]);
    h.step("button a 1", &[down("n")]);
    h.step("button a 0", &[up("n")]);

    // Disconnect releases everything held.
    h.step("button rightshoulder 1", &[down("o")]);
    h.step("button b 1", &[down("i")]);
    h.os_key('o', true, "held before disconnect");
    h.step("detach", &[up("i"), up("o")]);
    h.os_key('o', false, "released on disconnect");
    h.os_key('i', false, "released on disconnect");

    h.helper.send("quit");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = h.helper.child.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < WAIT, "helper did not exit{}", h.report());
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "helper exit {status}{}", h.report());
    assert!(
        !h.other.entries().iter().any(|(kind, _)| kind == "key_down"),
        "other window received a key press{}",
        h.report()
    );
    println!("{}", h.report());
}
