//! What the controller service knows, the messages on its local interface, and
//! the plain-language status a window shows for it, and the built-in binding
//! tables. No I/O: the service and every window share it.
#![forbid(unsafe_code)]

pub mod any_map;
pub mod pad;

mod bindings;
mod settings;
mod status;
mod view;
mod wire;

pub use bindings::*;
pub use settings::*;
pub use status::*;
pub use view::*;
pub use wire::*;

#[cfg(test)]
pub(crate) mod rng {
    pub struct Rng(u64);

    impl Rng {
        pub fn new(salt: u64) -> Self {
            Self(std::env::var("PROPERTY_SEED").ok().and_then(|seed| seed.parse().ok()).unwrap_or(0x5eed_u64) ^ salt)
        }
        pub fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        pub fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        pub fn pick<T: Clone>(&mut self, items: &[T]) -> T {
            items[self.below(items.len())].clone()
        }
        pub fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
    }

    pub fn cases() -> usize {
        std::env::var("PROPERTY_CASES").ok().and_then(|cases| cases.parse().ok()).unwrap_or(300)
    }
}

#[cfg(test)]
mod properties {
    use super::rng::{Rng, cases};
    use super::*;

    const CONTROLS: [Control; 23] = [
        Control::LeftStick, Control::A, Control::B, Control::X, Control::Y, Control::Lb, Control::Rb, Control::Lt, Control::Rt,
        Control::Start, Control::Back, Control::DpadUp, Control::DpadDown, Control::DpadLeft, Control::DpadRight,
        Control::LeftUp, Control::LeftDown, Control::LeftLeft, Control::LeftRight, Control::RightUp, Control::RightDown, Control::RightLeft, Control::RightRight,
    ];
    const PRESETS: [PadPreset; 4] = [PadPreset::Melee, PadPreset::ZJump, PadPreset::Tom, PadPreset::Script];
    const SHIELDS: [TriggerShield; 2] = [TriggerShield::Full, TriggerShield::Light];
    const KINDS: [PadKind; 2] = [PadKind::Xbox, PadKind::GameCube];

    fn right_bindings(preset: PadPreset, mode: RightStick, triggers: TriggerShields, kind: PadKind) -> Vec<Binding> {
        pad_bindings(preset, triggers, mode, kind).into_iter().filter(|b| matches!(b.control, Control::RightUp | Control::RightDown | Control::RightLeft | Control::RightRight)).collect()
    }

    fn remaps(rng: &mut Rng) -> Remaps {
        (0..rng.below(5)).map(|_| (rng.pick(&REMAPPABLE), rng.pick(&Move::ALL))).collect()
    }

    fn settings(rng: &mut Rng) -> ControllerSettings {
        ControllerSettings {
            pad_preset: rng.pick(&PRESETS),
            tap_jump: rng.chance(50),
            triggers: TriggerShields { left: rng.pick(&SHIELDS), right: rng.pick(&SHIELDS) },
            right_stick: rng.pick(&RightStick::ALL),
            remaps: remaps(rng),
        }
    }

    fn snapshot(rng: &mut Rng) -> Snapshot {
        let profile = rng.pick(&Profile::ALL);
        Snapshot {
            pad: rng.chance(70).then(|| Pad { name: "Pad".into(), id: "pad-1".into() }),
            game: rng.chance(70).then(|| Game { pid: 4242, window: rng.chance(70) }),
            session: rng.chance(60).then(|| Session {
                map: "Arena".into(),
                phase: rng.pick(&[Phase::Lobby, Phase::CharacterSelect, Phase::StageSelect, Phase::Match, Phase::Results]),
                player: rng.chance(50).then_some(2),
            }),
            profile,
            choice: rng.pick(&[ProfileChoice::Auto, ProfileChoice::Map, ProfileChoice::AnyMap, ProfileChoice::Off]),
            map: rng.chance(60).then(|| "Arena".into()),
            output: Output { running: rng.chance(80), ready: rng.chance(80), focused: rng.chance(70) },
            problem: rng.chance(15).then(|| "Something stopped.".into()),
            settings: settings(rng),
        }
    }

