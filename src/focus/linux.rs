use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};
use x11rb::{
    protocol::{
        res::{ClientIdMask, ClientIdSpec, ConnectionExt as _},
        xproto::{AtomEnum, ConnectionExt},
    },
    rust_connection::RustConnection,
};

pub struct Target {
    pub display: String,
    pub window: u32,
    pub pid: u32,
    pub niri_window: Option<u64>,
    pub wlr_app_id: Option<String>,
}

pub struct Gate {
    target: Target,
    socket: Option<PathBuf>,
    wlr: Option<crate::wlr::Monitor>,
    x11: RustConnection,
    pid_atom: u32,
    birth: String,
    root: u32,
    satellite: Option<Satellite>,
    away: Option<String>,
}

struct Satellite {
    pid: u32,
    birth: String,
    wm_window: u32,
}

fn matching_niri_window<'a>(
    windows: &'a Value,
    id: u64,
    pid: u32,
    title: &str,
    app_id: &str,
) -> Option<&'a Value> {
    let mut matching = windows.as_array()?.iter().filter(|window| {
        window.get("pid").and_then(Value::as_u64) == Some(u64::from(pid))
            && window.get("title").and_then(Value::as_str) == Some(title)
            && window.get("app_id").and_then(Value::as_str) == Some(app_id)
    });
    let window = matching.next()?;
    (matching.next().is_none() && window.get("id").and_then(Value::as_u64) == Some(id))
        .then_some(window)
}

fn satellite_focus(target: u32, active: u32, focus: u32, pointer_child: u32) -> bool {
    // PointerRoot routes core keyboard events to the window under the pointer.
    // Active-window metadata alone does not establish that recipient.
    active == target && (focus == target || (focus == 1 && pointer_child == target))
}

fn process_birth(pid: u32) -> Result<String, String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .map(str::to_owned)
        .ok_or_else(|| "cannot read selected process identity".into())
}

