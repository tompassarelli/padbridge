//! Test-only stand-in for the Warcraft III window (`--features e2e`). It logs
//! every keyboard and focus event its window receives, one line per event:
//! `SDL_TIMESTAMP_NS<TAB>KIND<TAB>DETAIL`. Stdin `raise` asks SDL to bring the
//! window forward; `quit` or end of input exits.
#![forbid(unsafe_code)]

use sdl3::event::{Event, WindowEvent};
use std::{
    fs::File,
    io::{BufRead, Write},
    sync::mpsc,
    time::Duration,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(title), Some(log)) = (args.next(), args.next()) else {
        eprintln!("usage: padbridge-standin TITLE LOG");
        std::process::exit(2);
    };
    if let Err(error) = run(&title, &log) {
        eprintln!("padbridge-standin: {error}");
        std::process::exit(1);
    }
}

/// SDL event timestamps are nanoseconds since SDL initialization; these lines
/// use the same clock at millisecond resolution.
fn now_ns() -> u64 {
    sdl3::timer::ticks() * 1_000_000
}

fn run(title: &str, path: &str) -> Result<(), String> {
    // Windows otherwise refuses foreground changes requested by a background process.
    sdl3::hint::set("SDL_FORCE_RAISEWINDOW", "1");
    let sdl = sdl3::init().map_err(|e| e.to_string())?;
    let video = sdl.video().map_err(|e| e.to_string())?;
    let mut window = video
        .window(title, 360, 200)
        .position_centered()
        .build()
        .map_err(|e| e.to_string())?;
    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    let mut log = File::create(path).map_err(|e| e.to_string())?;
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
    let mut record = |timestamp: u64, kind: &str, detail: &str| -> Result<(), String> {
        writeln!(log, "{timestamp}\t{kind}\t{detail}")
            .and_then(|()| log.flush())
            .map_err(|e| e.to_string())
    };
    record(now_ns(), "ready", &std::process::id().to_string())?;
    loop {
        match commands.try_recv().as_deref() {
            Ok("raise") => {
                let raised = window.raise();
                record(now_ns(), "raise", &raised.to_string())?;
            }
            Ok("quit") | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            Ok(other) => return Err(format!("unknown command {other:?}")),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Some(event) = events.wait_event_timeout(Duration::from_millis(10)) {
            for event in std::iter::once(event).chain(events.poll_iter()) {
                let key = |keycode: Option<sdl3::keyboard::Keycode>| {
                    keycode.map_or_else(|| "unknown".into(), |k| k.name().to_lowercase())
                };
                match event {
                    Event::KeyDown {
                        timestamp,
                        keycode,
                        repeat,
                        ..
                    } => record(
                        timestamp,
                        if repeat { "key_repeat" } else { "key_down" },
                        &key(keycode),
                    )?,
                    Event::KeyUp {
                        timestamp, keycode, ..
                    } => record(timestamp, "key_up", &key(keycode))?,
                    Event::Window {
                        timestamp,
                        win_event: WindowEvent::FocusGained,
                        ..
                    } => record(timestamp, "focus_gained", "")?,
                    Event::Window {
                        timestamp,
                        win_event: WindowEvent::FocusLost,
                        ..
                    } => record(timestamp, "focus_lost", "")?,
                    _ => {}
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
