use super::*;
use model::Button;

#[derive(Default)]
struct Recorded(Vec<String>);

impl Output for Recorded {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        self.0.push(format!("{} {name}", if down { "down" } else { "up" }));
        Ok(())
    }
    fn click(&mut self, _: bool, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn pointer(&mut self, _: f64, _: f64) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn the_fighter_keys_hold_a_one_frame_tap_two_frames_and_focus_loss_releases_it_at_once() {
    let mut driver = Driver::keys(model::PadPreset::Melee);
    let mut out = Recorded::default();
    let start = Instant::now();
    let neutral = InputView::default();
    let mut jump = InputView::default();
    jump.press(Button::X, true);
    driver.step(&neutral, true, 0.0, start, &mut out).unwrap();
    driver.step(&jump, true, 0.0, start, &mut out).unwrap();
    driver.step(&neutral, true, 0.0, start + Duration::from_micros(16_667), &mut out).unwrap();
    assert_eq!(out.0, ["down t"], "a one-frame tap released before two frames");
    assert_eq!(driver.next_release(), Some(start + crate::MIN_HOLD));
    driver.step(&neutral, true, 0.0, start + crate::MIN_HOLD, &mut out).unwrap();
    assert_eq!(out.0, ["down t", "up t"]);
    driver.step(&jump, true, 0.0, start + crate::MIN_HOLD * 2, &mut out).unwrap();
    driver.step(&neutral, true, 0.0, start + crate::MIN_HOLD * 2, &mut out).unwrap();
    driver.step(&neutral, false, 0.0, start + crate::MIN_HOLD * 2, &mut out).unwrap();
    assert_eq!(out.0, ["down t", "up t", "down t", "up t"], "focus loss releases a held tap at once");
}
