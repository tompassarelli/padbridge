//! The always-on controller service: finds Warcraft III on a display, the
//! controller and the map's session by itself, and keeps one helper process
//! serving them, replacing it when any of them changes.
//!
//! Game, window and pad discovery are map-agnostic. What a map asks of the
//! helper, and the helper's command line, belong to a [`Profile`];
//! [`smashcraft::Smashcraft`] is the Smashcraft one.

pub mod any_map;
pub mod interface;
pub mod pointer;
pub mod smashcraft;

use crate::model;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, Window},
};

/// Where the helper delivers what it types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Warcraft III's window on an X11 display, under niri.
    Window { display: String, x11_window: u32, niri_window: u64, niri_socket: PathBuf },
    /// A stand-in game with no window: the helper appends its typing to this file.
    Headless { text_out: PathBuf },
}

/// One running Warcraft III and its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Game {
    pub pid: u32,
    /// The process start time in clock ticks: a reused PID is another game.
    pub birth: u64,
    /// Documents/Warcraft III in the game's Wine prefix.
    pub documents: PathBuf,
    pub target: Target,
}

/// The controller, found by its stable /dev/input/by-id link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pad {
    pub link: PathBuf,
    /// The event node the link points at now.
    pub device: PathBuf,
    pub name: String,
}

/// What the map currently asks of the helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The helper is replaced when this changes.
    pub key: String,
    /// For people, as in "Smashcraft playable-0047, player 1, fighter selection".
    pub summary: String,
    /// What a window shows of it.
    pub shown: Option<model::Session>,
}

/// What a helper's log line says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperEvent {
    /// Waiting for a match, menus driven by the controller.
    Ready,
    InMatch,
    Focus(bool),
    PadLost,
    PadBack,
}

