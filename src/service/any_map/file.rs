use super::Output;
use std::{fs, io::Write};

/// Presses written as lines, for a stand-in game.
pub(super) struct FileOutput(pub(super) fs::File);

impl Output for FileOutput {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        writeln!(self.0, "{} {name}", if down { "down" } else { "up" }).map_err(|error| error.to_string())
    }
    fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
        writeln!(self.0, "click {} {}", if right { "right" } else { "left" }, if down { "down" } else { "up" }).map_err(|error| error.to_string())
    }
    fn pointer(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        writeln!(self.0, "pointer {dx:.0} {dy:.0}").map_err(|error| error.to_string())
    }
}
