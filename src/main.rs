// Unsafe code is confined to the Windows/macOS foreground adapters in focus.rs.
#![deny(unsafe_code)]
mod focus;
#[cfg(target_os = "linux")]
mod wlr;

use focus::Foreground;
use sdl3::{
    JoystickSubsystem,
    event::Event,
    gamepad::{Axis, Button, Gamepad},
    joystick::{
        Joystick, JoystickId, JoystickType, VirtualJoystickConnection, VirtualJoystickDescription,
    },
};
use std::{
    io::BufRead,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use wc3_controller::{
    EventMapper, Sample,
    output::{KeyboardOutput, Output},
    selected_input,
};

#[derive(Default)]
struct Options {
    seconds: Option<u64>,
    gamepad: Option<u32>,
    emit: bool,
    display: Option<String>,
    window: Option<u32>,
    pid: Option<u32>,
    niri_window: Option<u64>,
    wlr_app_id: Option<String>,
    check_focus: bool,
    virtual_pad: bool,
}

#[cfg(target_os = "linux")]
const HELP: &str = "wc3-controller [--list] [--watch-seconds N] [--gamepad ID]\n\
                    Observation only by default. Live output additionally requires:\n\
                    --emit --display DISPLAY --x11-window DECIMAL_ID --pid PID --niri-window ID\n\
                    Or --private-wlr-app-id ID in the isolated labwc test desktop instead of --niri-window.\n\
                    --check-focus checks selected target without opening keyboard output. Windows/macOS refuse live output.";

#[cfg(not(target_os = "linux"))]
const HELP: &str = "wc3-controller [--list] [--watch-seconds N] [--gamepad ID]\n\
                    Observation only by default. --emit sends keys only while Warcraft III is the\n\
                    foreground application; add --pid PID to require one game process.\n\
                    --check-focus checks the foreground game without opening keyboard output.\n\
                    --virtual-pad (testing) replaces hardware with an SDL virtual gamepad driven by stdin.";

#[cfg(target_os = "linux")]
fn target(o: &Options) -> Result<focus::Target, String> {
    Ok(focus::Target {
        display: o.display.clone().ok_or("--emit requires --display")?,
        window: o.window.ok_or("--emit requires --x11-window")?,
        pid: o.pid.ok_or("--emit requires --pid")?,
        niri_window: o.niri_window,
        wlr_app_id: o.wlr_app_id.clone(),
    })
}

#[cfg(not(target_os = "linux"))]
fn target(o: &Options) -> Result<focus::Target, String> {
    if o.display.is_some()
        || o.window.is_some()
        || o.niri_window.is_some()
        || o.wlr_app_id.is_some()
    {
        return Err(
            "--display, --x11-window, --niri-window and --private-wlr-app-id select Linux desktops"
                .into(),
        );
    }
    Ok(focus::Target { pid: o.pid })
}

// Ascending SDL enum order: SDL numbers a virtual gamepad's controls that way.
const VIRTUAL_BUTTONS: [Button; 7] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::Start,
    Button::LeftShoulder,
    Button::RightShoulder,
];
const VIRTUAL_AXES: [Axis; 6] = [
    Axis::LeftX,
    Axis::LeftY,
    Axis::RightX,
    Axis::RightY,
    Axis::TriggerLeft,
    Axis::TriggerRight,
];

/// Test seam: an SDL virtual gamepad inside this process, so SDL's own gamepad
/// event path carries scripted input exactly as it carries a physical pad's.
/// Stdin lines use SDL's control names: `button a|b|x|y|start|leftshoulder|rightshoulder 0|1`,
/// `axis leftx|lefty|rightx|righty|lefttrigger|righttrigger RAW` (raw joystick
/// units; SDL maps triggers from -32768..32767 to 0..32767), `detach`, `quit`.
struct VirtualPad {
    attached: Option<(Joystick, VirtualJoystickConnection)>,
    commands: mpsc::Receiver<String>,
}