/// Map-specific knowledge: the session the map publishes and the helper that serves it.
pub trait Profile {
    fn name(&self) -> &str;
    /// The map's current session in this game, if it runs one.
    fn session(&mut self, game: &Game) -> Option<Session>;
    /// The helper's arguments for this game, pad and session.
    fn args(&self, game: &Game, pad: &Pad, session: &Session) -> Vec<String>;
    fn event(&self, line: &str) -> Option<HelperEvent>;
    /// Whether the map shows a menu the pad drives with the desktop pointer now.
    fn pointer_menu(&self) -> bool {
        false
    }
    /// Whether the session plays on keys: no helper; the service presses the pad's keys itself.
    fn keys(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HelperStatus {
    pub pid: u32,
    pub ready: bool,
    pub in_match: bool,
    pub focused: Option<bool>,
    pub pad_connected: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub profile: String,
    pub game: Option<Game>,
    pub pad: Option<Pad>,
    pub session: Option<Session>,
    pub helper: Option<HelperStatus>,
    /// The pad's keys reach a session that plays on keys.
    pub keys: bool,
    /// Why the last helper stopped, or what is missing.
    pub problem: Option<String>,
}

impl Status {
    /// One `key=value` per line, for the status file.
    pub fn lines(&self) -> String {
        let mut text = format!("service_pid={}\nprofile={}\n", std::process::id(), self.profile);
        let state = match (&self.helper, &self.game, &self.pad, &self.session) {
            (Some(helper), ..) if helper.ready => "serving",
            (None, Some(_), Some(_), Some(_)) if self.keys => "serving",
            (Some(_), ..) => "starting",
            (None, None, ..) => "no-game",
            (None, _, None, _) => "no-controller",
            (None, _, _, None) => "no-session",
            (None, ..) => "starting",
        };
        text += &format!("state={state}\n");
        if let Some(game) = &self.game {
            text += &format!("game_pid={}\ngame_documents={}\n", game.pid, game.documents.display());
            if let Target::Window { display, x11_window, niri_window, .. } = &game.target {
                text += &format!("display={display}\nx11_window={x11_window}\nniri_window={niri_window}\n");
            }
        }
        if let Some(pad) = &self.pad {
            text += &format!("pad={}\npad_device={}\n", pad.name, pad.device.display());
        }
        if let Some(session) = &self.session {
            text += &format!("session={}\nsession_summary={}\n", session.key, session.summary);
        }
        if let Some(helper) = &self.helper {
            text += &format!("helper_pid={}\nin_match={}\npad_connected={}\n", helper.pid, helper.in_match, helper.pad_connected);
            if let Some(focused) = helper.focused {
                text += &format!("focused={focused}\n");
            }
        }
        if let Some(problem) = &self.problem {
            text += &format!("problem={}\n", problem.replace('\n', " "));
        }
        text
    }
}

/// How long a helper may stay without its pad while a pad is plugged in
/// before it is replaced: it reconnects the same pad by itself in 100 ms.
pub const PAD_SWAP: Duration = Duration::from_secs(3);
/// How often to look at the map's menu while the pad drives its pointer.
pub const MENU_POLL: Duration = Duration::from_millis(100);
/// How often to look for Warcraft III while none runs.
pub const IDLE_POLL: Duration = Duration::from_secs(1);
/// The wait before starting a helper again after one stopped.
pub const RETRY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Start,
    Stop(String),
}

struct Serving {
    game: Game,
    session: String,
    pad_lost_since: Option<Instant>,
}

/// When to start, keep or replace the helper; no I/O.
#[derive(Default)]
pub struct Supervisor {
    serving: Option<Serving>,
    retry_at: Option<Instant>,
}

impl Supervisor {
    pub fn step(&mut self, game: Option<&Game>, pad: Option<&Pad>, session: Option<&Session>, now: Instant) -> Decision {
        if let Some(serving) = &self.serving {
            let Some(game) = game else { return Decision::Stop("Warcraft III closed".into()) };
            if *game != serving.game {
                return Decision::Stop(if game.pid != serving.game.pid || game.birth != serving.game.birth {
                    "Warcraft III restarted".into()
                } else {
                    "Warcraft III's window changed".into()
                });
            }
            if let Some(session) = session {
                if session.key != serving.session {
                    return Decision::Stop(format!("new map session ({})", session.summary));
                }
            }
            if pad.is_some() && serving.pad_lost_since.is_some_and(|since| now.duration_since(since) >= PAD_SWAP) {
                return Decision::Stop("the controller came back as another device".into());
            }
            return Decision::Keep;
        }
        if self.retry_at.is_some_and(|at| now < at) {
            return Decision::Keep;
        }
        if game.is_some() && pad.is_some() && session.is_some() { Decision::Start } else { Decision::Keep }
    }

    /// A helper now serves this game and session.
    pub fn started(&mut self, game: &Game, session: &Session) {
        self.serving = Some(Serving { game: game.clone(), session: session.key.clone(), pad_lost_since: None });
    }

    /// The helper exited or was stopped.
    pub fn stopped(&mut self, now: Instant) {
        self.serving = None;
        self.retry_at = Some(now + RETRY);
    }

