//! The foreground window's process image is Warcraft III.exe, optionally
//! pinned to one PID. Keyboard input follows the foreground window.
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
