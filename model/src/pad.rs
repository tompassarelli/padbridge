//! Quantized pad values carried by either candidate Warcraft ingress channel.

pub const AXIS_LEVELS: [i8; 17] = [-127, -114, -101, -88, -75, -62, -49, -36, 0, 36, 49, 62, 75, 88, 101, 114, 127];
pub const TRIGGER_LEVELS: [u8; 4] = [0, 77, 166, 255];
pub const KEY_NAMES: [&str; 14] = ["f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "insert", "delete"];
pub const ACTIVE_KEY: &str = "end";
pub const PRESENT_KEY: &str = "home";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    pub x: i8,
    pub z: i8,
    pub left: u8,
    pub right: u8,
}

fn axis(raw: i16) -> i8 {
    let value = i32::from(raw).clamp(-32767, 32767);
    let magnitude = value.abs();
    if magnitude <= 9174 { return 0; }
    let level = ((magnitude - 9175) * 7 + 11796) / 23592;
    let index = if value < 0 { 7 - level } else { 9 + level };
    AXIS_LEVELS[index as usize]
}

fn trigger(raw: u16) -> u8 {
    if raw <= 4000 { return 0; }
    let value = u32::from(raw.min(32767)) * 255 / 32767;
    *TRIGGER_LEVELS[1..].iter().min_by_key(|&&level| u32::from(level).abs_diff(value)).expect("three active trigger levels")
}

impl Pad {
    /// Sticks have already passed through the helper's radial clamp; SDL y is down.
    pub fn quantize([x, y]: [i16; 2], left: u16, right: u16) -> Self {
        Self { x: axis(x), z: axis(y.saturating_neg()), left: trigger(left), right: trigger(right) }
    }

    pub fn packed(self) -> Option<u16> {
        let x = AXIS_LEVELS.iter().position(|&value| value == self.x)? as u16;
        let z = AXIS_LEVELS.iter().position(|&value| value == self.z)? as u16;
        let left = TRIGGER_LEVELS.iter().position(|&value| value == self.left)? as u16;
        let right = TRIGGER_LEVELS.iter().position(|&value| value == self.right)? as u16;
        Some(x | z << 5 | left << 10 | right << 12)
    }

    pub fn unpack(packed: u16) -> Option<Self> {
        if packed >= 16384 { return None; }
        Some(Self { x: *AXIS_LEVELS.get(usize::from(packed & 31))?, z: *AXIS_LEVELS.get(usize::from((packed >> 5) & 31))?,
            left: TRIGGER_LEVELS[usize::from((packed >> 10) & 3)], right: TRIGGER_LEVELS[usize::from((packed >> 12) & 3)] })
    }

    pub fn cursor_cell(self) -> Option<(u8, u8)> {
        let packed = self.packed()?;
        Some(((packed & 127) as u8, (packed >> 7) as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamped_pad_keeps_neutral_half_full_and_trigger_pressure() {
        assert_eq!(Pad::quantize([9174, -9174], 4000, 0), Pad::default());
        assert_eq!(Pad::quantize([16384, -32767], 10000, 32767), Pad { x: 62, z: 127, left: 77, right: 255 });
        assert_eq!(Pad::quantize([-32767, 9175], 22000, 0), Pad { x: -127, z: -36, left: 166, right: 0 });
    }

    #[test]
    fn every_level_roundtrips_through_keys_and_cursor_cells() {
        for x in AXIS_LEVELS { for z in AXIS_LEVELS { for left in TRIGGER_LEVELS { for right in TRIGGER_LEVELS {
            let pad = Pad { x, z, left, right };
            let bits = pad.packed().unwrap();
            assert_eq!(Pad::unpack(bits), Some(pad));
            let (cx, cy) = pad.cursor_cell().unwrap();
            assert_eq!(Pad::unpack(u16::from(cx) | u16::from(cy) << 7), Some(pad));
        } } } }
        assert_eq!(Pad { x: -62, z: 127, left: 77, right: 255 }.packed(), Some(13829));
        assert_eq!(Pad::unpack(31), None);
    }
}
