use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

/// The game window's width in logical pixels, as niri lays it out.
pub fn niri_window_width(socket: &Path, window: u64) -> Option<f64> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(200))).ok()?;
    writeln!(stream, "\"Windows\"").ok()?;
    stream.shutdown(std::net::Shutdown::Write).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    let parsed: Value = serde_json::from_str(&reply).ok()?;
    parsed.pointer("/Ok/Windows")?.as_array()?.iter()
        .find(|entry| entry.get("id").and_then(Value::as_u64) == Some(window))?
        .pointer("/layout/window_size/0")?.as_f64()
}

/// Whether niri's focused window is `window`; any failure reads as not focused.
pub fn niri_focused(socket: &Path, window: u64) -> bool {
    let ask = || -> Result<bool, String> {
        let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(Duration::from_millis(50))).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(Duration::from_millis(50))).map_err(|e| e.to_string())?;
        writeln!(stream, "\"FocusedWindow\"").map_err(|e| e.to_string())?;
        stream.shutdown(std::net::Shutdown::Write).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
        let parsed: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
        Ok(parsed.pointer("/Ok/FocusedWindow/id").and_then(Value::as_u64) == Some(window))
    };
    ask().unwrap_or(false)
}
