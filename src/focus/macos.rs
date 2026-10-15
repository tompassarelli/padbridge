//! The frontmost application's executable is named Warcraft III, optionally
//! pinned to one PID. Keyboard events posted to the HID stream reach it.
#![allow(unsafe_code)]
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};

pub struct Target {
    pub pid: Option<u32>,
}

pub struct Gate {
    target: Target,
}

const GAME: &str = "Warcraft III";

fn is_game(app: &NSRunningApplication) -> bool {
    app.executableURL()
        .and_then(|url| url.path())
        .is_some_and(|path| super::executable_matches(&path.to_string(), GAME))
}

fn refresh() {
    // NSWorkspace publishes activation changes only while the main run
    // loop runs; this helper owns no Cocoa event loop, so drain it here.
    // SAFETY: reads an immutable CoreFoundation constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    CFRunLoop::run_in_mode(mode, 0.0, false);
}

impl Gate {
    pub fn new(target: Target) -> Result<Self, String> {
        if let Some(pid) = target.pid {
            let pid = i32::try_from(pid).map_err(|_| "invalid PID")?;
            let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                .ok_or("selected PID is not a running application")?;
            if !is_game(&app) {
                return Err("selected PID does not identify Warcraft III".into());
            }
        }
        Ok(Self { target })
    }
}

impl super::Foreground for Gate {
    fn eligible(&mut self) -> Result<bool, String> {
        refresh();
        let Some(app) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
            return Ok(false);
        };
        let pid = app.processIdentifier();
        if self
            .target
            .pid
            .is_some_and(|selected| i32::try_from(selected) != Ok(pid))
        {
            return Ok(false);
        }
        Ok(is_game(&app))
    }
}
