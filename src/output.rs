use crate::{KeyOrClick, Transition};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::collections::BTreeSet;

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
