//! The analog-stick conversion, shared by both helpers for every pad. A stick is
//! limited radially to its full scale of 80 units, then each axis whose magnitude
//! is at most 0.28 of full scale reads 0; a value outside the deadzone is kept,
//! not rescaled.

/// The axial stick deadzone as a fraction of full scale (design target 0.28).
pub const STICK_DEADZONE: f32 = 0.28;

/// One stick in full-scale ±32767 units (SDL values and normalized evdev
/// values), with Melee's radial clamp and axial deadzone applied.
pub fn melee_stick(x: i16, y: i16) -> (i16, i16) {
    let (mut nx, mut ny) = (f32::from(x) / 32_767.0, f32::from(y) / 32_767.0);
    let radius = (nx * nx + ny * ny).sqrt();
    if radius > 1.0 {
        nx /= radius;
        ny /= radius;
    }
    let axis = |value: f32| {
        if value.abs() <= STICK_DEADZONE {
            0
        } else {
            (value * 32_767.0) as i16
        }
    };
    (axis(nx), axis(ny))
}

#[cfg(test)]
#[test]
fn resting_and_drifted_sticks_read_neutral_and_just_outside_reads_direction() {
    // 0.28 * 32767 = 9174.76: 9174 is inside the deadzone, 9175 outside.
    for (x, y) in [(0, 0), (1_500, -2_200), (-9_174, 9_174), (9_174, 0)] {
        assert_eq!(melee_stick(x, y), (0, 0), "{x},{y}");
    }
    assert_eq!(melee_stick(9_175, 0), (9_175, 0));
    assert_eq!(melee_stick(-9_175, 0), (-9_175, 0));
    assert_eq!(melee_stick(0, 9_175), (0, 9_175));
    assert_eq!(melee_stick(i16::MIN, 0), (-32_767, 0));
    // The radial clamp applies before the deadzone, as in Melee.
    let (x, y) = melee_stick(32_767, 32_767);
    assert_eq!((x, y), (23_169, 23_169));
    assert_eq!(melee_stick(9_200, 32_767).0, 0);
}
