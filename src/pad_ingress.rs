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
mod properties {
    use super::*;
    use crate::rng::{Rng, cases};
    use std::collections::BTreeSet;

    #[derive(Default)]
    struct Recorded {
        down: BTreeSet<String>,
        events: Vec<(String, bool)>,
        cursor: Option<(f64, f64)>,
    }

    impl Output for Recorded {
        fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
            if down {
                assert!(self.down.insert(name.into()), "{name} pressed twice");
            } else {
                assert!(self.down.remove(name), "{name} released while up");
            }
            if KEY_NAMES.contains(&name) {
                assert!(!self.down.contains(ACTIVE_KEY), "{name} changed while the marker is held");
            }
            self.events.push((name.into(), down));
            Ok(())
        }
        fn cursor(&mut self, x: f64, y: f64, _: u32, _: u32) -> Result<(), String> {
            self.cursor = Some((x, y));
            Ok(())
        }
    }

    #[test]
    fn held_keys_always_spell_the_quantized_pad_and_cleanup_releases_them_all() {
        let mut rng = Rng::new(7);
        let grid = CursorGrid { x: 10.0, y: 20.0, width: 127.0, height: 254.0, screen_width: 1920, screen_height: 1080 };
        for _ in 0..cases() {
            let route = rng.pick(&[Route::Digital, Route::Keys, Route::Cursor(grid)]);
            let (mut ingress, mut out) = (Ingress::new(route), Recorded::default());
            let mut last = None;
            for _ in 0..20 {
                out.events.clear();
                if rng.chance(10) {
                    ingress.release(&mut out).unwrap();
                    assert!(out.down.is_empty(), "{:?}", out.down);
                    last = None;
                    continue;
                }
                let axis = |rng: &mut Rng| { let free = rng.next() as i16; rng.pick(&[0, 0, 9174, 16384, -32767, free]) };
                let sample = Sample { left_x: axis(&mut rng), left_y: axis(&mut rng), left_trigger: rng.pick(&[0, 20000, 32767, -5]), right_trigger: rng.pick(&[0, 32767]), ..Sample::default() };
                let eligible = rng.chance(85);
                ingress.update(&sample, eligible, &mut out).unwrap();
                if route == Route::Digital {
                    assert!(out.events.is_empty());
                    continue;
                }
                if !eligible {
                    assert!(out.down.is_empty(), "{:?}", out.down);
                    last = None;
                    continue;
                }
                let (x, y) = crate::stick::melee_stick(sample.left_x, sample.left_y);
                let pad = Pad::quantize([x, y], sample.left_trigger.max(0) as u16, sample.right_trigger.max(0) as u16);
                let payload = pad.packed().unwrap();
                let mut expected: BTreeSet<String> = [PRESENT_KEY.to_owned(), ACTIVE_KEY.to_owned()].into();
                if route == Route::Keys {
                    expected.extend(KEY_NAMES.iter().enumerate().filter(|(bit, _)| payload & (1 << bit) != 0).map(|(_, name)| name.to_string()));
                }
                assert_eq!(out.down, expected, "{sample:?}");
                if last == Some(payload) {
                    assert!(out.events.is_empty(), "an unchanged pad sent {:?}", out.events);
                    assert_eq!(out.cursor, None);
                } else if let Route::Cursor(grid) = route {
                    let (cx, cy) = pad.cursor_cell().unwrap();
                    let point = (grid.x + f64::from(cx) * grid.width / 127.0, grid.y + f64::from(cy) * grid.height / 127.0);
                    assert_eq!(out.cursor.take(), Some(point));
                }
                last = Some(payload);
            }
        }
    }
}
