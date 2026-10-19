use crate::{Hold, KeyOrClick, Transition};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::{collections::BTreeSet, time::Instant};

pub trait Output {
    fn apply(&mut self, transition: Transition) -> Result<(), String>;
    fn release_owned(&mut self) -> Result<(), String>;
}

/// Global keyboard delivery. The caller must independently establish foreground
/// eligibility before every press; this backend does not address a game window.
pub struct KeyboardOutput {
    enigo: Enigo,
    owned: BTreeSet<KeyOrClick>,
}

impl KeyboardOutput {
    pub fn new(display: Option<String>) -> Result<Self, String> {
        let settings = Settings {
            x11_display: display,
            ..Settings::default()
        };
        Ok(Self {
            enigo: Enigo::new(&settings).map_err(|e| e.to_string())?,
            owned: BTreeSet::new(),
        })
    }
}

impl Output for KeyboardOutput {
    fn apply(&mut self, transition: Transition) -> Result<(), String> {
        if self.owned.contains(&transition.action) == transition.pressed {
            return Ok(());
        }
        let key = Key::Unicode(transition.action.key());
        // Track before submission: an error can follow partial OS delivery.
        if transition.pressed {
            self.owned.insert(transition.action);
        }
        self.enigo
            .key(
                key,
                if transition.pressed {
                    Direction::Press
                } else {
                    Direction::Release
                },
            )
            .map_err(|e| e.to_string())?;
        if !transition.pressed {
            self.owned.remove(&transition.action);
        }
        Ok(())
    }

    fn release_owned(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        for action in self.owned.clone() {
            if let Err(error) = self.apply(Transition {
                action,
                pressed: false,
            }) {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl Drop for KeyboardOutput {
    fn drop(&mut self) {
        if let Err(error) = self.release_owned() {
            eprintln!("owned-key cleanup failed: {error}");
        }
    }
}

pub struct Held<O: Output> {
    pub output: O,
    hold: Hold,
}

impl<O: Output> Held<O> {
    pub fn new(output: O) -> Self {
        Self { output, hold: Hold::default() }
    }

    pub fn submit(&mut self, transition: Transition, now: Instant) -> Result<bool, String> {
        let out = self.hold.apply(vec![transition], now);
        let sent = out.contains(&transition);
        for transition in out {
            self.output.apply(transition)?;
        }
        Ok(sent)
    }

    pub fn due(&mut self, now: Instant) -> Result<(), String> {
        for transition in self.hold.due(now) {
            self.output.apply(transition)?;
        }
        Ok(())
    }

    pub fn next_due(&self) -> Option<Instant> {
        self.hold.next_due()
    }

    pub fn release_now(&mut self, transitions: Vec<Transition>) -> Result<(), String> {
        for transition in self.hold.release().into_iter().chain(transitions) {
            self.output.apply(transition)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MIN_HOLD;
    use std::time::Duration;

    #[derive(Default)]
    struct Recorded(Vec<Transition>);

    impl Output for Recorded {
        fn apply(&mut self, transition: Transition) -> Result<(), String> {
            self.0.push(transition);
            Ok(())
        }
        fn release_owned(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn the_emit_output_holds_a_one_frame_tap_two_frames_and_releases_it_at_once_on_disarm() {
        let start = Instant::now();
        let down = Transition { action: KeyOrClick::Jump, pressed: true };
        let up = Transition { action: KeyOrClick::Jump, pressed: false };
        let mut held = Held::new(Recorded::default());
        assert!(held.submit(down, start).unwrap());
        assert!(!held.submit(up, start + Duration::from_micros(16_667)).unwrap());
        held.due(start + MIN_HOLD - Duration::from_micros(1)).unwrap();
        assert_eq!(held.output.0, [down], "released before two frames");
        assert_eq!(held.next_due(), Some(start + MIN_HOLD));
        held.due(start + MIN_HOLD).unwrap();
        assert_eq!(held.output.0, [down, up]);
        let later = start + MIN_HOLD * 3;
        assert!(held.submit(down, later).unwrap());
        assert!(held.submit(up, later + Duration::from_millis(50)).unwrap());
        assert_eq!(held.output.0, [down, up, down, up], "a 50 ms press is released when the pad releases");
        assert!(held.submit(down, later + MIN_HOLD * 3).unwrap());
        assert!(!held.submit(up, later + MIN_HOLD * 3).unwrap());
        held.release_now(Vec::new()).unwrap();
        assert_eq!(held.output.0, [down, up, down, up, down, up], "disarm releases a held tap at once");
        assert_eq!(held.next_due(), None);
    }
}