impl Gate {
    pub fn new(target: Target) -> Result<Self, String> {
        if target.niri_window.is_some() == target.wlr_app_id.is_some() {
            return Err(
                "select exactly one foreground adapter: --niri-window or --private-wlr-app-id"
                    .into(),
            );
        }
        let socket = if target.niri_window.is_some() {
            Some(
                std::env::var_os("NIRI_SOCKET")
                    .map(PathBuf::from)
                    .ok_or("NIRI_SOCKET is missing")?,
            )
        } else {
            None
        };
        let wlr = target
            .wlr_app_id
            .clone()
            .map(crate::wlr::Monitor::new)
            .transpose()?;
        let command =
            fs::read(format!("/proc/{}/cmdline", target.pid)).map_err(|e| e.to_string())?;
        let is_game = command.split(|b| *b == 0).any(|arg| {
            let arg = String::from_utf8_lossy(arg);
            arg.rsplit(['/', '\\'])
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("Warcraft III.exe"))
        });
        if !is_game {
            return Err("selected PID does not identify Warcraft III.exe".into());
        }
        let birth = process_birth(target.pid)?;
        let (x11, _) = x11rb::connect(Some(&target.display)).map_err(|e| e.to_string())?;
        let pid_atom = x11
            .intern_atom(false, b"_NET_WM_PID")
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .atom;
        let root = x11
            .get_geometry(target.window)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .root;
        let mut gate = Self {
            target,
            socket,
            wlr,
            x11,
            pid_atom,
            birth,
            root,
            satellite: None,
            away: None,
        };
        gate.check_window_pid()?;
        if let Some(id) = gate.target.niri_window {
            let windows = gate.request("Windows")?;
            let window = windows
                .as_array()
                .and_then(|windows| {
                    windows
                        .iter()
                        .find(|window| window.get("id").and_then(Value::as_u64) == Some(id))
                })
                .ok_or("selected Niri window is missing")?;
            let pid = window
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .ok_or("Niri window PID missing")?;
            if pid != gate.target.pid {
                let executable =
                    fs::read_link(format!("/proc/{pid}/exe")).map_err(|e| e.to_string())?;
                if !matches!(
                    executable.file_name().and_then(|name| name.to_str()),
                    Some("xwayland-satellite" | ".xwayland-satellite-wrapped")
                ) {
                    return Err("selected Niri window has an unrecognized process owner".into());
                }
                let wm_window = gate.word_property(
                    gate.root,
                    b"_NET_SUPPORTING_WM_CHECK",
                    AtomEnum::WINDOW,
                )?;
                gate.satellite = Some(Satellite {
                    pid,
                    birth: process_birth(pid)?,
                    wm_window,
                });
                gate.check_satellite()?;
                if gate.satellite_window(&windows)?.is_none() {
                    return Err(
                        "selected Niri window does not uniquely identify the selected X11 game"
                            .into(),
                    );
                }
            }
        }
        Ok(gate)
    }

    fn atom(&self, name: &[u8]) -> Result<u32, String> {
        self.x11
            .intern_atom(false, name)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())
            .map(|reply| reply.atom)
    }

    fn word_property(&self, window: u32, name: &[u8], kind: AtomEnum) -> Result<u32, String> {
        let property = self
            .x11
            .get_property(false, window, self.atom(name)?, kind, 0, 2)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if property.bytes_after != 0 || property.value_len != 1 {
            return Err(format!(
                "missing or invalid {}",
                String::from_utf8_lossy(name)
            ));
        }
        property
            .value32()
            .and_then(|mut values| values.next())
            .ok_or_else(|| "invalid window property type".into())
    }

    fn text_property(&self, window: u32, name: &[u8]) -> Result<String, String> {
        let property = self
            .x11
            .get_property(false, window, self.atom(name)?, AtomEnum::ANY, 0, 1024)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if property.format != 8 || property.bytes_after != 0 || property.value.is_empty() {
            return Err(format!(
                "missing or invalid {}",
                String::from_utf8_lossy(name)
            ));
        }
        String::from_utf8(property.value).map_err(|e| e.to_string())
    }

    fn check_satellite(&self) -> Result<(), String> {
        let satellite = self.satellite.as_ref().ok_or("missing bridge identity")?;
        if process_birth(satellite.pid)? != satellite.birth
            || self.word_property(self.root, b"_NET_SUPPORTING_WM_CHECK", AtomEnum::WINDOW)?
                != satellite.wm_window
            || self.word_property(
                satellite.wm_window,
                b"_NET_SUPPORTING_WM_CHECK",
                AtomEnum::WINDOW,
            )? != satellite.wm_window
            || self.text_property(satellite.wm_window, b"_NET_WM_NAME")? != "xwayland-satellite"
        {
            return Err("X11 window manager identity changed".into());
        }
        // XRes obtains the owning connection's PID from the X server, rather
        // than trusting a window's self-declared PID or matching a title alone.
        let reply = self
            .x11
            .res_query_client_ids(&[ClientIdSpec {
                client: satellite.wm_window,
                mask: ClientIdMask::LOCAL_CLIENT_PID,
            }])
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if reply.ids.len() != 1 || reply.ids[0].value.as_slice() != [satellite.pid] {
            return Err("Niri proxy does not own the selected X11 window manager".into());
        }
        Ok(())
    }

    fn satellite_window<'a>(&self, windows: &'a Value) -> Result<Option<&'a Value>, String> {
        let title = self.text_property(self.target.window, b"_NET_WM_NAME")?;
        let class = self.text_property(self.target.window, b"WM_CLASS")?;
        let mut parts = class.split('\0');
        let _instance = parts.next();
        let app_id = parts
            .next()
            .filter(|value| !value.is_empty())
            .ok_or("X11 window class missing")?;
        Ok(matching_niri_window(
            windows,
            self.target.niri_window.ok_or("Niri target missing")?,
            self.satellite
                .as_ref()
                .ok_or("bridge identity missing")?
                .pid,
            &title,
            app_id,
        ))
    }

    fn request(&self, request: &str) -> Result<Value, String> {
        let mut socket =
            UnixStream::connect(self.socket.as_ref().ok_or("Niri socket not selected")?)
                .map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_millis(50)))
            .map_err(|e| e.to_string())?;
        writeln!(socket, "\"{request}\"").map_err(|e| e.to_string())?;
        socket
            .shutdown(std::net::Shutdown::Write)
            .map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(socket)
            .read_line(&mut reply)
            .map_err(|e| e.to_string())?;
        let parsed: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
        parsed
            .get("Ok")
            .and_then(|ok| ok.get(request))
            .cloned()
            .ok_or_else(|| format!("Niri did not return {request}"))
    }

    fn check_window_pid(&self) -> Result<(), String> {
        let property = self
            .x11
            .get_property(
                false,
                self.target.window,
                self.pid_atom,
                AtomEnum::CARDINAL,
                0,
                1,
            )
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if property.value32().and_then(|mut v| v.next()) != Some(self.target.pid) {
            return Err("selected X11 window no longer belongs to selected game PID".into());
        }
        Ok(())
    }
}

/// The compositor's view of a window, for diagnostics; titles stay out.
fn niri_summary(window: &Value) -> String {
    if window.is_null() {
        return "none".into();
    }
    let field = |name: &str| window.get(name).map_or("?".into(), Value::to_string);
    format!(
        "id={} app_id={} pid={} is_focused={} workspace_id={}",
        field("id"),
        field("app_id"),
        field("pid"),
        field("is_focused"),
        field("workspace_id")
    )
}

impl Gate {
    fn x11_class(&self, window: u32) -> String {
        self.text_property(window, b"WM_CLASS")
            .map_or_else(|_| "?".into(), |class| class.trim_end_matches('\0').replace('\0', "/"))
    }

