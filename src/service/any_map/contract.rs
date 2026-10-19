use super::*;
use model::{Button, Control, Move, PadKind, PadPreset, Press, TriggerShield, TriggerShields};
use std::collections::{BTreeMap, BTreeSet};

#[derive(serde::Deserialize)]
struct Contract {
    /// The model's action label to the Smashcraft action its key must reach.
    map_actions: BTreeMap<String, String>,
}

// Smashcraft's Action order, presets, pad layer and key saves: smashcraft:ts/src/game/input/actions.ts, keyBindings.ts
// (presetBindings, inputAction, decodeBindings, upgradeDefaults) and padKeys.ts; Y starts the match through its own trigger in shell.ts.
const ACTIONS: [&str; 18] = ["moveLeft", "moveRight", "moveDown", "moveUp", "jump", "attack", "special", "grab", "leftTrigger",
    "rightTrigger", "smashLeft", "smashRight", "smashUp", "smashDown", "walk", "lightShield", "shortHop", "exSpecial"];

const PAD_LAYER: [(u8, &str); 18] = [(b'C', "attack"), (b'V', "special"), (b'T', "jump"), (b'1', "shortHop"), (b'2', "grab"),
    (b'3', "leftTrigger"), (b'4', "rightTrigger"), (b'5', "lightShield"), (b'6', "walk"), (192, "exSpecial"), (219, "moveLeft"),
    (221, "moveRight"), (186, "moveUp"), (222, "moveDown"), (188, "smashLeft"), (190, "smashRight"), (189, "smashUp"), (220, "smashDown")];

fn code(key: &str) -> u8 {
    match key {
        "space" => 32, "`" => 192, "[" => 219, "]" => 221, ";" => 186, "'" => 222, "," => 188, "." => 190, "-" => 189, "\\" => 220,
        _ => key.to_ascii_uppercase().as_bytes()[0],
    }
}

fn esdf() -> Vec<[u8; 2]> {
    vec![[b'S', 0], [b'F', 0], [b'D', 0], [b'E', 0], [32, b'8'], [b'N', 0], [b'U', 0], [b'O', 0], [b'A', 0], [b'7', 0],
        [b'B', 0], [b'M', 0], [b'J', 0], [b'H', 0], [b'P', 0], [b'0', 0], [b'I', 0], [b'9', 0]]
}

fn hitbox() -> Vec<[u8; 2]> {
    vec![[b'W', 0], [b'R', 0], [b'E', 0], [32, 0], [b'8', 0], [b'N', 0], [b'U', 0], [b'O', 0], [b'Q', 0], [b'7', 0],
        [b'B', 0], [b'M', 0], [b'J', 0], [b'H', 0], [b'P', 0], [b'0', 0], [b'I', 0], [b'9', 0]]
}

fn custom() -> Vec<[u8; 2]> {
    vec![[b'W', 0], [b'R', 0], [b'E', 0], [32, 0], [b'Z', b'9'], [b'N', 0], [b'U', 0], [b'O', 0], [b'Q', 0], [b'8', b'G'],
        [b'B', 191], [b'M', 0], [b'J', 0], [b'H', 0], [b'P', 0], [b'A', 0], [b'I', 0], [b'0', b'X']]
}

fn saved(file: &str) -> Vec<[u8; 2]> {
    let (version, mut keys) = decode_saved(file);
    fill_second_slots(&mut keys, version);
    upgrade_legacy_layout(&mut keys);
    keys
}

fn decode_saved(file: &str) -> (&str, Vec<[u8; 2]>) {
    let start = file.find("\"K").unwrap() + 1;
    let digits: String = file[start + 2..].chars().take_while(char::is_ascii_digit).collect();
    let mut keys = vec![[0u8; 2]; ACTIONS.len()];
    for (index, chunk) in digits.as_bytes().chunks(3).enumerate() {
        let key: u8 = std::str::from_utf8(chunk).unwrap().parse().unwrap();
        if key != b'Y' { keys[index / 2][index % 2] = key; }
    }
    (&file[start..start + 2], keys)
}

fn action(name: &str) -> usize {
    ACTIONS.iter().position(|a| *a == name).unwrap()
}

fn free(keys: &[[u8; 2]], candidates: &[u8]) -> bool {
    candidates.iter().all(|c| !keys.iter().flatten().any(|k| k == c))
}

fn fill_second_slots(keys: &mut [[u8; 2]], version: &str) {
    if keys[action("lightShield")][1] == 0 && free(keys, b"T") { keys[action("lightShield")][1] = b'T'; }
    if keys[action("exSpecial")][1] == 0 && free(keys, b"X") { keys[action("exSpecial")][1] = b'X'; }
    if keys[action("rightTrigger")][1] == 0 && free(keys, b"G") { keys[action("rightTrigger")][1] = b'G'; }
    if keys[action("shortHop")][0] == 0 && free(keys, b"Z") { keys[action("shortHop")][0] = b'Z'; }
    upgrade_renamed_keys(keys, version);
}