    pub fn event(&mut self, event: HelperEvent, now: Instant) {
        let Some(serving) = &mut self.serving else { return };
        match event {
            HelperEvent::PadLost => serving.pad_lost_since = serving.pad_lost_since.or(Some(now)),
            HelperEvent::PadBack => serving.pad_lost_since = None,
            _ => {}
        }
    }
}

// ---- Discovery ----

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

fn process_birth(process: &Path) -> Option<u64> {
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

/// The controller among `dir`'s `*-event-joystick` links: an Xbox pad first.
pub fn find_pad(dir: &Path) -> Option<Pad> {
    let mut links: Vec<(bool, String)> = fs::read_dir(dir).ok()?.flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with("-event-joystick"))
        .map(|name| {
            let lower = name.to_ascii_lowercase();
            (!(lower.contains("microsoft") || lower.contains("xbox")), name)
        })
        .collect();
    links.sort();
    links.into_iter().find_map(|(_, name)| {
        let link = dir.join(&name);
        let device = fs::canonicalize(&link).ok()?;
        let node = device.file_name()?.to_str()?.to_owned();
        let pad_name = fs::read_to_string(format!("/sys/class/input/{node}/device/name"))
            .map(|name| name.trim().to_owned())
            .unwrap_or(name);
        Some(Pad { link, device, name: pad_name })
    })
}

// ---- Running it ----

pub struct Config {
    /// The X11 display Warcraft III runs on.
    pub display: String,
    /// Where the pads' stable links are.
    pub pads: PathBuf,
    /// The helper executable.
    pub helper: PathBuf,
    /// Rewritten on every change; absent writes none.
    pub status_file: Option<PathBuf>,
    pub poll: Duration,
    /// A stand-in game with no window: its Documents folder. Its `game` file
    /// names the game; changing it is a restart. The helper types into `typed.txt` there.
    pub headless: Option<PathBuf>,
    /// The local interface's address (interface::ADDRESS); absent opens none.
    pub interface: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self {
            display: ":0".into(),
            pads: "/dev/input/by-id".into(),
            helper: std::env::current_exe().unwrap_or_else(|_| "wc3-journal".into()),
            status_file: Some(home.join(".local/state/smashcraft/controller-service.txt")),
            poll: Duration::from_millis(250),
            headless: None,
            interface: Some(interface::ADDRESS.into()),
        }
    }
}

fn headless_game(documents: &Path) -> Option<Game> {
    let name = fs::read_to_string(documents.join("game")).ok()?;
    let birth = name.trim().parse().ok()?;
    Some(Game { pid: 0, birth, documents: documents.into(), target: Target::Headless { text_out: documents.join("typed.txt") } })
}

struct Running {
    child: Child,
    lines: mpsc::Receiver<String>,
}

fn stop_child(running: Running) {
    let Running { mut child, .. } = running;
    // The edit-box helper holds no keys, so ending it releases nothing.
    let _ = child.kill();
    let _ = child.wait();
}

/// The lock one service holds for its display (or stand-in game), so a
/// second copy never runs a second helper typing into the same game.
pub fn lock_path(config: &Config) -> PathBuf {
    match &config.headless {
        Some(documents) => documents.join("wc3-controller.lock"),
        None => {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            runtime.join(format!("wc3-controller-{}.lock", config.display.replace(['/', ':'], "")))
        }
    }
}

