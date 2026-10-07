//! Candidate analog channels, selected explicitly for the native comparison.
use crate::{Sample, model::pad::{Pad, ACTIVE_KEY, PRESENT_KEY, KEY_NAMES}};

/// Rectangle in compositor logical pixels, and the compositor output extent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorGrid {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub screen_width: u32,
    pub screen_height: u32,
}

impl CursorGrid {
    /// `x,y,width,height,screen_width,screen_height` in logical pixels.
    pub fn parse(text: &str) -> Result<Self, String> {
        let values: Vec<f64> = text.split(',').map(str::parse).collect::<Result<_, _>>().map_err(|_| "invalid cursor rectangle")?;
        let [x, y, width, height, sw, sh] = values.as_slice() else { return Err("cursor rectangle needs six numbers".into()); };
        if values.iter().any(|v| !v.is_finite()) || *x < 0.0 || *y < 0.0 || *width <= 0.0 || *height <= 0.0
            || *sw > f64::from(u32::MAX) || *sh > f64::from(u32::MAX) || sw.fract() != 0.0 || sh.fract() != 0.0
            || x + width >= *sw || y + height >= *sh {
            return Err("cursor rectangle must fit inside a positive screen extent".into());
        }
        Ok(Self { x: *x, y: *y, width: *width, height: *height, screen_width: *sw as u32, screen_height: *sh as u32 })
    }

    fn point(self, (x, y): (u8, u8)) -> (f64, f64) {
        (self.x + f64::from(x) * self.width / 127.0, self.y + f64::from(y) * self.height / 127.0)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Route {
    #[default]
    Digital,
    Keys,
    Cursor(CursorGrid),
}

impl Route {
    pub fn parse(name: &str, grid: Option<&str>) -> Result<Self, String> {
        match name {
            "digital" => Ok(Self::Digital),
            "keys" => Ok(Self::Keys),
            "cursor" => Ok(Self::Cursor(CursorGrid::parse(grid.ok_or("cursor output requires a cursor rectangle")?)?)),
            _ => Err("pad ingress must be digital, keys or cursor".into()),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        Self::parse(&std::env::var("WC3_PAD_INGRESS").unwrap_or_else(|_| "digital".into()), std::env::var("WC3_PAD_CURSOR_GRID").ok().as_deref())
    }
}

pub trait Output {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String>;
    fn cursor(&mut self, x: f64, y: f64, screen_width: u32, screen_height: u32) -> Result<(), String>;
}

pub struct Ingress {
    route: Route,
    held: u16,
    active: bool,
    present: bool,
    last: Option<u16>,
}

impl Ingress {
    pub fn new(route: Route) -> Self { Self { route, held: 0, active: false, present: false, last: None } }

    /// Eligibility and neutral rearming come from the ordinary keys mapper.
    pub fn update(&mut self, sample: &Sample, eligible: bool, out: &mut dyn Output) -> Result<(), String> {
        if self.route == Route::Digital { return Ok(()); }
        if !eligible { return self.release(out); }
        if !self.present {
            self.present = true;
            out.key(PRESENT_KEY, true)?;
        }
        let (x, y) = crate::stick::melee_stick(sample.left_x, sample.left_y);
        let pad = Pad::quantize([x, y], sample.left_trigger.max(0) as u16, sample.right_trigger.max(0) as u16);
        let payload = pad.packed().expect("quantization returns valid levels");
        if self.last != Some(payload) {
            match self.route {
                Route::Keys => {
                    if self.active {
                        out.key(ACTIVE_KEY, false)?;
                        self.active = false;
                    }
                    for (bit, name) in KEY_NAMES.iter().enumerate() {
                        let mask = 1 << bit;
                        if (self.held ^ payload) & mask != 0 {
                            if payload & mask != 0 { self.held |= mask; }
                            out.key(name, payload & mask != 0)?;
                            if payload & mask == 0 { self.held &= !mask; }
                        }
                    }
                }
                Route::Cursor(grid) => {
                    if !self.active {
                        self.active = true;
                        out.key(ACTIVE_KEY, true)?;
                    }
                    let (x, y) = grid.point(pad.cursor_cell().expect("valid payload"));
                    out.cursor(x, y, grid.screen_width, grid.screen_height)?;
                }
                Route::Digital => {}
            }
            self.last = Some(payload);
            eprintln!("pad_ingress payload={payload} x={} z={} left={} right={}", pad.x, pad.z, pad.left, pad.right);
        }
        if !self.active {
            self.active = true;
            out.key(ACTIVE_KEY, true)?;
        }
        Ok(())
    }

    pub fn release(&mut self, out: &mut dyn Output) -> Result<(), String> {
        if self.active {
            out.key(ACTIVE_KEY, false)?;
            self.active = false;
        }
        if self.present {
            out.key(PRESENT_KEY, false)?;
            self.present = false;
        }
        for (bit, name) in KEY_NAMES.iter().enumerate() {
            if self.held & (1 << bit) != 0 {
                out.key(name, false)?;
                self.held &= !(1 << bit);
            }
        }
        self.last = None;
        Ok(())
    }
}

/// Hold a calibration marker until the caller observes the map callback.
pub fn calibrate(grid: CursorGrid, end: bool, down: bool, out: &mut dyn Output) -> Result<(), String> {
    out.key(PRESENT_KEY, down)?;
    out.key(ACTIVE_KEY, down)?;
    out.key(if end { "pagedown" } else { "pageup" }, down)?;
    if down {
        let (x, y) = grid.point(if end { (127, 127) } else { (0, 0) });
        out.cursor(x, y, grid.screen_width, grid.screen_height)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Recorded(Vec<(String, bool)>);
    impl Output for Recorded {
        fn key(&mut self, name: &str, down: bool) -> Result<(), String> { self.0.push((name.into(), down)); Ok(()) }
        fn cursor(&mut self, _: f64, _: f64, _: u32, _: u32) -> Result<(), String> { Ok(()) }
    }

    #[test]
    fn keys_change_only_different_bits_and_cleanup_releases_every_owned_key() {
        let (mut ingress, mut out) = (Ingress::new(Route::Keys), Recorded::default());
        ingress.update(&Sample::default(), true, &mut out).unwrap();
        assert_eq!(out.0.last(), Some(&(ACTIVE_KEY.into(), true)));
        out.0.clear();
        ingress.update(&Sample::default(), true, &mut out).unwrap();
        assert!(out.0.is_empty());
        ingress.update(&Sample { left_x: 16384, right_trigger: 32767, ..Sample::default() }, true, &mut out).unwrap();
        assert_eq!(out.0.first(), Some(&(ACTIVE_KEY.into(), false)));
        assert_eq!(out.0.last(), Some(&(ACTIVE_KEY.into(), true)));
        assert!(!out.0.iter().any(|(key, _)| key == PRESENT_KEY));
        ingress.update(&Sample::default(), false, &mut out).unwrap();
        assert_eq!(ingress.held, 0);
        assert!(!ingress.active);
        assert!(!ingress.present);
        out.0.clear();
        ingress.release(&mut out).unwrap();
        assert!(out.0.is_empty());
    }
}
