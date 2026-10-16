use crate::rng::{Rng, cases};
use super::*;
use model::{Control as Pad, Move, PadKind, PadPreset, RightStick, TriggerShield, TriggerShields};

const PRESETS: [PadPreset; 4] = [PadPreset::Melee, PadPreset::ZJump, PadPreset::Tom, PadPreset::Script];
const SHIELDS: [TriggerShield; 2] = [TriggerShield::Full, TriggerShield::Light];
const REMAPPABLE: [Pad; 9] = [Pad::A, Pad::B, Pad::X, Pad::Y, Pad::Lb, Pad::Rb, Pad::Lt, Pad::Rt, Pad::LeftStick];

struct Config {
    preset: PadPreset,
    tap_jump: bool,
    triggers: TriggerShields,
    right_stick: RightStick,
    kind: PadKind,
    remaps: model::Remaps,
}

/// The README's fighter layout per preset, by Xbox position.
fn reference_buttons(config: &Config) -> Vec<(Pad, KeyOrClick)> {
    use KeyOrClick::*;
    let shield = |control: Pad| {
        let mode = match control { Pad::Lt => config.triggers.left, Pad::Rt => config.triggers.right, _ => TriggerShield::Full };
        match (control, mode) {
            (_, TriggerShield::Light) => LightShield,
            (Pad::Rt, _) => RightShield,
            _ => Shield,
        }
    };
    let mut table: Vec<(Pad, KeyOrClick)> = match config.preset {
        PadPreset::Melee => vec![(Pad::A, Attack), (Pad::B, Special), (Pad::X, Jump), (Pad::Y, Jump), (Pad::Rb, Grab), (Pad::Lb, ExSpecial)],
        PadPreset::ZJump => vec![(Pad::A, Attack), (Pad::B, Special), (Pad::X, Grab), (Pad::Y, Jump), (Pad::Rb, Jump), (Pad::Lb, ExSpecial)],
        PadPreset::Tom => vec![(Pad::A, Attack), (Pad::B, Grab), (Pad::X, Special), (Pad::Y, Jump), (Pad::Lb, ShortHop), (Pad::Rb, ExSpecial)],
        PadPreset::Script => vec![(Pad::A, Attack), (Pad::X, Special), (Pad::B, Jump), (Pad::Y, Jump), (Pad::LeftStick, ShortHop), (Pad::Rb, Grab), (Pad::Lb, Walk)],
    };
    table.extend([(Pad::Lt, shield(Pad::Lt)), (Pad::Rt, shield(Pad::Rt)), (Pad::Start, Start)]);
    if config.kind == PadKind::GameCube {
        for (control, _) in &mut table {
            *control = match *control { Pad::B => Pad::X, Pad::X => Pad::B, other => other };
        }
    }
    table.retain(|(control, _)| !config.remaps.contains_key(control));
    table.extend(config.remaps.iter().filter_map(|(&control, step)| Some((control, match step {
        Move::Attack => Attack,
        Move::Special => Special,
        Move::Jump => Jump,
        Move::Grab => Grab,
        Move::Shield => shield(control),
        Move::Tilt => Walk,
        Move::ShortHop => ShortHop,
        Move::ExSpecial => ExSpecial,
        Move::None => return None,
    }))));
    table
}

fn reference_neutral(s: &Sample) -> bool {
    stick::melee_stick(s.left_x, s.left_y) == (0, 0)
        && stick::c_stick(s.right_x, s.right_y) == (0, 0)
        && s.left_trigger <= 4000
        && s.right_trigger <= 4000
        && !(s.a || s.b || s.x || s.y || s.lb || s.rb || s.start || s.left_stick)
}