/// Keeps a helper serving the game until `stop`; `report` sees every status change.
pub fn run(config: &Config, profile: &mut dyn Profile, stop: &AtomicBool, mut report: impl FnMut(&Status)) -> Result<(), String> {
    let lock_file = lock_path(config);
    let lock = fs::File::create(&lock_file).map_err(|e| format!("open {}: {e}", lock_file.display()))?;
    if lock.try_lock().is_err() {
        return Err(format!("another controller service is already running for display {} (lock {})", config.display, lock_file.display()));
    }
    let interface = config.interface.as_deref().map(interface::Interface::listen).transpose()
        .map_err(|error| format!("{error}: is another controller service running?"))?;
    if let Some(interface) = &interface {
        eprintln!("service: windows connect to {}", interface.address);
    }
    let mut choice = model::ProfileChoice::Auto;
    let mut pad_preset = model::PadPreset::Standard;
    let mut bindings = any_map::default_bindings();
    // The running Any map output's feed; the pad watcher sends every state to it.
    let feed: Arc<Mutex<Option<mpsc::Sender<any_map::Feed>>>> = Arc::new(Mutex::new(None));
    let mut any_map_running: Option<(any_map::Window, u32, any_map::Kind, Arc<AtomicBool>, Arc<AtomicBool>)> = None;
    let mut watching: Option<(PathBuf, Arc<AtomicBool>)> = None;
    let mut supervisor = Supervisor::default();
    let mut running: Option<Running> = None;
    let mut status = Status { profile: profile.name().into(), ..Status::default() };
    let mut known: Option<Game> = None;
    let mut first = true;
    let mut written = String::new();
    let mut publish = |status: &Status, written: &mut String| {
        let text = status.lines();
        if text == *written {
            return;
        }
        if let Some(path) = &config.status_file {
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let temporary = path.with_extension("tmp");
            if fs::write(&temporary, &text).is_ok() {
                let _ = fs::rename(&temporary, path);
            }
        }
        report(status);
        *written = text;
    };
    eprintln!("service: profile={} display={} pads={} helper={}", profile.name(), config.display, config.pads.display(), config.helper.display());
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if let Some(current) = &mut running {
            while let Ok(line) = current.lines.try_recv() {
                eprintln!("helper: {line}");
                if let (Some(event), Some(helper)) = (profile.event(&line), status.helper.as_mut()) {
                    match event {
                        HelperEvent::Ready => { helper.ready = true; helper.in_match = false; }
                        HelperEvent::InMatch => helper.in_match = true,
                        HelperEvent::Focus(focused) => helper.focused = Some(focused),
                        HelperEvent::PadLost => helper.pad_connected = false,
                        HelperEvent::PadBack => helper.pad_connected = true,
                    }
                    supervisor.event(event, now);
                }
            }
            if let Ok(Some(exit)) = current.child.try_wait() {
                // Drain what it said last.
                while let Ok(line) = current.lines.recv_timeout(Duration::from_millis(50)) {
                    eprintln!("helper: {line}");
                    if line.starts_with("wc3-journal: ") {
                        status.problem = Some(line.trim_start_matches("wc3-journal: ").to_owned());
                    }
                }
                eprintln!("service: helper exited ({exit})");
                running = None;
                status.helper = None;
                supervisor.stopped(now);
                // Its window may be gone; look again.
                known = None;
            }
        }
        let game = match &config.headless {
            Some(documents) => headless_game(documents),
            None => match &known {
                // A known game stays known while its process lives.
                Some(game) if process_birth(Path::new(&format!("/proc/{}", game.pid))) == Some(game.birth) => known.clone(),
                _ => find_game(&config.display),
            },
        };
        if game != known {
            match &game {
                Some(game) => eprintln!("service: Warcraft III pid={} documents={}", game.pid, game.documents.display()),
                None => eprintln!("service: Warcraft III is gone"),
            }
            known = game.clone();
        }
        if first && game.is_none() {
            eprintln!("service: waiting for Warcraft III on {}", config.display);
        }
        first = false;
        let pad = find_pad(&config.pads);
        if pad.as_ref().map(|pad| &pad.device) != status.pad.as_ref().map(|pad| &pad.device) {
            match &pad {
                Some(pad) => eprintln!("service: controller {} at {}", pad.name, pad.device.display()),
                None => eprintln!("service: no controller in {}", config.pads.display()),
            }
        }
        let session = game.as_ref().and_then(|game| profile.session(game));
        if session != status.session {
            if let Some(session) = &session {
                eprintln!("service: session {} ({})", session.key, session.summary);
            }
        }
        if let Some(interface) = &interface {
            for message in interface.commands() {
                match message {
                    model::ClientMessage::PadPreset(preset) => {
                        if preset != pad_preset {
                            pad_preset = preset;
                            if let Some(current) = running.take() { stop_child(current); }
                            status.helper = None;
                            supervisor = Supervisor::default();
                            if any_map_running.as_ref().is_some_and(|(_, _, kind, ..)| *kind == any_map::Kind::Keys) {
                                if let Some((_, _, _, stop, _)) = any_map_running.take() { stop.store(true, Ordering::Relaxed); }
                                *feed.lock().unwrap() = None;
                            }
                        }
                    }
                    model::ClientMessage::Profile(chosen) => {
                        eprintln!("service: profile chosen: {chosen:?}");
                        choice = chosen;
                    }
                    model::ClientMessage::Bindings(new) => {
                        eprintln!("service: {} Any map bindings received", new.len());
                        if any_map_running.as_ref().is_some_and(|(_, _, kind, ..)| *kind == any_map::Kind::AnyMap) {
                            if let Some(send) = feed.lock().unwrap().as_ref() {
                                let _ = send.send(any_map::Feed::Bindings(new.clone()));
                            }
                        }
                        bindings = new;
                    }
                }
            }
        }
        // The pad's live state, for windows and the Any map output; a new
        // watcher follows a new device.
        if let Some(pad) = &pad {
            if watching.as_ref().is_none_or(|(device, alive)| *device != pad.device || !alive.load(Ordering::Relaxed)) {
                if let Some((_, alive)) = watching.take() {
                    alive.store(false, Ordering::Relaxed);
                }
                let alive = Arc::new(AtomicBool::new(true));
                let (device, flag, out, feed) = (pad.device.clone(), Arc::clone(&alive), interface.clone(), Arc::clone(&feed));
                thread::spawn(move || {
                    let _ = interface::watch_pad(&device, |view| {
                        if flag.load(Ordering::Relaxed) {
                            if let Some(out) = &out {
                                out.input(view);
                            }
                            if let Some(send) = feed.lock().unwrap().as_ref() {
                                let _ = send.send(any_map::Feed::Input(*view));
                            }
                        }
                    });
                    flag.store(false, Ordering::Relaxed);
                });
                watching = Some((pad.device.clone(), alive));
            }
        }
        let resolved = choice.resolve(session.is_some());
        // Another profile than this one presses nothing through its helper.
        let session = if resolved == model::Profile::Smashcraft { session } else { None };
        // A session on keys needs no helper: the pad's keys output serves it.
        let keys = resolved == model::Profile::Smashcraft && session.is_some() && profile.keys();
        if resolved != model::Profile::Smashcraft || keys {
            if let Some(current) = running.take() {
                eprintln!("service: stopping the helper: {}", if keys { "the session plays on keys".to_owned() } else { format!("profile {resolved:?}") });
                stop_child(current);
                status.helper = None;
                supervisor = Supervisor::default();
            }
        }
        // Pad output into the game's window while it has focus: the Any map
        // profile, or the pointer in the map's menus (never during play).
        let menu = resolved == model::Profile::Smashcraft && profile.pointer_menu()
            && status.helper.as_ref().is_none_or(|helper| !helper.in_match);
        let kind = if keys { Some(any_map::Kind::Keys) } else if menu { Some(any_map::Kind::Menu) } else if resolved == model::Profile::AnyMap { Some(any_map::Kind::AnyMap) } else { None };
        let want = match (&game, &pad, kind) {
            (Some(Game { pid, target: Target::Window { display, niri_window, niri_socket, .. }, .. }), Some(_), Some(kind)) => {
                Some((any_map::Window { display: display.clone(), niri_socket: niri_socket.clone(), niri_window: *niri_window }, *pid, kind))
            }
            _ => None,
        };
        if any_map_running.as_ref().map(|(window, pid, kind, ..)| (window.clone(), *pid, *kind)) != want {
            if let Some((_, _, kind, stop, _)) = any_map_running.take() {
                stop.store(true, Ordering::Relaxed);
                *feed.lock().unwrap() = None;
                eprintln!("service: {} off", kind.name());
            }
            if let Some((window, pid, kind)) = want {
                let (send, receive) = mpsc::channel();
                let (stop, focused) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
                let mode = match kind {
                    any_map::Kind::Menu => any_map::Mode::Menu,
                    any_map::Kind::Keys => any_map::Mode::Keys(pad_preset),
                    any_map::Kind::AnyMap => any_map::Mode::AnyMap(bindings.clone()),
                };
                any_map::spawn(window.clone(), mode, receive, Arc::clone(&stop), Arc::clone(&focused));
                *feed.lock().unwrap() = Some(send);
                eprintln!("service: {} on for Warcraft III pid={pid}", kind.name());
                any_map_running = Some((window, pid, kind, stop, focused));
            }
        }
        status.keys = any_map_running.as_ref().is_some_and(|(_, _, kind, ..)| *kind == any_map::Kind::Keys);
        let helper_session = if keys { None } else { session.as_ref() };
        match supervisor.step(game.as_ref(), pad.as_ref(), helper_session, now) {
            Decision::Keep => {}
            Decision::Stop(reason) => {
                eprintln!("service: replacing the helper: {reason}");
                if let Some(current) = running.take() {
                    stop_child(current);
                }
                status.helper = None;
                supervisor.stopped(now);
                // Start the replacement at once.
                supervisor = Supervisor::default();
                known = None;
            }
            Decision::Start => {
                let (game, pad, session) = (game.as_ref().unwrap(), pad.as_ref().unwrap(), session.as_ref().unwrap());
                let args = profile.args(game, pad, session);
                let mut command = Command::new(&config.helper);
                command.args(&args).stdin(Stdio::null()).stderr(Stdio::piped());
                command.args(["--preset", pad_preset.name()]);
                command.env("WC3_SERVICE_PID", std::process::id().to_string());
                if let Target::Window { niri_socket, display, .. } = &game.target {
                    command.env("NIRI_SOCKET", niri_socket).env("DISPLAY", display);
                }
                match command.spawn() {
                    Ok(mut child) => {
                        let (send, lines) = mpsc::channel();
                        let stderr = child.stderr.take().expect("piped helper stderr");
                        thread::spawn(move || {
                            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                                if send.send(line).is_err() {
                                    break;
                                }
                            }
                        });
                        eprintln!("service: helper pid={} {}", child.id(), args.join(" "));
                        status.helper = Some(HelperStatus { pid: child.id(), pad_connected: true, ..HelperStatus::default() });
                        status.problem = None;
                        supervisor.started(game, session);
                        running = Some(Running { child, lines });
                    }
                    Err(error) => {
                        status.problem = Some(format!("couldn't start the helper {}: {error}", config.helper.display()));
                        supervisor.stopped(now);
                    }
                }
            }
        }
        status.game = game;
        status.pad = pad;
        status.session = session;
        let idle = status.game.is_none();
        publish(&status, &mut written);
        if let Some(interface) = &interface {
            let any_map = any_map_running.as_ref().filter(|(_, _, kind, ..)| *kind == any_map::Kind::AnyMap).map(|(.., focused)| focused.load(Ordering::Relaxed));
            interface.status(&snapshot(&status, choice, resolved, any_map));
        }
        // Without a game, looking once a second is enough and costs little.
        // The menu pointer must stop promptly when play begins.
        let pointing = any_map_running.as_ref().is_some_and(|(_, _, kind, ..)| *kind == any_map::Kind::Menu);
        thread::sleep(if idle { config.poll.max(IDLE_POLL) } else if pointing { config.poll.min(MENU_POLL) } else { config.poll });
    }
    if let Some(current) = running.take() {
        stop_child(current);
    }
    if let Some((_, _, _, stop, _)) = any_map_running.take() {
        stop.store(true, Ordering::Relaxed);
        // Its release of held keys runs on its own thread; give it a moment.
        thread::sleep(Duration::from_millis(50));
    }
    status.helper = None;
    publish(&Status { problem: Some("stopped".into()), ..status }, &mut written);
    Ok(())
}