impl VirtualPad {
    fn attach(joysticks: &JoystickSubsystem) -> Result<Self, String> {
        let desc = VirtualJoystickDescription::new()
            .name("Smashcraft virtual pad")
            .joystick_type(JoystickType::Gamepad)
            .with_buttons(VIRTUAL_BUTTONS)
            .with_axes(VIRTUAL_AXES);
        let connection = joysticks
            .attach_virtual_joystick(desc)
            .map_err(|e| e.to_string())?;
        let joystick = joysticks.open(connection.id()).map_err(|e| e.to_string())?;
        // A physical pad reports its resting state on connect, and SDL recenters
        // each axis to its first report on disconnect. Report rest the same way.
        for (axis, rest) in [0, 0, 0, 0, i16::MIN, i16::MIN].into_iter().enumerate() {
            joystick
                .set_virtual_axis(axis as u32, rest)
                .map_err(|e| e.to_string())?;
        }
        let (sender, commands) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    return;
                }
            }
            let _ = sender.send("quit".into());
        });
        Ok(Self {
            attached: Some((joystick, connection)),
            commands,
        })
    }

    /// Apply at most one command, so each state change reaches SDL's event
    /// queue in its own pump rather than collapsing with the next one.
    fn step(&mut self, running: &AtomicBool) -> Result<(), String> {
        let Ok(line) = self.commands.try_recv() else {
            return Ok(());
        };
        let words: Vec<_> = line.split_whitespace().collect();
        match words.as_slice() {
            ["quit"] => running.store(false, Ordering::Relaxed),
            ["detach"] => {
                // Close this extra handle before SDL_DetachVirtualJoystick runs.
                if let Some((joystick, connection)) = self.attached.take() {
                    drop(joystick);
                    drop(connection);
                }
            }
            ["button", name, value] => {
                let button = VIRTUAL_BUTTONS
                    .iter()
                    .position(|button| button.string() == *name)
                    .ok_or_else(|| format!("unknown virtual button {name}"))?;
                if let Some((joystick, _)) = &self.attached {
                    joystick
                        .set_virtual_button(button as u32, *value == "1")
                        .map_err(|e| e.to_string())?;
                }
            }
            ["axis", name, value] => {
                let axis = VIRTUAL_AXES
                    .iter()
                    .position(|axis| axis.string() == *name)
                    .ok_or_else(|| format!("unknown virtual axis {name}"))?;
                let value = value.parse().map_err(|_| "invalid virtual axis value")?;
                if let Some((joystick, _)) = &self.attached {
                    joystick
                        .set_virtual_axis(axis as u32, value)
                        .map_err(|e| e.to_string())?;
                }
            }
            _ => return Err(format!("unknown virtual pad command {line:?}")),
        }
        Ok(())
    }
}