/// What the fighter layout holds for an armed pad: buttons by the table, the
/// left stick through Melee's clamp, deadzone and down threshold, the right
/// stick at the C-stick flick thresholds, and stick jump above 0.6625 up
/// (capped at 0.65 while tilt and shield are both held).
fn reference_held(config: &Config, s: &Sample) -> BTreeSet<KeyOrClick> {
    let down = |control: Pad| match control {
        Pad::A => s.a, Pad::B => s.b, Pad::X => s.x, Pad::Y => s.y, Pad::Lb => s.lb, Pad::Rb => s.rb,
        Pad::Start => s.start, Pad::LeftStick => s.left_stick,
        Pad::Lt => s.left_trigger > 4000, Pad::Rt => s.right_trigger > 4000,
        _ => false,
    };
    let mut held: BTreeSet<KeyOrClick> = reference_buttons(config).into_iter().filter(|(c, _)| down(*c)).map(|(_, a)| a).collect();
    let (lx, ly) = stick::melee_stick(s.left_x, s.left_y);
    let (rx, ry) = stick::c_stick(s.right_x, s.right_y);
    let tilt = held.contains(&KeyOrClick::Walk);
    let shield = [KeyOrClick::Shield, KeyOrClick::RightShield, KeyOrClick::LightShield].iter().any(|a| held.contains(a));
    let up = -f32::from(ly) / 32_767.0;
    let up = if tilt && shield { up.min(0.65) } else { up };
    let directions = [
        (KeyOrClick::Jump, config.tap_jump && up > 0.6625),
        (KeyOrClick::Walk, config.right_stick == RightStick::Tilt && (rx, ry) != (0, 0)),
        (KeyOrClick::Left, lx < 0),
        (KeyOrClick::Right, lx > 0),
        (KeyOrClick::Down, stick::stick_down(ly)),
        (KeyOrClick::Up, ly < 0),
        (KeyOrClick::CLeft, rx < 0),
        (KeyOrClick::CRight, rx > 0),
        (KeyOrClick::CUp, ry < 0),
        (KeyOrClick::CDown, ry > 0),
    ];
    held.extend(directions.into_iter().filter(|(_, on)| *on).map(|(a, _)| a));
    held
}

fn diff(held: &BTreeSet<KeyOrClick>, next: &BTreeSet<KeyOrClick>) -> Vec<Transition> {
    let mut edges: Vec<Transition> = held.difference(next).map(|&action| Transition { action, pressed: false }).collect();
    edges.extend(next.difference(held).map(|&action| Transition { action, pressed: true }));
    edges
}

fn axis(rng: &mut Rng) -> i16 {
    let free = rng.next() as i16;
    rng.pick(&[0, 0, 0, 9174, -9174, 9175, 16384, -16384, 19660, -19660, 21708, -21708, 21709, -21709, 22937, -22937, 26500, -26500, 32767, i16::MIN, free])
}

fn trigger(rng: &mut Rng) -> i16 {
    rng.pick(&[0, 0, 4000, 4001, 20000, 32767])
}

fn sample(rng: &mut Rng) -> Sample {
    if rng.chance(40) {
        return Sample { left_x: rng.pick(&[0, 9174, -9174]), right_x: rng.pick(&[0, 16384, 22937]), right_y: rng.pick(&[0, 22937]), ..Sample::default() };
    }
    if rng.chance(25) {
        return Sample { left_y: rng.pick(&[-32767, -22500]), left_trigger: 20000, lb: rng.chance(70), right_x: rng.pick(&[0, 32767]), ..Sample::default() };
    }
    Sample {
        left_x: axis(rng), left_y: axis(rng), right_x: axis(rng), right_y: axis(rng),
        left_trigger: trigger(rng), right_trigger: trigger(rng),
        a: rng.chance(20), b: rng.chance(20), x: rng.chance(20), y: rng.chance(20),
        lb: rng.chance(20), rb: rng.chance(20), start: rng.chance(10), left_stick: rng.chance(10),
    }
}

fn key_name(action: KeyOrClick) -> String {
    match action.key() { ' ' => "space".to_owned(), key => key.to_string() }
}

