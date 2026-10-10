/// Whether the selected game currently owns keyboard focus. `Ok(false)` is
/// ordinary focus loss; `Err` means the selected target can no longer be
/// identified. Each OS compiles exactly one adapter, exported as `Gate`.
pub trait Foreground {
    fn eligible(&mut self) -> Result<bool, String>;

    /// Why the last check found the game ineligible, when the adapter can tell.
    fn away(&self) -> Option<&str> {
        None
    }
}

#[cfg(any(windows, target_os = "macos", test))]
fn executable_matches(path: &str, name: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .is_some_and(|file| file.eq_ignore_ascii_case(name))
}

#[cfg(test)]
#[test]
fn executable_match_uses_the_exact_file_name() {
    assert!(executable_matches(
        r"C:\Games\Warcraft III\_retail_\x86_64\Warcraft III.exe",
        "Warcraft III.exe"
    ));
    assert!(executable_matches(
        "/Applications/Warcraft III.app/Contents/MacOS/Warcraft III",
        "Warcraft III"
    ));
    assert!(executable_matches("warcraft iii.EXE", "Warcraft III.exe"));
    assert!(!executable_matches(
        r"C:\Games\Warcraft III.exe\launcher.exe",
        "Warcraft III.exe"
    ));
    assert!(!executable_matches(
        "/usr/bin/Warcraft III Launcher",
        "Warcraft III"
    ));
    assert!(!executable_matches("", "Warcraft III"));
}

#[cfg(target_os = "linux")]
mod linux {
    use serde_json::Value;
    use std::{fs, path::PathBuf, time::Duration};
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
            let socket = self.socket.as_ref().ok_or("Niri socket not selected")?;
            crate::niri::request(socket, request, Duration::from_millis(50))
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
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn pointer_root_requires_exact_active_and_pointer_recipient() {
            assert!(satellite_focus(42, 42, 1, 42));
            assert!(satellite_focus(42, 42, 42, 99));
            assert!(!satellite_focus(42, 42, 1, 99));
            assert!(!satellite_focus(42, 99, 1, 42));
            assert!(!satellite_focus(42, 42, 99, 42));
            assert!(!satellite_focus(42, 42, 0, 42));
        }

        #[test]
        fn bridge_metadata_must_match_exact_id_and_be_unique() {
            let window = json!({"id":354,"pid":3751,"title":"Warcraft III","app_id":"game"});
            let windows = json!([window]);
            assert!(matching_niri_window(&windows, 354, 3751, "Warcraft III", "game").is_some());
            assert!(matching_niri_window(&windows, 353, 3751, "Warcraft III", "game").is_none());
            assert!(matching_niri_window(&windows, 354, 3752, "Warcraft III", "game").is_none());
            assert!(matching_niri_window(&windows, 354, 3751, "Battle.net", "game").is_none());
            assert!(matching_niri_window(&windows, 354, 3751, "Warcraft III", "other").is_none());
            let duplicate =
                json!([window, {"id":355,"pid":3751,"title":"Warcraft III","app_id":"game"}]);
            assert!(matching_niri_window(&duplicate, 354, 3751, "Warcraft III", "game").is_none());
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{Gate, Target};

/// The foreground window's process image is Warcraft III.exe, optionally
/// pinned to one PID. Keyboard input follows the foreground window.
#[cfg(windows)]
mod windows {
    #![allow(unsafe_code)]
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
            UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
        },
        core::PWSTR,
    };

    pub struct Target {
        pub pid: Option<u32>,
    }

    pub struct Gate {
        target: Target,
    }

    const GAME: &str = "Warcraft III.exe";

    fn foreground_pid() -> Option<u32> {
        // SAFETY: no arguments; a null HWND means no foreground window.
        let window = unsafe { GetForegroundWindow() };
        if window.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        // SAFETY: pid is a live, writable u32 for the duration of the call.
        unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        (pid != 0).then_some(pid)
    }

    fn image_path(pid: u32) -> Result<String, String> {
        // SAFETY: plain query; the returned handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|e| format!("open process {pid}: {e}"))?;
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        // SAFETY: buffer holds `length` UTF-16 units; the call updates length.
        let result = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
        };
        // SAFETY: process is the handle opened above and is closed once.
        let _ = unsafe { CloseHandle(process) };
        result.map_err(|e| format!("query process {pid} image: {e}"))?;
        Ok(String::from_utf16_lossy(&buffer[..length as usize]))
    }

    impl Gate {
        pub fn new(target: Target) -> Result<Self, String> {
            if let Some(pid) = target.pid {
                if !super::executable_matches(&image_path(pid)?, GAME) {
                    return Err("selected PID does not identify Warcraft III.exe".into());
                }
            }
            Ok(Self { target })
        }
    }

    impl super::Foreground for Gate {
        fn eligible(&mut self) -> Result<bool, String> {
            let Some(pid) = foreground_pid() else {
                return Ok(false);
            };
            match self.target.pid {
                Some(selected) if selected != pid => Ok(false),
                // The selected game must remain inspectable; it may have exited.
                Some(_) => Ok(super::executable_matches(&image_path(pid)?, GAME)),
                // Another user's or an elevated foreground process cannot be the
                // game this helper can type into.
                None => {
                    Ok(image_path(pid).is_ok_and(|path| super::executable_matches(&path, GAME)))
                }
            }
        }
    }
}

#[cfg(windows)]
pub use windows::{Gate, Target};

/// The frontmost application's executable is named Warcraft III, optionally
/// pinned to one PID. Keyboard events posted to the HID stream reach it.
#[cfg(target_os = "macos")]
mod macos {
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
}

#[cfg(target_os = "macos")]
pub use macos::{Gate, Target};
