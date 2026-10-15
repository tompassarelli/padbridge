use serde::{Deserialize, Serialize};

use crate::{Binding, PadPreset, ProfileChoice, Remaps, RightStick, Snapshot, TriggerShields};

/// Live pad state for display. Axes are SDL-normalized: -32768..=32767, and a
/// stick's y is negative when pushed up. Triggers are 0..=32767.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputView {
    pub left: [i16; 2],
    pub right: [i16; 2],
    pub lt: u16,
    pub rt: u16,
    /// One bit per [`Button`], at `1 << (button as u32)`.
    pub buttons: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    A,
    B,
    X,
    Y,
    Lb,
    Rb,
    Start,
    Back,
    LeftStick,
    RightStick,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl Button {
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }
}

impl InputView {
    pub fn pressed(&self, button: Button) -> bool {
        self.buttons & button.bit() != 0
    }
    pub fn press(&mut self, button: Button, down: bool) {
        if down {
            self.buttons |= button.bit();
        } else {
            self.buttons &= !button.bit();
        }
    }
}

/// Service to window, one JSON object per line: `{"status":{...}}` or `{"input":{...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceMessage {
    Status(Snapshot),
    Input(InputView),
}

/// Window to service, one JSON object per line, e.g. `{"profile":"any_map"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    Profile(ProfileChoice),
    PadPreset(PadPreset),
    TapJump(bool),
    TriggerShields(TriggerShields),
    RightStick(RightStick),
    /// Replaces the per-control changes on top of the pad preset.
    Remaps(Remaps),
    /// Replaces the Any map profile's bindings.
    Bindings(Vec<Binding>),
}

impl ServiceMessage {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("status serializes") + "\n"
    }
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim_end())
    }
}

impl ClientMessage {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("message serializes") + "\n"
    }
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim_end())
    }
}