#[test]
fn the_fighter_mapper_agrees_with_the_layout_reference_over_generated_sdl_events() {
    let mut rng = Rng::new(5);
    let axes = [Axis::LeftX, Axis::LeftY, Axis::RightX, Axis::RightY, Axis::TriggerLeft, Axis::TriggerRight];
    let buttons = [Button::South, Button::East, Button::West, Button::North, Button::LeftShoulder, Button::RightShoulder, Button::Start, Button::LeftStick, Button::Back, Button::DPadUp];
    for _ in 0..cases() {
        let preset = rng.pick(&PRESETS);
        let mut config = Config { preset, tap_jump: false, triggers: TriggerShields::default(), right_stick: RightStick::Smash, kind: PadKind::Xbox, remaps: Default::default() };
        let mut mapper = EventMapper::new(preset);
        config.tap_jump = rng.chance(50);
        mapper.set_tap_jump(config.tap_jump);
        let (mut sample, mut armed, mut held) = (Sample::default(), false, BTreeSet::new());
        for step in 0..40u64 {
            let roll = rng.below(100);
            let edges = if roll < 55 {
                let which = if rng.chance(90) { 1 } else { 2 };
                let event = if rng.chance(45) {
                    let axis = rng.pick(&axes);
                    let value = if matches!(axis, Axis::TriggerLeft | Axis::TriggerRight) { trigger(&mut rng) } else { self::axis(&mut rng) };
                    Event::GamepadAxisMotion { timestamp: step, which: JoystickId::from(which), axis, value }
                } else {
                    let button = rng.pick(&buttons);
                    if rng.chance(50) {
                        Event::GamepadButtonDown { timestamp: step, which: JoystickId::from(which), button }
                    } else {
                        Event::GamepadButtonUp { timestamp: step, which: JoystickId::from(which), button }
                    }
                };
                let Some(input) = selected_input(&event, JoystickId::from(1)) else {
                    assert_eq!(which, 2, "{event:?} from the selected pad was dropped");
                    continue;
                };
                assert_eq!(which, 1, "{event:?} from another pad was selected");
                assert_eq!(input.capture_ns, step);
                let eligible = rng.chance(95);
                let edges = mapper.apply(input, eligible);
                if !eligible || !armed {
                    sample = Sample::default();
                    armed = false;
                    assert_eq!(edges, diff(&held, &BTreeSet::new()));
                    held.clear();
                    continue;
                }
                let field = match (input.control, input.value) {
                    (Control::Axis(Axis::LeftX), InputValue::Axis(v)) => { sample.left_x = v; true }
                    (Control::Axis(Axis::LeftY), InputValue::Axis(v)) => { sample.left_y = v; true }
                    (Control::Axis(Axis::RightX), InputValue::Axis(v)) => { sample.right_x = v; true }
                    (Control::Axis(Axis::RightY), InputValue::Axis(v)) => { sample.right_y = v; true }
                    (Control::Axis(Axis::TriggerLeft), InputValue::Axis(v)) => { sample.left_trigger = v; true }
                    (Control::Axis(Axis::TriggerRight), InputValue::Axis(v)) => { sample.right_trigger = v; true }
                    (Control::Button(Button::South), InputValue::Button(v)) => { sample.a = v; true }
                    (Control::Button(Button::East), InputValue::Button(v)) => { sample.b = v; true }
                    (Control::Button(Button::West), InputValue::Button(v)) => { sample.x = v; true }
                    (Control::Button(Button::North), InputValue::Button(v)) => { sample.y = v; true }
                    (Control::Button(Button::LeftShoulder), InputValue::Button(v)) => { sample.lb = v; true }
                    (Control::Button(Button::RightShoulder), InputValue::Button(v)) => { sample.rb = v; true }
                    (Control::Button(Button::Start), InputValue::Button(v)) => { sample.start = v; true }
                    (Control::Button(Button::LeftStick), InputValue::Button(v)) => { sample.left_stick = v; true }
                    _ => false,
                };
                if !field {
                    assert!(edges.is_empty(), "an unmapped control pressed {edges:?}");
                    continue;
                }
                edges
            } else if roll < 72 {
                sample = if rng.chance(50) { Sample::default() } else { self::sample(&mut rng) };
                let eligible = rng.chance(85);
                let edges = mapper.resync(&sample, eligible);
                if !eligible {
                    armed = false;
                    assert_eq!(edges, diff(&held, &BTreeSet::new()));
                    held.clear();
                    continue;
                }
                if !armed {
                    armed = reference_neutral(&sample);
                    assert!(edges.is_empty(), "pressed while arming: {edges:?}");
                    continue;
                }
                edges
            } else if roll < 82 {
                sample = Sample::default();
                armed = false;
                assert_eq!(mapper.disarm(), diff(&held, &BTreeSet::new()));
                held.clear();
                continue;
            } else {
                match rng.below(5) {
                    0 => { config.tap_jump = rng.chance(50); mapper.set_tap_jump(config.tap_jump) }
                    3 => { config.right_stick = rng.pick(&RightStick::ALL); mapper.set_right_stick(config.right_stick) }
                    1 => { config.triggers = TriggerShields { left: rng.pick(&SHIELDS), right: rng.pick(&SHIELDS) }; mapper.set_trigger_shields(config.triggers) }
                    2 => { config.kind = rng.pick(&[PadKind::Xbox, PadKind::GameCube]); mapper.set_pad_kind(config.kind) }
                    _ => {
                        config.remaps = (0..rng.below(4)).map(|_| (rng.pick(&REMAPPABLE), rng.pick(&Move::ALL))).collect();
                        mapper.set_remaps(config.remaps.clone())
                    }
                }
                continue;
            };
            let next = reference_held(&config, &sample);
            assert_eq!(edges, diff(&held, &next), "{:?} {:?} {:?} {:?} {sample:?}", config.preset, config.kind, config.triggers, config.remaps);
            held = next;
            assert_eq!(mapper.armed(), armed);
            assert_eq!(mapper.sample(), &sample);
        }
    }

    // The keys pressed are the keys the window shows for the same controls.
    let mut keys = BTreeSet::new();
    let all = [KeyOrClick::Attack, KeyOrClick::Special, KeyOrClick::Jump, KeyOrClick::ShortHop, KeyOrClick::Grab, KeyOrClick::Shield, KeyOrClick::RightShield, KeyOrClick::LightShield, KeyOrClick::Walk, KeyOrClick::ExSpecial,
        KeyOrClick::Start, KeyOrClick::Left, KeyOrClick::Right, KeyOrClick::Down, KeyOrClick::Up, KeyOrClick::CLeft, KeyOrClick::CRight, KeyOrClick::CUp, KeyOrClick::CDown];
    for action in all {
        assert!(keys.insert(action.key()), "{action:?} shares its key");
    }
    let shown = |control: Pad| model::fighter_bindings().into_iter().find(|b| b.control == control).map(|b| b.press);
    for (control, action) in [(Pad::LeftLeft, KeyOrClick::Left), (Pad::LeftRight, KeyOrClick::Right), (Pad::LeftDown, KeyOrClick::Down), (Pad::LeftUp, KeyOrClick::Up),
        (Pad::RightUp, KeyOrClick::CUp), (Pad::RightDown, KeyOrClick::CDown), (Pad::RightLeft, KeyOrClick::CLeft), (Pad::RightRight, KeyOrClick::CRight), (Pad::Start, KeyOrClick::Start)] {
        assert_eq!(shown(control), Some(model::Press::Key(key_name(action))), "{control:?}");
    }
    let named: BTreeSet<String> = all.into_iter().map(key_name).collect();
    for preset in PRESETS {
        for left in SHIELDS {
            for right in SHIELDS {
                for binding in RightStick::ALL.into_iter().flat_map(|mode| model::fighter_bindings_with(preset, TriggerShields { left, right }, mode)) {
                    let model::Press::Key(key) = binding.press else { panic!("{binding:?}") };
                    assert!(named.contains(&key), "{} {key}", preset.name());
                }
            }
        }
    }
}
