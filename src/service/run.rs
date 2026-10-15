use super::{
    any_map,
    discovery::{PadDiscovery, find_game, find_pad, headless_game, process_birth},
    interface, settings,
    profile::{Game, Helper, HelperEvent, Profile, Target},
    status::{self, HelperStatus, Status, snapshot},
    supervisor::{Decision, IDLE_POLL, MENU_POLL, Running, Supervisor, output_target, stop_child},
};
use crate::model;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub struct Config {
    pub settings_file: PathBuf,
    /// The X11 display Warcraft III runs on.
    pub display: String,
    /// Where the pads' stable links are.
    pub pads: PathBuf,
    /// Rewritten on every change; absent writes none.
    pub status_file: Option<PathBuf>,
    pub poll: Duration,
    /// A stand-in game with no window: its Documents folder. Its `game` file
    /// names the game; changing it is a restart. The helper types into `typed.txt` there.
    pub headless: Option<PathBuf>,
    /// The local interface's address (interface::ADDRESS); absent opens none.
    pub interface: Option<String>,
    /// The Any map profile's bindings until a window sends others; absent is the built-in layout.
    pub layout: Option<Vec<model::Binding>>,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self {
            settings_file: settings::default_path(),
            display: ":0".into(),
            pads: "/dev/input/by-id".into(),
            status_file: Some(home.join(".local/state/padbridge/service.txt")),
            poll: Duration::from_millis(250),
            headless: None,
            interface: Some(interface::ADDRESS.into()),
            layout: None,
        }
    }
}

/// The lock one service holds for its display (or stand-in game), so a
/// second copy never runs a second helper typing into the same game.
pub fn lock_path(config: &Config) -> PathBuf {
    match &config.headless {
        Some(documents) => documents.join("padbridge.lock"),
        None => {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            runtime.join(format!("padbridge-{}.lock", config.display.replace(['/', ':'], "")))
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
    let mut settings = settings::load(&config.settings_file)?;
    let mut bindings = config.layout.clone().unwrap_or_else(any_map::default_bindings);
    // The running Any map output's feed; the pad watcher sends every state to it.
    let feed: Arc<Mutex<Option<mpsc::Sender<any_map::Feed>>>> = Arc::new(Mutex::new(None));
    let mut any_map_running: Option<(any_map::Window, u32, any_map::Kind, Arc<AtomicBool>, Arc<AtomicBool>)> = None;
    let mut watching: Option<(PathBuf, Arc<AtomicBool>)> = None;
    let mut discovery = PadDiscovery::default();
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
            status::write(path, &text);
        }
        report(status);
        *written = text;
    };
    eprintln!("service: profile={} display={} pads={}", profile.name(), config.display, config.pads.display());
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if let Some(current) = &mut running {
            while let Ok(line) = current.lines.try_recv() {
                eprintln!("helper: {line}");
                let meaning = profile.line(&line);
                if meaning.problem.is_some() {
                    status.problem = meaning.problem;
                }
                if let (Some(event), Some(helper)) = (meaning.event, status.helper.as_mut()) {
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
                    if let Some(problem) = profile.line(&line).problem {
                        status.problem = Some(problem);
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
        let pad = discovery.update(find_pad(&config.pads), now);
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
                    message @ (model::ClientMessage::TapJump(_) | model::ClientMessage::PadPreset(_) | model::ClientMessage::TriggerShields(_) | model::ClientMessage::RightStick(_) | model::ClientMessage::Remaps(_)) => {
                        if settings.apply(&message) {
                            settings::save(&config.settings_file, &settings)?;
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
        let session = if resolved == model::Profile::Map { session } else { None };
        // A session on keys needs no helper: the pad's keys output serves it.
        let keys = resolved == model::Profile::Map && session.is_some() && profile.keys();
        if resolved != model::Profile::Map || keys {
            if let Some(current) = running.take() {
                eprintln!("service: stopping the helper: {}", if keys { "the session plays on keys".to_owned() } else { format!("profile {resolved:?}") });
                stop_child(current);
                status.helper = None;
                supervisor = Supervisor::default();
            }
        }
        // Pad output into the game's window while it has focus: the Any map
        // profile, or the pointer in the map's menus (never during play).
        let menu = resolved == model::Profile::Map && profile.pointer_menu()
            && status.helper.as_ref().is_none_or(|helper| !helper.in_match);
        let kind = if menu { Some(any_map::Kind::Menu { start_on_keys: keys }) } else if keys { Some(any_map::Kind::Keys) } else if resolved == model::Profile::AnyMap { Some(any_map::Kind::AnyMap) } else { None };
        let want = output_target(game.as_ref(), pad.as_ref(), kind);
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
                    any_map::Kind::Menu { start_on_keys } => any_map::Mode::Menu { start_on_keys },
                    any_map::Kind::Keys => any_map::Mode::Keys(settings.clone()),
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
                let helper = profile.helper(game, pad, session).unwrap_or_else(|| Helper { program: PathBuf::new(), args: Vec::new() });
                let args = &helper.args;
                let mut command = Command::new(&helper.program);
                command.args(args).stdin(Stdio::null()).stderr(Stdio::piped());
                command.args(["--preset", settings.pad_preset.name()]);
                command.args(["--tap-jump", if settings.tap_jump { "on" } else { "off" }]);
                command.args(["--left-trigger", settings.triggers.left.name(), "--right-trigger", settings.triggers.right.name()]);
                command.args(["--right-stick", settings.right_stick.name()]);
                if !settings.remaps.is_empty() {
                    command.args(["--remaps".to_owned(), model::remaps_arg(&settings.remaps)]);
                }
                command.env("WC3_SERVICE_PID", std::process::id().to_string());
                if let Target::Window { niri_socket, display, .. } = &game.target {
                    command.env("NIRI_SOCKET", niri_socket).env("DISPLAY", display);
                }
                match if helper.program.as_os_str().is_empty() { Err(std::io::Error::other("the map plug-in names none")) } else { command.spawn() } {
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
                        status.problem = Some(format!("couldn't start the helper {}: {error}", helper.program.display()));
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
            let mut snapshot = snapshot(&status, choice, resolved, any_map);
            snapshot.settings = settings.clone();
            snapshot.map = Some(profile.title()).filter(|title| !title.is_empty()).map(str::to_owned);
            interface.status(&snapshot);
        }
        // Without a game, looking once a second is enough and costs little.
        // The menu pointer must stop promptly when play begins.
        let pointing = any_map_running.as_ref().is_some_and(|(_, _, kind, ..)| matches!(kind, any_map::Kind::Menu { .. }));
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
