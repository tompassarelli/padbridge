use super::Output;

/// Keys through XTEST on an X11 display; the pointer and clicks through the
/// compositor's virtual pointer.
pub struct DesktopOutput {
    keys: enigo::Enigo,
    pointer: super::pointer::VirtualPointer,
}

impl DesktopOutput {
    pub fn new(display: &str) -> Result<Self, String> {
        let settings = enigo::Settings { x11_display: Some(display.to_owned()), release_keys_when_dropped: true, ..enigo::Settings::default() };
        Ok(Self {
            keys: enigo::Enigo::new(&settings).map_err(|e| e.to_string())?,
            pointer: super::pointer::VirtualPointer::new()?,
        })
    }
}

/// The enigo key for a binding's key name (model::Press::Key).
pub fn key_of(name: &str) -> Option<enigo::Key> {
    use enigo::Key;
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return c.is_ascii_alphanumeric().then(|| Key::Unicode(c.to_ascii_lowercase()));
    }
    Some(match name {
        "space" => Key::Space,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        "enter" => Key::Return,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "f1" => Key::F1, "f2" => Key::F2, "f3" => Key::F3, "f4" => Key::F4,
        "f5" => Key::F5, "f6" => Key::F6, "f7" => Key::F7, "f8" => Key::F8,
        "f9" => Key::F9, "f10" => Key::F10, "f11" => Key::F11, "f12" => Key::F12,
        "f13" => Key::F13, "f14" => Key::F14, "f15" => Key::F15, "f16" => Key::F16,
        "f17" => Key::F17, "f18" => Key::F18, "f19" => Key::F19, "f20" => Key::F20,
        "f21" => Key::F21, "f22" => Key::F22, "f23" => Key::F23, "f24" => Key::F24,
        "insert" => Key::Insert, "delete" => Key::Delete, "end" => Key::End, "home" => Key::Home,
        "pageup" => Key::PageUp, "pagedown" => Key::PageDown,
        _ => return None,
    })
}

impl Output for DesktopOutput {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        use enigo::Keyboard;
        let key = key_of(name).ok_or_else(|| format!("unknown key {name:?}"))?;
        self.keys.key(key, if down { enigo::Direction::Press } else { enigo::Direction::Release }).map_err(|e| e.to_string())
    }

    fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
        self.pointer.button(right, down)
    }

    fn pointer(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        self.pointer.motion(dx, dy)
    }

    fn cursor(&mut self, x: f64, y: f64, width: u32, height: u32) -> Result<(), String> {
        self.pointer.absolute(x, y, width, height)
    }
}

impl crate::pad_ingress::Output for DesktopOutput {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> { Output::key(self, name, down) }
    fn cursor(&mut self, x: f64, y: f64, width: u32, height: u32) -> Result<(), String> { Output::cursor(self, x, y, width, height) }
}