/// What windows see: the service's state in the shared model.
/// `any_map` is the Any map output's focus while it runs.
pub fn snapshot(status: &Status, choice: model::ProfileChoice, profile: model::Profile, any_map: Option<bool>) -> model::Snapshot {
    model::Snapshot {
        pad: status.pad.as_ref().map(|pad| model::Pad {
            name: pad.name.clone(),
            id: pad.link.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        }),
        game: status.game.as_ref().map(|game| model::Game { pid: game.pid, window: matches!(game.target, Target::Window { .. }) }),
        session: status.session.as_ref().and_then(|session| session.shown.clone()),
        profile,
        choice,
        output: if let Some(focused) = any_map { model::Output { running: true, ready: true, focused } } else { model::Output {
            running: status.helper.is_some(),
            ready: status.helper.as_ref().is_some_and(|helper| helper.ready),
            // The helper reports focus changes; it starts focused until it says otherwise.
            focused: status.helper.as_ref().is_some_and(|helper| helper.focused.unwrap_or(true)),
        } },
        problem: (status.helper.is_none() && status.problem.is_some())
            .then(|| "Controller support hit a problem and is starting again.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(pid: u32, window: u32) -> Game {
        Game {
            pid,
            birth: u64::from(pid) * 10,
            documents: "/prefix/Documents/Warcraft III".into(),
            target: Target::Window { display: ":0".into(), x11_window: window, niri_window: 7, niri_socket: "/run/niri.sock".into() },
        }
    }
    fn pad() -> Pad {
        Pad { link: "/dev/input/by-id/usb-Microsoft-event-joystick".into(), device: "/dev/input/event4".into(), name: "Xbox".into() }
    }
    fn session(key: &str) -> Session {
        Session { key: key.into(), summary: key.into(), shown: None }
    }

    #[test]
    fn starts_only_with_game_pad_and_session() {
        let mut s = Supervisor::default();
        let now = Instant::now();
        assert_eq!(s.step(None, Some(&pad()), Some(&session("a")), now), Decision::Keep);
        assert_eq!(s.step(Some(&game(1, 5)), None, Some(&session("a")), now), Decision::Keep);
        assert_eq!(s.step(Some(&game(1, 5)), Some(&pad()), None, now), Decision::Keep);
        assert_eq!(s.step(Some(&game(1, 5)), Some(&pad()), Some(&session("a")), now), Decision::Start);
        s.started(&game(1, 5), &session("a"));
        assert_eq!(s.step(Some(&game(1, 5)), Some(&pad()), Some(&session("a")), now), Decision::Keep);
        // A pad unplugged while serving is the helper's to wait for.
        assert_eq!(s.step(Some(&game(1, 5)), None, Some(&session("a")), now), Decision::Keep);
    }

    #[test]
    fn rediscovers_a_restarted_game_or_a_new_window() {
        let mut s = Supervisor::default();
        let now = Instant::now();
        s.started(&game(1, 5), &session("a"));
        assert!(matches!(s.step(Some(&game(2, 5)), Some(&pad()), Some(&session("a")), now), Decision::Stop(reason) if reason.contains("restarted")));
        assert!(matches!(s.step(Some(&game(1, 6)), Some(&pad()), Some(&session("a")), now), Decision::Stop(reason) if reason.contains("window")));
        assert!(matches!(s.step(None, Some(&pad()), Some(&session("a")), now), Decision::Stop(reason) if reason.contains("closed")));
        // An exited helper is started again for the new game after a short wait.
        s.stopped(now);
        assert_eq!(s.step(Some(&game(2, 5)), Some(&pad()), Some(&session("a")), now), Decision::Keep);
        assert_eq!(s.step(Some(&game(2, 5)), Some(&pad()), Some(&session("a")), now + RETRY), Decision::Start);
    }

    #[test]
    fn follows_a_new_session_and_keeps_an_unchanged_one() {
        let mut s = Supervisor::default();
        let now = Instant::now();
        s.started(&game(1, 5), &session("a"));
        // A session not visible right now (the map closed) keeps the helper.
        assert_eq!(s.step(Some(&game(1, 5)), Some(&pad()), None, now), Decision::Keep);
        assert!(matches!(s.step(Some(&game(1, 5)), Some(&pad()), Some(&session("b")), now), Decision::Stop(reason) if reason.contains("new map session")));
    }

    #[test]
    fn replaces_a_helper_whose_pad_came_back_as_another_device() {
        let mut s = Supervisor::default();
        let now = Instant::now();
        s.started(&game(1, 5), &session("a"));
        s.event(HelperEvent::PadLost, now);
        assert_eq!(s.step(Some(&game(1, 5)), None, Some(&session("a")), now + PAD_SWAP), Decision::Keep);
        assert_eq!(s.step(Some(&game(1, 5)), Some(&pad()), Some(&session("a")), now + PAD_SWAP / 2), Decision::Keep);
        // The same pad reconnects by itself.
        s.event(HelperEvent::PadBack, now + PAD_SWAP / 2);
        assert_eq!(s.step(Some(&game(1, 5)), Some(&pad()), Some(&session("a")), now + PAD_SWAP * 2), Decision::Keep);
        s.event(HelperEvent::PadLost, now + PAD_SWAP * 2);
        assert!(matches!(s.step(Some(&game(1, 5)), Some(&pad()), Some(&session("a")), now + PAD_SWAP * 3), Decision::Stop(_)));
    }

    #[test]
    fn finds_warcraft_on_its_display_only() {
        let root = std::env::temp_dir().join(format!("service-proc-{}", std::process::id()));
        let process = |pid: u32, command: &str, display: &str, start: u64| {
            let dir = root.join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("cmdline"), format!("{command}\0-launch\0")).unwrap();
            fs::write(dir.join("environ"), format!("HOME=/home/tom\0DISPLAY={display}\0WINEPREFIX=/prefix/{pid}/\0")).unwrap();
            fs::write(dir.join("comm"), if command.ends_with(".exe") { "Warcraft III.ex\n" } else { "bash\n" }).unwrap();
            fs::write(dir.join("stat"), format!("{pid} (Warcraft III.ex) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 {start} 20")).unwrap();
        };
        process(10, "C:\\Program Files (x86)\\Warcraft III\\_retail_\\x86_64\\Warcraft III.exe", ":0", 500);
        process(11, "C:\\Program Files (x86)\\Warcraft III\\_retail_\\x86_64\\Warcraft III.exe", ":1", 600);
        process(12, "/usr/bin/bash", ":0", 700);
        assert_eq!(warcraft_processes(&root, ":0"), vec![(10, 500, PathBuf::from("/prefix/10/"))]);
        assert_eq!(warcraft_processes(&root, ":1").len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn picks_the_game_window_niri_reports_uniquely() {
        let windows: Value = serde_json::from_str(r#"[
            {"id": 38, "pid": 3140, "title": "Steam", "app_id": "steam"},
            {"id": 92, "pid": 3140, "title": "Warcraft III", "app_id": "steam_app_3516115571"},
            {"id": 4, "pid": 58013, "title": "r/warcraft3 - Google Chrome", "app_id": "google-chrome"}
        ]"#).unwrap();
        assert_eq!(niri_game_window(&windows, 2078193, "Warcraft III", "steam_app_3516115571"), Some(92));
        assert_eq!(niri_game_window(&windows, 2078193, "Warcraft III", "other"), None);
        assert_eq!(niri_game_window(&windows, 58013, "", ""), Some(4));
        let twice: Value = serde_json::from_str(r#"[
            {"id": 1, "pid": 3140, "title": "Warcraft III", "app_id": "steam_app_3516115571"},
            {"id": 2, "pid": 3140, "title": "Warcraft III", "app_id": "steam_app_3516115571"}
        ]"#).unwrap();
        assert_eq!(niri_game_window(&twice, 2078193, "Warcraft III", "steam_app_3516115571"), None);
    }

    #[test]
    fn finds_the_xbox_pad_by_its_stable_link() {
        let dir = std::env::temp_dir().join(format!("service-pads-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(find_pad(&dir), None);
        let target = dir.join("event9");
        fs::write(&target, "").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("usb-Generic_Pad-event-joystick")).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("usb-Microsoft_Controller_123-joystick")).unwrap();
        assert_eq!(find_pad(&dir).unwrap().link, dir.join("usb-Generic_Pad-event-joystick"));
        std::os::unix::fs::symlink(&target, dir.join("usb-Microsoft_Controller_123-event-joystick")).unwrap();
        let pad = find_pad(&dir).unwrap();
        assert_eq!(pad.link, dir.join("usb-Microsoft_Controller_123-event-joystick"));
        assert_eq!(pad.device, fs::canonicalize(&target).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