fn upgrade_renamed_keys(keys: &mut [[u8; 2]], version: &str) {
    if version == "K1" && free(keys, b"P") { keys[action("walk")][0] = b'P'; }
    if keys[action("grab")][0] == b'L' && free(keys, b"O") { keys[action("grab")][0] = b'O'; }
    if keys[action("walk")][0] == 186 && free(keys, b"P") { keys[action("walk")][0] = b'P'; }
}

fn upgrade_legacy_layout(keys: &mut [[u8; 2]]) {
    if keys[action("moveLeft")][0] == b'S' && keys[action("moveRight")][0] == b'F' && keys[action("moveDown")][0] == b'D'
        && keys[action("leftTrigger")][0] == b'A' && free(keys, b"QWER") {
        for (name, key) in [("moveLeft", b'W'), ("moveRight", b'R'), ("moveDown", b'E'), ("leftTrigger", b'Q')] { keys[action(name)][0] = key; }
    }
    if keys[action("jump")][0] == b'I' && keys[action("shortHop")][0] == b'Z' {
        keys[action("jump")][0] = b'Z';
        keys[action("shortHop")][0] = b'I';
    }
    if [0, b'9', b'0'].contains(&keys[action("lightShield")][0]) && keys[action("lightShield")][1] == b'T' && free(keys, b"A") {
        keys[action("lightShield")] = [b'A', 0];
    }
}

fn bound(keys: &[[u8; 2]], key: &str) -> Option<&'static str> {
    if key == "y" { return Some("start"); }
    let code = code(key);
    keys.iter().position(|slots| slots.contains(&code)).map(|index| ACTIONS[index])
        .or_else(|| PAD_LAYER.iter().find(|(pad, _)| *pad == code).map(|&(_, action)| action))
}

fn key(press: &Press) -> Option<String> {
    match press {
        Press::Key(name) => Some(name.clone()),
        Press::LeftClick => Some("left-click".into()),
        Press::RightClick => Some("right-click".into()),
        Press::Pointer => None,
    }
}

const PRESETS: [PadPreset; 3] = PadPreset::ALL;
const SHIELDS: [TriggerShield; 2] = [TriggerShield::Full, TriggerShield::Light];

#[test]
fn every_preset_key_reaches_its_labelled_action_through_the_maps_pad_layer_under_each_keyboard_preset_and_toms_saved_bindings() {
    let contract: Contract = serde_json::from_str(include_str!("../../../contract/bindings.json")).unwrap();
    let sets = [("ESDF", esdf()), ("Hitbox", hitbox()), ("Custom", custom()), ("tom-saved-bindings.pld", saved(include_str!("../../../contract/tom-saved-bindings.pld")))];
    for (set, keys) in &sets {
        for preset in PRESETS {
            for (left, right) in SHIELDS.into_iter().flat_map(|left| SHIELDS.map(|right| (left, right))) {
                for binding in model::RightStick::ALL.into_iter().flat_map(|mode| model::fighter_bindings_with(preset, TriggerShields { left, right }, mode)) {
                    let Press::Key(sent) = &binding.press else { continue };
                    let wanted = contract.map_actions.get(&binding.action)
                        .unwrap_or_else(|| panic!("{} {:?} is labelled {:?}, which the contract leaves out", preset.name(), binding.control, binding.action));
                    if *set != "tom-saved-bindings.pld" && sent != "y" {
                        assert!(!keys.iter().flatten().any(|&key| key == code(sent)), "{} {:?} sends {sent:?}, which {set} binds as a keyboard key", preset.name(), binding.control);
                    }
                    let reached = bound(keys, sent);
                    let ok = match wanted.as_str() { "shield" => matches!(reached, Some("leftTrigger" | "rightTrigger")), action => reached == Some(action) };
                    assert!(ok, "{} {:?} {:?} sends {sent:?}, which {set} binds to {reached:?}", preset.name(), binding.control, binding.action);
                }
            }
        }
    }
}

#[derive(Default)]
struct Recorded {
    held: BTreeSet<String>,
    moved: bool,
}

impl Output for Recorded {
    fn key(&mut self, name: &str, down: bool) -> Result<(), String> {
        self.press(name.to_owned(), down);
        Ok(())
    }
    fn click(&mut self, right: bool, down: bool) -> Result<(), String> {
        self.press(if right { "right-click" } else { "left-click" }.to_owned(), down);
        Ok(())
    }
    fn pointer(&mut self, _: f64, _: f64) -> Result<(), String> {
        self.moved = true;
        Ok(())
    }
}

