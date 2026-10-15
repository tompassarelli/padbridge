use sdl3::{
    JoystickSubsystem,
    gamepad::{Axis, Button},
    joystick::{Joystick, JoystickType, VirtualJoystickConnection, VirtualJoystickDescription},
};
use std::{
    io::BufRead,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

// Ascending SDL enum order: SDL numbers a virtual gamepad's controls that way.
const VIRTUAL_BUTTONS: [Button; 9] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::Back,
    Button::Start,
    Button::LeftShoulder,
    Button::RightShoulder,
    Button::LeftStick,
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
/// `button back 0|1` retains View input; the keyboard layout has no View action.
/// `axis leftx|lefty|rightx|righty|lefttrigger|righttrigger RAW` (raw joystick
/// units; SDL maps triggers from -32768..32767 to 0..32767), `detach`, `quit`.
pub struct VirtualPad {
    pub attached: Option<(Joystick, VirtualJoystickConnection)>,
    commands: mpsc::Receiver<String>,
}

impl VirtualPad {
    pub fn attach(joysticks: &JoystickSubsystem) -> Result<Self, String> {
        let desc = VirtualJoystickDescription::new()
            .name("padbridge virtual pad")
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
    pub fn step(&mut self, running: &AtomicBool) -> Result<(), String> {
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
