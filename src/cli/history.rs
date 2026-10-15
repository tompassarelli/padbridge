use padbridge::Sample;
use sdl3::{
    event::Event,
    gamepad::{Axis, Button, Gamepad},
};
use std::time::Instant;

pub fn sample(pad: &Gamepad) -> Sample {
    Sample {
        left_x: pad.axis(Axis::LeftX),
        left_y: pad.axis(Axis::LeftY),
        right_x: pad.axis(Axis::RightX),
        right_y: pad.axis(Axis::RightY),
        left_trigger: pad.axis(Axis::TriggerLeft),
        right_trigger: pad.axis(Axis::TriggerRight),
        a: pad.button(Button::South),
        b: pad.button(Button::East),
        x: pad.button(Button::West),
        y: pad.button(Button::North),
        lb: pad.button(Button::LeftShoulder),
        rb: pad.button(Button::RightShoulder),
        start: pad.button(Button::Start),
        left_stick: pad.button(Button::LeftStick),
    }
}

pub fn event_control(input: padbridge::CapturedInput) -> (&'static str, String) {
    use padbridge::{Control, InputValue};
    match (input.control, input.value) {
        (Control::Axis(axis), InputValue::Axis(value)) => ("axis", format!("{axis:?}={value}")),
        (Control::Button(button), InputValue::Button(value)) => {
            ("button", format!("{button:?}={value}"))
        }
        _ => ("unknown", "unknown".into()),
    }
}

pub fn clock_ns(start: Instant) -> u128 {
    start.elapsed().as_nanos()
}

pub fn next_event_id(next: &mut u64) -> u64 {
    *next = next.checked_add(1).expect("controller event ID exhausted");
    *next
}

pub fn event_timestamp(event: &Event) -> u64 {
    match event {
        Event::GamepadRemoved { timestamp, .. } | Event::GamepadRemapped { timestamp, .. } => {
            *timestamp
        }
        _ => 0,
    }
}

pub fn history_event(
    id: u64,
    capture_ns: u64,
    dequeue_ns: u128,
    control: &str,
    value: &str,
    disposition: &str,
) {
    println!("event\t{id}\t{capture_ns}\t{dequeue_ns}\t\t{control}\t{value}\t\t\t{disposition}");
}

pub fn history_transition(
    id: u64,
    capture_ns: u64,
    dequeue_ns: u128,
    submit_ns: Option<u128>,
    control: &str,
    value: &str,
    action: padbridge::KeyOrClick,
    pressed: bool,
    disposition: &str,
) {
    let submit_ns = submit_ns.map_or_else(String::new, |value| value.to_string());
    println!(
        "transition\t{id}\t{capture_ns}\t{dequeue_ns}\t{submit_ns}\t{control}\t{value}\t{action:?}\t{pressed}\t{disposition}"
    );
}