impl Recorded {
    fn press(&mut self, name: String, down: bool) {
        if down {
            assert!(self.held.insert(name.clone()), "{name} pressed twice");
        } else {
            assert!(self.held.remove(&name), "{name} released while up");
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const BUTTONS: [(Control, Button); 9] = [
    (Control::A, Button::A), (Control::B, Button::B), (Control::X, Button::X), (Control::Y, Button::Y), (Control::Lb, Button::Lb),
    (Control::Rb, Button::Rb), (Control::Start, Button::Start), (Control::Back, Button::Back), (Control::LeftStick, Button::LeftStick),
];
const LEFT: [Control; 4] = [Control::LeftLeft, Control::LeftRight, Control::LeftUp, Control::LeftDown];
const RIGHT: [Control; 4] = [Control::RightLeft, Control::RightRight, Control::RightUp, Control::RightDown];

/// A pad with some controls clearly held: buttons, triggers past their threshold, and each stick neutral or tilted to one side.
fn generate(rng: &mut Rng) -> (InputView, BTreeSet<Control>) {
    let mut input = InputView::default();
    let mut held = BTreeSet::new();
    for (control, button) in BUTTONS {
        if rng.below(4) == 0 {
            input.press(button, true);
            held.insert(control);
        }
    }
    for (control, value) in [(Control::Lt, &mut input.lt), (Control::Rt, &mut input.rt)] {
        *value = if rng.below(3) == 0 { held.insert(control); 4001 + rng.below(28767) as u16 } else { rng.below(4001) as u16 };
    }
    for (directions, stick) in [(LEFT, &mut input.left), (RIGHT, &mut input.right)] {
        let pick = rng.below(6) as usize;
        let Some(&direction) = directions.get(pick) else { continue };
        let tilt = 29491 + rng.below(3277) as i16;
        *stick = match pick { 0 => [-tilt, 0], 1 => [tilt, 0], 2 => [0, -tilt], _ => [0, tilt] };
        held.insert(direction);
    }
    (input, held)
}

#[test]
fn the_service_presses_the_models_bindings_on_every_screen() {
    let mut rng = Rng(std::env::var("PROPERTY_SEED").ok().and_then(|seed| seed.parse().ok()).unwrap_or(0xc0ffee));
    let mut clock = Instant::now();
    let mut tick = || { clock += crate::MIN_HOLD; clock };
    for preset in PRESETS {
        let triggers = TriggerShields { left: SHIELDS[rng.below(2) as usize], right: SHIELDS[rng.below(2) as usize] };
        let right_stick = model::RightStick::ALL[rng.below(2) as usize];
        let settings = model::ControllerSettings { pad_preset: preset, triggers, right_stick, ..Default::default() };
        let tilt = model::remap(Vec::new(), &[(Control::Lb, Move::Tilt)].into(), triggers).remove(0).press;
        let fight = settings.bindings(PadKind::Xbox);
        let menus = menu_bindings(true);
        for (screen, table) in [("match", &fight), ("menus", &menus)] {
            let mut driver = match screen {
                "match" => Driver::keys_for(&settings, crate::pad_ingress::Route::Digital),
                _ => Driver::menu(MenuCurve::for_window_width(2560.0), true),
            };
            let mut out = Recorded::default();
            driver.step(&InputView::default(), true, 0.0, tick(), &mut out).unwrap();
            for _ in 0..400 {
                let (input, controls) = generate(&mut rng);
                out.moved = false;
                driver.step(&input, true, 0.01, tick(), &mut out).unwrap();
                let mut presses: Vec<&Press> = table.iter().filter(|b| controls.contains(&b.control)).map(|b| &b.press).collect();
                if screen == "match" && right_stick == model::RightStick::Tilt && controls.iter().any(|c| RIGHT.contains(c)) {
                    presses.push(&tilt);
                }
                let keys: BTreeSet<String> = presses.iter().filter_map(|p| key(p)).collect();
                let pointer = screen == "menus" && controls.iter().any(|c| LEFT.contains(c));
                assert_eq!(out.held, keys, "{} {screen}: holding {controls:?}", preset.name());
                assert_eq!(out.moved, pointer, "{} {screen}: pointer while holding {controls:?}", preset.name());
                driver.step(&InputView::default(), true, 0.0, tick(), &mut out).unwrap();
                assert!(out.held.is_empty(), "{} {screen}: {:?} still held at neutral", preset.name(), out.held);
            }
        }
    }
    let shields: Vec<_> = model::fighter_bindings_with(PadPreset::Tom, TriggerShields::default(), model::RightStick::default()).into_iter()
        .filter(|b| matches!(b.control, Control::Lt | Control::Rt)).map(|b| b.action).collect();
    assert_eq!(shields, ["Shield", "Right shield"], "LT and RT both shield");
    for preset in [PadPreset::Melee, PadPreset::ZJump] {
        let settings = model::ControllerSettings { pad_preset: preset, ..Default::default() };
        let mut driver = Driver::keys_for(&settings, crate::pad_ingress::Route::Digital);
        let mut out = Recorded::default();
        for input in [InputView::default(), InputView { lt: 20000, ..Default::default() }, InputView { lt: 20000, rt: 20000, ..Default::default() }] {
            driver.step(&input, true, 0.0, tick(), &mut out).unwrap();
        }
        assert_eq!(out.held, ["3", "4"].map(String::from).into(), "{}: RT pressed while LT is held", preset.name());
    }
    assert!(menu_bindings(true).iter().any(|b| b.control == Control::Start && b.press == Press::Key("y".into())), "Start presses Y in menus");
}
