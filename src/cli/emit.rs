use super::{
    history::{clock_ns, event_control, event_timestamp, history_event, history_transition, next_event_id, sample},
    options::{options, target},
    virtual_pad::VirtualPad,
};
#[cfg(target_os = "linux")]
use super::analog::AnalogSession;
use padbridge::{
    EventMapper,
    focus::{self, Foreground},
    output::{Held, KeyboardOutput, Output},
    selected_input,
};
use sdl3::{event::Event, joystick::JoystickId};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub fn run() -> Result<(), String> {
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
    #[cfg(target_os = "linux")]
    let route = padbridge::pad_ingress::Route::parse(o.pad_ingress.as_deref().unwrap_or("digital"), o.cursor_grid.as_deref())?;
    #[cfg(target_os = "linux")]
    if let Some(end) = o.cursor_calibrate {
        use padbridge::pad_ingress::{CursorGrid, calibrate};
        let grid = CursorGrid::parse(o.cursor_grid.as_deref().ok_or("calibration requires --cursor-grid")?)?;
        let mut out = padbridge::service::any_map::DesktopOutput::new(o.display.as_deref().ok_or("missing display")?)?;
        let start = Instant::now();
        let mut held = false;
        let calibrating = Arc::new(AtomicBool::new(true));
        let signal_calibrating = Arc::clone(&calibrating);
        ctrlc::set_handler(move || signal_calibrating.store(false, Ordering::Relaxed)).map_err(|e| e.to_string())?;
        let result = (|| -> Result<(), String> {
            while calibrating.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(o.seconds.ok_or("missing duration")?) {
                let eligible = gate.as_mut().ok_or("missing focus gate")?.eligible()?;
                if eligible != held {
                    calibrate(grid, end, eligible, &mut out)?;
                    held = eligible;
                }
                std::thread::sleep(Duration::from_millis(4));
            }
            Ok(())
        })();
        let cleanup = calibrate(grid, end, false, &mut out);
        return result.and(cleanup);
    }
    #[cfg(target_os = "linux")]
    let mut analog = if o.emit && route != padbridge::pad_ingress::Route::Digital {
        Some(AnalogSession {
            ingress: padbridge::pad_ingress::Ingress::new(route),
            out: padbridge::service::any_map::DesktopOutput::new(o.display.as_deref().ok_or("missing display")?)?,
        })
    } else { None };
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
            "gamepad id={id} name={:?} kind={:?} path={:?} vendor={:?} product={:?}\nmapping={:?}\nnormalized={:?}",
            pad.name(),
            padbridge::pad_kind(*id),
            pad.path(),
            pad.vendor_id(),
            pad.product_id(),
            pad.mapping(),
            sample(&pad)
        );
    }
    listing!("presets={}", padbridge::model::PadPreset::ALL.map(|preset| preset.name()).join(","));
    let Some(seconds) = o.seconds else {
        return Ok(());
    };
    let id = match o.gamepad {
        Some(id) if ids.contains(&JoystickId::from(id)) => JoystickId::from(id),
        Some(_) => return Err("selected gamepad is not connected".into()),
        None if virtual_pad.is_some() => virtual_pad.as_ref()
            .and_then(|pad| pad.attached.as_ref().map(|(_, connection)| connection.id()))
            .ok_or("virtual pad is not connected")?,
        None if ids.len() == 1 => ids[0],
        None => {
            return Err("watch requires exactly one gamepad or an explicit --gamepad ID".into());
        }
    };
    let pad = gamepads.open(id).map_err(|e| e.to_string())?;
    let mut keyboard = if o.emit {
        Some(Held::new(KeyboardOutput::new(o.display)?))
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
    let mut mapper = EventMapper::new(o.preset);
    mapper.set_tap_jump(o.tap_jump);
    mapper.set_right_stick(o.right_stick);
    mapper.set_trigger_shields(o.triggers);
    mapper.set_remaps(o.remaps.clone());
    mapper.set_pad_kind(padbridge::pad_kind(id));
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
            if let Some(keyboard) = &mut keyboard {
                keyboard.release_now(release)?;
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(analog) = &mut analog {
            analog.step(mapper.sample(), eligible && mapper.armed() && pad.connected())?;
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
                            let release = mapper.disarm();
                            if let Some(keyboard) = &mut keyboard {
                                keyboard.release_now(release)?;
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
                        let sent = match &mut keyboard {
                            Some(keyboard) => keyboard.submit(transition, Instant::now())?,
                            None => true,
                        };
                        history_transition(
                            event_id,
                            input.capture_ns,
                            dequeued_ns,
                            submit_ns,
                            control,
                            &value,
                            transition.action,
                            transition.pressed,
                            if !sent { "held" } else if o.emit { "submitted" } else { "preview" },
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
                #[cfg(target_os = "linux")]
                if let Some(analog) = &mut analog {
                    analog.step(mapper.sample(), event_eligible && mapper.armed())?;
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
                if let Some(keyboard) = &mut keyboard {
                    keyboard.release_now(release)?;
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
                if let Some(keyboard) = &mut keyboard {
                    keyboard.release_now(release)?;
                }
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(analog) = &mut analog {
            analog.step(mapper.sample(), last_eligibility == Some(true) && mapper.armed() && pad.connected())?;
        }
        let tick = Duration::from_millis(4);
        let wait = match &mut keyboard {
            Some(keyboard) => {
                keyboard.due(Instant::now())?;
                keyboard.next_due().map_or(tick, |due| due.saturating_duration_since(Instant::now()).min(tick))
            }
            None => tick,
        };
        std::thread::sleep(wait);
    }
    if let Some(keyboard) = &mut keyboard {
        keyboard.release_now(Vec::new())?;
    }
    for transition in mapper.disarm() {
        if let Some(keyboard) = &mut keyboard {
            keyboard.output.apply(transition)?;
        }
        eprintln!(
            "# shutdown\t{}\t{}",
            transition.action.key(),
            transition.pressed
        );
    }
    if let Some(keyboard) = &mut keyboard {
        keyboard.output.release_owned()?;
    }
    Ok(())
}