fn options() -> Result<Options, String> {
    let mut o = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            "--list" => {}
            "--emit" => o.emit = true,
            "--check-focus" => o.check_focus = true,
            "--virtual-pad" => o.virtual_pad = true,
            "--private-wlr-app-id" => {
                o.wlr_app_id = Some(args.next().ok_or("missing private compositor app ID")?)
            }
            "--watch-seconds" => {
                o.seconds = Some(
                    args.next()
                        .ok_or("missing seconds")?
                        .parse()
                        .map_err(|_| "invalid seconds")?,
                )
            }
            "--gamepad" => {
                o.gamepad = Some(
                    args.next()
                        .ok_or("missing gamepad ID")?
                        .parse()
                        .map_err(|_| "invalid gamepad ID")?,
                )
            }
            "--display" => o.display = Some(args.next().ok_or("missing display")?),
            "--x11-window" => {
                o.window = Some(
                    args.next()
                        .ok_or("missing X11 window")?
                        .parse()
                        .map_err(|_| "invalid decimal X11 window")?,
                )
            }
            "--pid" => {
                o.pid = Some(
                    args.next()
                        .ok_or("missing PID")?
                        .parse()
                        .map_err(|_| "invalid PID")?,
                )
            }
            "--niri-window" => {
                o.niri_window = Some(
                    args.next()
                        .ok_or("missing Niri window")?
                        .parse()
                        .map_err(|_| "invalid Niri window")?,
                )
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if o.emit && o.seconds.is_none() {
        return Err("--emit requires a bounded --watch-seconds duration".into());
    }
    Ok(o)
}

fn sample(pad: &Gamepad) -> Sample {
    Sample {
        left_x: pad.axis(Axis::LeftX),
        left_y: pad.axis(Axis::LeftY),
        right_x: pad.axis(Axis::RightX),
        right_y: pad.axis(Axis::RightY),
        left_trigger: pad.axis(Axis::TriggerLeft),
        right_trigger: pad.axis(Axis::TriggerRight),
        a: pad.button(Button::South),
        b: pad.button(Button::East),
        x: pad.button(Button::West),
        y: pad.button(Button::North),
        lb: pad.button(Button::LeftShoulder),
        rb: pad.button(Button::RightShoulder),
        start: pad.button(Button::Start),
    }
}

fn event_control(input: wc3_controller::CapturedInput) -> (&'static str, String) {
    use wc3_controller::{Control, InputValue};
    match (input.control, input.value) {
        (Control::Axis(axis), InputValue::Axis(value)) => ("axis", format!("{axis:?}={value}")),
        (Control::Button(button), InputValue::Button(value)) => {
            ("button", format!("{button:?}={value}"))
        }
        _ => ("unknown", "unknown".into()),
    }
}

fn clock_ns(start: Instant) -> u128 {
    start.elapsed().as_nanos()
}

fn next_event_id(next: &mut u64) -> u64 {
    *next = next.checked_add(1).expect("controller event ID exhausted");
    *next
}

fn event_timestamp(event: &Event) -> u64 {
    match event {
        Event::GamepadRemoved { timestamp, .. } | Event::GamepadRemapped { timestamp, .. } => {
            *timestamp
        }
        _ => 0,
    }
}

fn history_event(
    id: u64,
    capture_ns: u64,
    dequeue_ns: u128,
    control: &str,
    value: &str,
    disposition: &str,
) {
    println!("event\t{id}\t{capture_ns}\t{dequeue_ns}\t\t{control}\t{value}\t\t\t{disposition}");
}

fn history_transition(
    id: u64,
    capture_ns: u64,
    dequeue_ns: u128,
    submit_ns: Option<u128>,
    control: &str,
    value: &str,
    action: wc3_controller::Action,
    pressed: bool,
    disposition: &str,
) {
    let submit_ns = submit_ns.map_or_else(String::new, |value| value.to_string());
    println!(
        "transition\t{id}\t{capture_ns}\t{dequeue_ns}\t{submit_ns}\t{control}\t{value}\t{action:?}\t{pressed}\t{disposition}"
    );
}

fn run() -> Result<(), String> {
    let o = options()?;
    macro_rules! listing {
        ($($arg:tt)*) => {
            if o.seconds.is_some() { eprintln!($($arg)*); } else { println!($($arg)*); }
        };
    }
    let mut gate = if o.emit || o.check_focus {
        Some(focus::Gate::new(target(&o)?)?)
    } else {
        None
    };

    if o.check_focus {
        println!(
            "game-eligible={} (read-only; no keyboard backend opened)",
            gate.as_mut()
                .ok_or("missing foreground adapter")?
                .eligible()?
        );
        return Ok(());
    }
    if !sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1") {
        return Err("SDL refused background controller acquisition".into());
    }
    let sdl = sdl3::init().map_err(|e| e.to_string())?;
    let gamepads = sdl.gamepad().map_err(|e| e.to_string())?;
    let mut virtual_pad = if o.virtual_pad {
        Some(VirtualPad::attach(
            &sdl.joystick().map_err(|e| e.to_string())?,
        )?)
    } else {
        None
    };
    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    events.pump_events();
    let ids = gamepads.gamepads().map_err(|e| e.to_string())?;
    listing!(
        "mode={} SDL={} gamepads={}",
        if o.emit {
            "keyboard-output"
        } else {
            "observation-only"
        },
        sdl3::version::version(),
        ids.len()
    );
    for id in &ids {
        let pad = gamepads.open(*id).map_err(|e| e.to_string())?;
        listing!(
            "gamepad id={id} name={:?} path={:?} vendor={:?} product={:?}\nmapping={:?}\nnormalized={:?}",
            pad.name(),
            pad.path(),
            pad.vendor_id(),
            pad.product_id(),
            pad.mapping(),
            sample(&pad)
        );
    }
    let Some(seconds) = o.seconds else {
        return Ok(());
    };
    let id = match o.gamepad {
        Some(id) if ids.contains(&JoystickId::from(id)) => JoystickId::from(id),
        Some(_) => return Err("selected gamepad is not connected".into()),
        None if ids.len() == 1 => ids[0],
        None => {
            return Err("watch requires exactly one gamepad or an explicit --gamepad ID".into());
        }
    };
    let pad = gamepads.open(id).map_err(|e| e.to_string())?;
    let mut keyboard = if o.emit {
        Some(KeyboardOutput::new(o.display)?)
    } else {
        None
    };
    let running = Arc::new(AtomicBool::new(true));
    let signal_running = Arc::clone(&running);
    ctrlc::set_handler(move || signal_running.store(false, Ordering::Relaxed))
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    println!(
        "record\tevent_id\tcapture_ns\tdequeue_ns\tsubmit_ns\tcontrol\tvalue\taction\tpressed\tdisposition"
    );
    println!(
        "# capture_ns is SDL's event timestamp since SDL initialization; dequeue_ns and submit_ns are process-monotonic since watch start"
    );
    let mut mapper = EventMapper::default();
    let mut next_id = 0;
    let startup_events = events.poll_iter().count();
    let initial = sample(&pad);
    mapper.resync(&initial, true);
    let mut last_eligibility = None;
    let mut last_error: Option<String> = None;
    println!(
        "# selected={id}; discarded_startup_events={startup_events}; release all mapped controls to arm; transitions are previews only unless --emit"
    );
    while running.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(seconds) {
        if let Some(virtual_pad) = &mut virtual_pad {
            virtual_pad.step(&running)?;
        }
        let dequeued: Vec<_> = events
            .poll_iter()
            .map(|event| {
                let dequeued_ns = clock_ns(start);
                if matches!(&event, Event::Quit { .. }) {
                    running.store(false, Ordering::Relaxed);
                }
                (event, dequeued_ns)
            })
            .collect();
        let mut eligible = true;
        if let Some(gate) = &mut gate {
            match gate.eligible() {
                Ok(value) => {
                    eligible = value;
                    last_error = None;
                }
                Err(error) => {
                    eligible = false;
                    if last_error.as_ref() != Some(&error) {
                        eprintln!("eligibility unavailable: {error}");
                    }
                    last_error = Some(error);
                }
            }
        }
        let recovering = last_eligibility == Some(false);
        if !eligible || recovering || !mapper.armed() {
            let release = if eligible && pad.connected() {
                mapper.resync(&sample(&pad), true)
            } else {
                mapper.disarm()
            };
            for transition in release {
                if let Some(keyboard) = &mut keyboard {
                    keyboard.apply(transition)?;
                }
            }
        }
        if last_eligibility != Some(eligible) {
            let reason = gate
                .as_ref()
                .and_then(|gate| gate.away())
                .filter(|_| !eligible)
                .map(|reason| format!(" reason={reason}"))
                .unwrap_or_default();
            eprintln!(
                "t_us={} {}={eligible}{reason}",
                start.elapsed().as_micros(),
                if o.emit {
                    "game-eligible"
                } else {
                    "preview-enabled"
                }
            );
            last_eligibility = Some(eligible);
        }
        for (event, dequeued_ns) in dequeued {
            if let Some(input) = selected_input(&event, id) {
                let event_id = next_event_id(&mut next_id);
                let (control, value) = event_control(input);
                let suppression = recovering || !eligible;
                let transitions = if suppression {
                    mapper.disarm()
                } else {
                    mapper.apply(input, true)
                };
                let disposition = if suppression {
                    "suppressed"
                } else {
                    "observed"
                };
                history_event(
                    event_id,
                    input.capture_ns,
                    dequeued_ns,
                    control,
                    &value,
                    disposition,
                );
                let mut event_eligible = !suppression;
                for transition in transitions {
                    if event_eligible {
                        let still_eligible = match &mut gate {
                            Some(gate) => match gate.eligible() {
                                Ok(value) => value,
                                Err(error) => {
                                    if last_error.as_ref() != Some(&error) {
                                        eprintln!("eligibility unavailable: {error}");
                                    }
                                    last_error = Some(error);
                                    false
                                }
                            },
                            None => true,
                        };
                        if !still_eligible {
                            event_eligible = false;
                            last_eligibility = Some(false);
                            for release in mapper.disarm() {
                                if let Some(keyboard) = &mut keyboard {
                                    keyboard.apply(release)?;
                                }
                            }
                            history_transition(
                                event_id,
                                input.capture_ns,
                                dequeued_ns,
                                None,
                                control,
                                &value,
                                transition.action,
                                transition.pressed,
                                "suppressed-focus",
                            );
                            continue;
                        }
                        let submit_ns = o.emit.then(|| clock_ns(start));
                        if let Some(keyboard) = &mut keyboard {
                            keyboard.apply(transition)?;
                        }
                        history_transition(
                            event_id,
                            input.capture_ns,
                            dequeued_ns,
                            submit_ns,
                            control,
                            &value,
                            transition.action,
                            transition.pressed,
                            if o.emit { "submitted" } else { "preview" },
                        );
                    } else {
                        history_transition(
                            event_id,
                            input.capture_ns,
                            dequeued_ns,
                            None,
                            control,
                            &value,
                            transition.action,
                            transition.pressed,
                            "suppressed",
                        );
                    }
                }
            } else if matches!(&event, Event::GamepadRemoved { which, .. } if *which == id) {
                let event_id = next_event_id(&mut next_id);
                let release = mapper.disarm();
                history_event(
                    event_id,
                    event_timestamp(&event),
                    dequeued_ns,
                    "device",
                    "removed",
                    "disconnected",
                );
                for transition in release {
                    if let Some(keyboard) = &mut keyboard {
                        keyboard.apply(transition)?;
                    }
                }
            } else if matches!(&event, Event::GamepadRemapped { which, .. } if *which == id) {
                let event_id = next_event_id(&mut next_id);
                let release = mapper.disarm();
                history_event(
                    event_id,
                    event_timestamp(&event),
                    dequeued_ns,
                    "device",
                    "remapped",
                    "rearm-required",
                );
                for transition in release {
                    if let Some(keyboard) = &mut keyboard {
                        keyboard.apply(transition)?;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    for transition in mapper.disarm() {
        if let Some(keyboard) = &mut keyboard {
            keyboard.apply(transition)?;
        }
        eprintln!(
            "# shutdown\t{}\t{}",
            transition.action.key(),
            transition.pressed
        );
    }
    if let Some(keyboard) = &mut keyboard {
        keyboard.release_owned()?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("wc3-controller: {error}");
        std::process::exit(1);
    }
}
