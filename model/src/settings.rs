use serde::{Deserialize, Serialize};

use crate::{Binding, ClientMessage, PadKind, PadPreset, REMAPPABLE, Remaps, pad_bindings, remap};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerShield {
    #[default]
    Full,
    Light,
}

impl TriggerShield {
    pub fn name(self) -> &'static str { match self { Self::Full => "full", Self::Light => "light" } }
    pub fn parse(name: &str) -> Result<Self, String> {
        match name { "full" => Ok(Self::Full), "light" => Ok(Self::Light), _ => Err("trigger shield needs full or light".into()) }
    }
    pub fn pressure(self) -> u16 { match self { Self::Full => 255, Self::Light => 77 } }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerShields {
    pub left: TriggerShield,
    pub right: TriggerShield,
}

/// What a right-stick flick asks for; Tilt also holds Tilt (6), which turns the smash into a tilt or aerial.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightStick {
    #[default]
    Smash,
    Tilt,
}

impl RightStick {
    pub const ALL: [Self; 2] = [Self::Smash, Self::Tilt];
    pub fn name(self) -> &'static str { match self { Self::Smash => "smash", Self::Tilt => "tilt" } }
    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|mode| mode.name() == name).ok_or_else(|| "right stick needs smash or tilt".into())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControllerSettings {
    pub pad_preset: PadPreset,
    pub tap_jump: bool,
    pub triggers: TriggerShields,
    pub right_stick: RightStick,
    pub remaps: Remaps,
}

impl ControllerSettings {
    /// Applies a settings message; remaps naming a control that can't be remapped are refused whole.
    pub fn apply(&mut self, message: &ClientMessage) -> bool {
        let before = self.clone();
        match message {
            ClientMessage::PadPreset(preset) => self.pad_preset = *preset,
            ClientMessage::TapJump(on) => self.tap_jump = *on,
            ClientMessage::TriggerShields(triggers) => self.triggers = *triggers,
            ClientMessage::RightStick(mode) => self.right_stick = *mode,
            ClientMessage::Remaps(remaps) if remaps.keys().all(|control| REMAPPABLE.contains(control)) => self.remaps = remaps.clone(),
            _ => {}
        }
        *self != before
    }

    /// The preset on a pad of `kind` with the remaps applied.
    pub fn bindings(&self, kind: PadKind) -> Vec<Binding> {
        remap(pad_bindings(self.pad_preset, self.triggers, self.right_stick, kind), &self.remaps, self.triggers)
    }
}