    #[test]
    fn the_view_follows_the_light_rules_for_every_generated_status() {
        let mut rng = Rng::new(1);
        for _ in 0..cases() * 4 {
            let s = snapshot(&mut rng);
            let link = rng.pick(&[Link::Off, Link::Starting, Link::Connected]);
            let v = view(link, &s);
            let lights = v.rows.each_ref().map(|r| r.light);
            let expected = match link {
                Link::Off => [Light::Off; 3],
                Link::Starting => [Light::Amber; 3],
                Link::Connected => {
                    let session_ready = s.profile != Profile::Off
                        && s.game.is_some()
                        && !(s.profile == Profile::Map && s.session.is_none())
                        && s.output.running && s.output.ready && s.output.focused;
                    [
                        if s.pad.is_some() { Light::Green } else { Light::Off },
                        if s.game.as_ref().is_some_and(|g| g.window) { Light::Green } else { Light::Amber },
                        if session_ready { Light::Green } else { Light::Amber },
                    ]
                }
            };
            assert_eq!(lights, expected, "{link:?} {s:?}");
            let problem = if link == Link::Connected { s.problem.clone() } else { None };
            assert_eq!(v.problem, problem);
            let counted = if link == Link::Connected && s.pad.is_none() { &lights[1..] } else { &lights[..] };
            let overall = if problem.is_some() || counted.contains(&Light::Red) {
                Light::Red
            } else if counted.contains(&Light::Amber) {
                Light::Amber
            } else if counted.contains(&Light::Off) {
                Light::Off
            } else {
                Light::Green
            };
            assert_eq!(v.overall, overall, "{link:?} {s:?}");
            assert_eq!(v.rows[2].label, if s.profile == Profile::Off { "Controller output" } else { "Match" });
            for r in &v.rows {
                let t = format!("{} {}", r.label, r.text).to_lowercase();
                for word in ["epoch", "journal", "x11", "slot", "pid", "helper", "socket", "evdev", "service", "4242"] {
                    assert!(!t.contains(word), "{t:?} names {word}");
                }
            }
            let session = rng.chance(50);
            let choice = s.choice;
            let resolved = match choice {
                ProfileChoice::Auto => if session { Profile::Map } else { Profile::AnyMap },
                ProfileChoice::Map => Profile::Map,
                ProfileChoice::AnyMap => Profile::AnyMap,
                ProfileChoice::Off => Profile::Off,
            };
            assert_eq!(choice.resolve(session), resolved);
        }
    }

