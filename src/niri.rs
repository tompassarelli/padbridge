//! niri IPC: one connection per request, a JSON string out and one `{"Ok":{NAME:...}}` line back.

use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

/// Sends `request` (e.g. `Windows`) and returns what niri replied under its name.
pub fn request(socket: &Path, request: &str, timeout: Duration) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    writeln!(stream, "\"{request}\"").map_err(|e| e.to_string())?;
    stream.shutdown(std::net::Shutdown::Write).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    let mut parsed: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    parsed
        .get_mut("Ok")
        .and_then(|ok| ok.get_mut(request))
        .map(Value::take)
        .ok_or_else(|| format!("Niri did not return {request}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::net::UnixListener, thread};

    fn serve(name: &str, reply: &'static str) -> (std::path::PathBuf, thread::JoinHandle<String>) {
        let socket = std::env::temp_dir().join(format!("niri-test-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut asked = String::new();
            stream.read_to_string(&mut asked).unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
            asked
        });
        (socket, server)
    }

    #[test]
    fn a_request_is_one_json_string_and_the_reply_is_its_named_value() {
        let (socket, server) = serve("ok", "{\"Ok\":{\"FocusedWindow\":{\"id\":7}}}\n");
        let reply = request(&socket, "FocusedWindow", Duration::from_secs(5)).unwrap();
        assert_eq!(server.join().unwrap(), "\"FocusedWindow\"\n");
        assert_eq!(reply.get("id").and_then(Value::as_u64), Some(7));
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn an_error_or_another_reply_is_an_error() {
        let (socket, server) = serve("err", "{\"Err\":\"unknown request\"}\n");
        assert_eq!(request(&socket, "Windows", Duration::from_secs(5)), Err("Niri did not return Windows".into()));
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