    fn away_reason(&mut self) -> Result<Option<String>, String> {
        if process_birth(self.target.pid)? != self.birth {
            return Err("selected game process restarted".into());
        }
        self.check_window_pid()?;
        if let Some(wlr) = &mut self.wlr {
            if !wlr.eligible()? {
                return Ok(Some("wlr-toplevel-inactive".into()));
            }
        } else {
            let overview = self.request("OverviewState")?;
            if overview.get("is_open").and_then(Value::as_bool) != Some(false) {
                return Ok(Some("niri-overview-open".into()));
            }
            let window = self.request("FocusedWindow")?;
            if window.get("id").and_then(Value::as_u64) != self.target.niri_window
                || window.get("is_focused").and_then(Value::as_bool) != Some(true)
            {
                return Ok(Some(format!("niri-focused {}", niri_summary(&window))));
            }
            if self.satellite.is_some() {
                self.check_satellite()?;
                let windows = self.request("Windows")?;
                if self
                    .satellite_window(&windows)?
                    .is_none_or(|selected| selected != &window)
                {
                    return Ok(Some(format!("niri-window-metadata {}", niri_summary(&window))));
                }
                let active =
                    self.word_property(self.root, b"_NET_ACTIVE_WINDOW", AtomEnum::WINDOW)?;
                let focus = self
                    .x11
                    .get_input_focus()
                    .map_err(|e| e.to_string())?
                    .reply()
                    .map_err(|e| e.to_string())?
                    .focus;
                let pointer = self
                    .x11
                    .query_pointer(self.root)
                    .map_err(|e| e.to_string())?
                    .reply()
                    .map_err(|e| e.to_string())?;
                if !pointer.same_screen
                    || !satellite_focus(self.target.window, active, focus, pointer.child)
                {
                    return Ok(Some(format!(
                        "x11 target={:#x} active={active:#x}({}) focus={focus:#x}({}) pointer_child={:#x}({}) same_screen={}",
                        self.target.window,
                        self.x11_class(active),
                        self.x11_class(focus),
                        pointer.child,
                        self.x11_class(pointer.child),
                        pointer.same_screen,
                    )));
                }
                // Bracket the X11 observations with current compositor state.
                let after = self.request("FocusedWindow")?;
                if after != window {
                    return Ok(Some(format!("niri-focus-moved {}", niri_summary(&after))));
                }
                if self
                    .request("OverviewState")?
                    .get("is_open")
                    .and_then(Value::as_bool)
                    != Some(false)
                {
                    return Ok(Some("niri-overview-open".into()));
                }
                return Ok(None);
            }
            if window.get("pid").and_then(Value::as_u64) != Some(u64::from(self.target.pid)) {
                return Ok(Some(format!("niri-focused-pid {}", niri_summary(&window))));
            }
        }
        // Require the exact focus recipient, not stale _NET_ACTIVE_WINDOW metadata.
        let focus = self
            .x11
            .get_input_focus()
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .focus;
        Ok((focus != self.target.window).then(|| {
            format!("x11 target={:#x} focus={focus:#x}({})", self.target.window, self.x11_class(focus))
        }))
    }
}

impl super::Foreground for Gate {
    fn eligible(&mut self) -> Result<bool, String> {
        self.away = self.away_reason()?;
        Ok(self.away.is_none())
    }

    fn away(&self) -> Option<&str> {
        self.away.as_deref()
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use crate::rng::{Rng, cases};

    #[test]
    fn focus_needs_the_exact_target_and_niri_metadata_a_unique_exact_match() {
        let values = [0, 1, 42, 99];
        for target in values { for active in values { for focus in values { for pointer in values {
            let expected = target != 1 && active == target && (focus == target || (focus == 1 && pointer == target));
            if target != 1 {
                assert_eq!(satellite_focus(target, active, focus, pointer), expected, "{target} {active} {focus} {pointer}");
            }
        } } } }
        let mut rng = Rng::new(12);
        let candidates = [(354u64, 3751u64, "Warcraft III", "game"), (355, 3751, "Warcraft III", "game"), (356, 3752, "Warcraft III", "game"),
            (357, 3751, "Battle.net", "game"), (358, 3751, "Warcraft III", "other")];
        for _ in 0..cases() {
            let chosen: Vec<_> = candidates.iter().filter(|_| rng.chance(50)).collect();
            let windows = Value::Array(chosen.iter().map(|(id, pid, title, app)| serde_json::json!({"id": id, "pid": pid, "title": title, "app_id": app})).collect());
            let id = rng.pick(&[354u64, 355, 356, 357, 358]);
            let matching: Vec<u64> = chosen.iter().filter(|(_, pid, title, app)| *pid == 3751 && *title == "Warcraft III" && *app == "game").map(|(id, ..)| *id).collect();
            let found = matching_niri_window(&windows, id, 3751, "Warcraft III", "game").and_then(|w| w.get("id")).and_then(Value::as_u64);
            assert_eq!(found, (matching == [id]).then_some(id), "{windows} {id}");
        }
    }
}