    #[test]
    fn settings_remaps_and_messages_round_trip() {
        let mut rng = Rng::new(2);
        for _ in 0..cases() {
            let mut settings = settings(&mut rng);
            let remaps = settings.remaps.clone();
            assert_eq!(parse_remaps(&remaps_arg(&remaps)), Ok(remaps.clone()));
            for preset in PRESETS {
                assert_eq!(PadPreset::parse(preset.name()), Ok(preset));
            }
            for shield in SHIELDS {
                assert_eq!(TriggerShield::parse(shield.name()), Ok(shield));
            }
            for mode in RightStick::ALL {
                assert_eq!(RightStick::parse(mode.name()), Ok(mode));
            }
            let saved = serde_json::to_value(&settings).unwrap();
            assert_eq!(serde_json::from_value::<ControllerSettings>(saved.clone()).unwrap(), settings);
            let mut partial = saved.as_object().unwrap().clone();
            let dropped = rng.pick(&["pad_preset", "tap_jump", "triggers", "right_stick", "remaps"]);
            partial.remove(dropped);
            let loaded: ControllerSettings = serde_json::from_value(serde_json::Value::Object(partial)).unwrap();
            let defaults = ControllerSettings::default();
            match dropped {
                "pad_preset" => assert_eq!(loaded.pad_preset, defaults.pad_preset),
                "tap_jump" => assert_eq!(loaded.tap_jump, defaults.tap_jump),
                "triggers" => assert_eq!(loaded.triggers, defaults.triggers),
                "right_stick" => assert_eq!(loaded.right_stick, RightStick::Smash),
                _ => assert_eq!(loaded.remaps, defaults.remaps),
            }

            let unremappable = rng.pick(&[Control::LeftUp, Control::Start, Control::DpadDown, Control::RightLeft]);
            let mut refused = remaps.clone();
            refused.insert(unremappable, rng.pick(&Move::ALL));
            let before = settings.clone();
            assert!(!settings.apply(&ClientMessage::Remaps(refused.clone())));
            assert_eq!(settings, before);
            assert!(parse_remaps(&remaps_arg(&refused)).is_err());
            let next = self::remaps(&mut rng);
            assert_eq!(settings.apply(&ClientMessage::Remaps(next.clone())), next != before.remaps);
            assert_eq!(settings.remaps, next);
            let tap = rng.chance(50);
            let changed = tap != settings.tap_jump;
            assert_eq!(settings.apply(&ClientMessage::TapJump(tap)), changed);
            assert_eq!(settings.tap_jump, tap);
            let mode = rng.pick(&RightStick::ALL);
            let changed = mode != settings.right_stick;
            assert_eq!(settings.apply(&ClientMessage::RightStick(mode)), changed);
            assert_eq!(settings.right_stick, mode);

            let kind = rng.pick(&KINDS);
            let plain = pad_bindings(settings.pad_preset, settings.triggers, settings.right_stick, kind);
            let bound = settings.bindings(kind);
            for control in CONTROLS {
                let of = |bindings: &[Binding]| bindings.iter().filter(|b| b.control == control).cloned().collect::<Vec<_>>();
                match settings.remaps.get(&control) {
                    None => assert_eq!(of(&bound), of(&plain), "{control:?}"),
                    Some(Move::None) => assert!(of(&bound).is_empty(), "{control:?}"),
                    Some(step) => {
                        let actions = of(&bound);
                        assert_eq!(actions.len(), 1, "{control:?}");
                        let preset_of = |action: &str| fighter_bindings_with(PadPreset::Script, settings.triggers, settings.right_stick).into_iter().chain(fighter_bindings_with(PadPreset::Tom, settings.triggers, settings.right_stick))
                            .find(|b| b.action == action).map(|b| b.press);
                        let expected = match step {
                            Move::Attack => preset_of("Attack"),
                            Move::Special => preset_of("Special"),
                            Move::Jump => preset_of("Jump"),
                            Move::Grab => preset_of("Grab"),
                            Move::Tilt => preset_of("Tilt"),
                            Move::ShortHop => preset_of("Short hop"),
                            Move::Strong => preset_of("Strong"),
                            Move::Shield | Move::None => None,
                        };
                        if let Some(press) = expected {
                            assert_eq!(actions[0].press, press, "{control:?} {step:?}");
                        }
                    }
                }
            }
            for bindings in [bound, any_map_bindings(), menu_pointer_bindings()] {
                let mut seen = std::collections::BTreeSet::new();
                for b in &bindings {
                    assert!(seen.insert(b.control), "{:?} bound twice", b.control);
                }
            }
            for preset in PRESETS {
                let triggers = TriggerShields { left: rng.pick(&SHIELDS), right: rng.pick(&SHIELDS) };
                let printed = |kind: PadKind| -> std::collections::BTreeSet<(&'static str, String)> {
                    pad_bindings(preset, triggers, settings.right_stick, kind).into_iter().map(|b| {
                        let label = match (kind, kind.label(b.control)) {
                            (PadKind::GameCube, "Z") => "RB",
                            (PadKind::GameCube, "L") => "LT",
                            (PadKind::GameCube, "R") => "RT",
                            (_, label) => label,
                        };
                        (label, b.action)
                    }).collect()
                };
                assert_eq!(printed(PadKind::GameCube), printed(PadKind::Xbox), "a GameCube pad keeps its printed letters in {}", preset.name());
                let right = |mode| right_bindings(preset, mode, triggers, kind);
                let (smash, tilt) = (right(RightStick::Smash), right(RightStick::Tilt));
                for mode in RightStick::ALL {
                    assert_eq!(right(mode), right_bindings(PadPreset::Melee, mode, triggers, kind), "the right stick ignores the preset in {}", preset.name());
                }
                assert_eq!(smash.len(), 4);
                for (s, t) in smash.iter().zip(&tilt) {
                    assert_eq!((s.control, &s.press), (t.control, &t.press), "only the label follows the right stick mode");
                    assert!(s.action.to_lowercase().contains("smash") && t.action.to_lowercase().contains("tilt"), "{s:?} {t:?}");
                }
            }

            let s = snapshot(&mut rng);
            let mut input = InputView { left: [rng.next() as i16, rng.next() as i16], right: [rng.next() as i16, 0], lt: rng.below(32768) as u16, rt: 0, buttons: 0 };
            let mut reference = 0u32;
            for _ in 0..6 {
                let button = rng.pick(&[Button::A, Button::B, Button::X, Button::Y, Button::Lb, Button::Rb, Button::Start, Button::Back, Button::LeftStick, Button::RightStick, Button::DpadUp, Button::DpadDown, Button::DpadLeft, Button::DpadRight]);
                let down = rng.chance(50);
                input.press(button, down);
                let bit = 1u32 << (button as u32);
                if down { reference |= bit } else { reference &= !bit }
                assert_eq!(input.buttons, reference);
                assert_eq!(input.pressed(button), down);
            }
            for message in [ServiceMessage::Status(s), ServiceMessage::Input(input)] {
                let line = message.line();
                assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "{line:?}");
                assert_eq!(ServiceMessage::parse(&line).unwrap(), message);
            }
            for message in [
                ClientMessage::Profile(rng.pick(&[ProfileChoice::Auto, ProfileChoice::Map, ProfileChoice::AnyMap, ProfileChoice::Off])),
                ClientMessage::PadPreset(rng.pick(&PRESETS)),
                ClientMessage::TapJump(rng.chance(50)),
                ClientMessage::RightStick(rng.pick(&RightStick::ALL)),
                ClientMessage::Remaps(self::remaps(&mut rng)),
                ClientMessage::Bindings(any_map_bindings()),
            ] {
                let line = message.line();
                assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "{line:?}");
                assert_eq!(ClientMessage::parse(&line).unwrap(), message);
            }
        }
    }
}
