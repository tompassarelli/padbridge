//! The Smashcraft profile: the map publishes
//! `CustomMapData/smashcraft-journal-menu-BUILD-sSLOT.txt` for the local
//! player, naming its build, slot, epoch and menu phase; `wc3-journal
//! --follow-matches` serves it (smashcraft:companion/README.md).
//!
//! Within one map session the epoch only grows, one per match. A newer
//! publication with a lower epoch, or another build or slot, is a new map
//! session (the map was opened again), which a running helper would ignore,
//! so it gets a new session key and a fresh helper.
//!
//! A build that reads keyboard input (the playable build, #166)
//! also publishes menus. Its ready file `CustomMapData/wc3-melee-ready.txt`,
//! written at fighter selection, names the build and its keyboard profile: that
//! session plays on keys, so the service runs no helper and presses the
//! controller's keys itself ([`super::any_map::Mode::Keys`]).

use super::{Game, HelperEvent, Pad, Profile, Session, Target};
use crate::model;
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    pub build: String,
    pub slot: u32,
    pub epoch: u32,
    pub phase: String,
    pub modified: SystemTime,
}

/// The newest complete menu publication in a CustomMapData folder.
pub fn newest_menu(dir: &Path) -> Option<Menu> {
    let (modified, path, build, slot) = fs::read_dir(dir).ok()?.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (build, slot) = name.strip_prefix("smashcraft-journal-menu-")?.strip_suffix(".txt")?.rsplit_once("-s")?;
            let slot = slot.parse::<u32>().ok()?;
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path(), build.to_owned(), slot))
        })
        .max_by_key(|(modified, ..)| *modified)?;
    let contents = fs::read_to_string(path).ok()?;
    let (epoch, phase) = menu_fields(&contents, &build, slot)?;
    Some(Menu { build, slot, epoch, phase, modified })
}

/// Epoch and phase of a complete publication for this build and slot.
pub fn menu_fields(contents: &str, build: &str, slot: u32) -> Option<(u32, String)> {
    // The writer's closing line marks a complete file.
    if contents.lines().rev().find(|line| !line.trim().is_empty()).map(str::trim) != Some("endfunction") {
        return None;
    }
    let line = contents.split("SMASHCRAFT JOURNAL MENU v=1 ").nth(1)?.split('"').next()?;
    let field = |key: &str| line.split_whitespace().find_map(|token| token.strip_prefix(key)?.strip_prefix('='));
    if field("build")? != build || field("slot")?.parse::<u32>().ok()? != slot {
        return None;
    }
    Some((field("epoch")?.parse().ok()?, field("phase")?.to_owned()))
}

/// Follows the map's sessions from its menu publications.
#[derive(Default)]
pub struct Tracker {
    current: Option<(PathBuf, Menu)>,
    generation: u32,
}

impl Tracker {
    /// Takes a publication from `documents`; true when it starts a new session.
    pub fn observe(&mut self, documents: &Path, menu: Menu) -> bool {
        let new = match &self.current {
            None => true,
            Some((seen_in, seen)) => {
                seen_in != documents
                    || (menu.modified != seen.modified
                        && (menu.build != seen.build || menu.slot != seen.slot || menu.epoch < seen.epoch))
            }
        };
        if new {
            self.generation += 1;
        }
        self.current = Some((documents.into(), menu));
        new
    }

    pub fn session(&self) -> Option<Session> {
        let (_, menu) = self.current.as_ref()?;
        let (phase, shown) = match menu.phase.as_str() {
            "CHARACTER" => ("fighter selection", model::Phase::CharacterSelect),
            "CPU" => ("opponent settings", model::Phase::CharacterSelect),
            "STAGE" => ("stage selection", model::Phase::StageSelect),
            "RESULT" => ("results", model::Phase::Results),
            _ => ("in a match", model::Phase::Match),
        };
        Some(Session {
            key: format!("{}/s{}/{}", menu.build, menu.slot, self.generation),
            summary: format!("Smashcraft {}, player {}, {phase}", menu.build, menu.slot + 1),
            shown: Some(model::Session { map: "Smashcraft".into(), phase: shown, player: Some(menu.slot + 1) }),
        })
    }

    /// Another game: its sessions start afresh (generations keep counting).
    pub fn forget(&mut self) {
        self.current = None;
    }

    pub fn menu(&self) -> Option<&Menu> {
        self.current.as_ref().map(|(_, menu)| menu)
    }
}

/// A keyboard build's session: its build, from its ready file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeysSession {
    pub build: String,
    pub modified: SystemTime,
}

/// The ready file's build when it names a keyboard profile; None for a journal build or a partial file.
pub fn keys_build(contents: &str) -> Option<String> {
    if contents.lines().rev().find(|line| !line.trim().is_empty()).map(str::trim) != Some("endfunction") {
        return None;
    }
    let value = |label: &str| contents.split(&format!("\"{label} ")).nth(1)?.split('"').next().map(str::to_owned);
    let input = value("INPUT")?;
    matches!(input.split_whitespace().next(), Some("callback" | "keyboard-d2-r24")).then_some(())?;
    value("BUILD").filter(|build| !build.is_empty())
}

/// The ready file in a CustomMapData folder, when it names a keyboard build.
pub fn keys_session(dir: &Path) -> Option<KeysSession> {
    let path = dir.join("wc3-melee-ready.txt");
    let modified = fs::metadata(&path).ok()?.modified().ok()?;
    Some(KeysSession { build: keys_build(&fs::read_to_string(path).ok()?)?, modified })
}

/// When the process with these start ticks (/proc/PID/stat field 22) started.
pub fn started_at(birth: u64) -> Option<SystemTime> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let boot: u64 = stat.lines().find_map(|line| line.strip_prefix("btime "))?.trim().parse().ok()?;
    // USER_HZ is 100 on Linux.
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(boot) + std::time::Duration::from_millis(birth * 10))
}

#[derive(Default)]
pub struct Smashcraft {
    tracker: Tracker,
    game: Option<(u32, u64)>,
    /// The keyboard session this game runs, when it is newer than any menu publication.
    keys: Option<KeysSession>,
    /// Keyboard sessions seen, for their keys.
    keys_generation: u32,
}

impl Profile for Smashcraft {
    fn name(&self) -> &str {
        "smashcraft"
    }

    fn session(&mut self, game: &Game) -> Option<Session> {
        if self.game != Some((game.pid, game.birth)) {
            self.tracker.forget();
            self.keys = None;
            self.game = Some((game.pid, game.birth));
        }
        // Only what this game published: an older menu or ready file is an earlier game's.
        let started = started_at(game.birth);
        let data = game.documents.join("CustomMapData");
        if let Some(menu) = newest_menu(&data).filter(|menu| started.is_none_or(|at| menu.modified >= at)) {
            self.tracker.observe(&game.documents, menu);
        }
        let keys = keys_session(&data)
            .filter(|keys| started.is_none_or(|at| keys.modified >= at))
            .filter(|keys| self.tracker.menu().is_none_or(|menu| keys.build == menu.build || keys.modified > menu.modified));
        if keys.as_ref().map(|keys| keys.modified) != self.keys.as_ref().map(|keys| keys.modified) && keys.is_some() {
            self.keys_generation += 1;
        }
        self.keys = keys;
        match &self.keys {
            Some(keys) => Some(Session {
                key: format!("{}/keys/{}", keys.build, self.keys_generation),
                summary: format!("Smashcraft {} on keys", keys.build),
                shown: self.tracker.menu().filter(|menu| menu.build == keys.build)
                    .and_then(|_| self.tracker.session()).and_then(|session| session.shown),
            }),
            None => self.tracker.session(),
        }
    }

    fn keys(&self) -> bool {
        self.keys.is_some()
    }

    fn args(&self, game: &Game, pad: &Pad, _session: &Session) -> Vec<String> {
        let menu = self.tracker.menu().expect("a session has a menu");
        // The helper's epoch is the map's current one, so it drives this
        // menu and adopts the next match.
        let mut args = vec![
            "--follow-matches".into(), "--build".into(), menu.build.clone(), "--slot".into(), menu.slot.to_string(),
            "--epoch".into(), menu.epoch.to_string(), "--menu-keys".into(), "start".into(), "--device".into(), pad.device.display().to_string(),
            "--out".into(), game.documents.join("CustomMapData").display().to_string(),
        ];
        match &game.target {
            Target::Window { display, x11_window, niri_window, .. } => args.extend([
                "--editbox-display".into(), display.clone(), "--x11-window".into(), x11_window.to_string(),
                "--pid".into(), game.pid.to_string(), "--niri-window".into(), niri_window.to_string(),
            ]),
            Target::Headless { text_out } => args.extend(["--text-out".into(), text_out.display().to_string()]),
        }
        args
    }

    /// Fighter, stage and results menus, while the map keeps publishing them
    /// (it refreshes an open menu every 250 ms and publishes BLOCKED for play).
    fn pointer_menu(&self) -> bool {
        self.tracker.menu().is_some_and(|menu| {
            self.keys.as_ref().is_none_or(|keys| keys.build == menu.build)
                && matches!(menu.phase.as_str(), "CHARACTER" | "STAGE" | "RESULT")
                && SystemTime::now().duration_since(menu.modified).is_ok_and(|age| age <= std::time::Duration::from_secs(1))
        })
    }

    fn event(&self, line: &str) -> Option<HelperEvent> {
        let word = line.split_whitespace().next()?;
        Some(match word {
            "waiting_for_match" => HelperEvent::Ready,
            "match_ready" | "match_start" => HelperEvent::InMatch,
            "game-eligible=true" => HelperEvent::Focus(true),
            "game-eligible=false" => HelperEvent::Focus(false),
            "controller_disconnected" => HelperEvent::PadLost,
            "controller_reconnected" => HelperEvent::PadBack,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn menu(build: &str, slot: u32, epoch: u32, at: u64) -> Menu {
        Menu { build: build.into(), slot, epoch, phase: "CHARACTER".into(), modified: SystemTime::UNIX_EPOCH + Duration::from_millis(at) }
    }

    #[test]
    fn a_new_map_session_resets_the_epoch_and_is_followed() {
        let documents = Path::new("/prefix/Documents/Warcraft III");
        let mut tracker = Tracker::default();
        assert!(tracker.observe(documents, menu("playable-0047", 0, 0, 1)));
        let first = tracker.session().unwrap();
        // Refreshes and later matches of the same session keep it.
        assert!(!tracker.observe(documents, menu("playable-0047", 0, 0, 2)));
        assert!(!tracker.observe(documents, menu("playable-0047", 0, 1, 3)));
        assert!(!tracker.observe(documents, menu("playable-0047", 0, 2, 4)));
        assert_eq!(tracker.session(), Some(first.clone()));
        // The map opened again: its epoch starts over.
        assert!(tracker.observe(documents, menu("playable-0047", 0, 0, 5)));
        let second = tracker.session().unwrap();
        assert_ne!(second.key, first.key);
        // Rereading the same publication is not news.
        assert!(!tracker.observe(documents, menu("playable-0047", 0, 0, 5)));
        // Another build or slot is another session even at a higher epoch.
        assert!(tracker.observe(documents, menu("playable-0048", 0, 1, 6)));
        assert!(tracker.observe(documents, menu("playable-0048", 1, 1, 7)));
        // So is another game's folder.
        assert!(tracker.observe(Path::new("/other/Documents/Warcraft III"), menu("playable-0048", 1, 1, 7)));
    }

    #[test]
    fn reads_only_complete_publications_for_their_own_name() {
        let body = "function PreloadFiles takes nothing returns nothing\n\tcall Preload( \"SMASHCRAFT JOURNAL MENU v=1 build=playable-0047 epoch=2 slot=0 phase=RESULT\" )\n\tcall Preload( \"connected=1 human-fighters=1 computers=4 fighters=5\" )\n";
        assert_eq!(menu_fields(body, "playable-0047", 0), None);
        let complete = format!("{body}endfunction\n");
        assert_eq!(menu_fields(&complete, "playable-0047", 0), Some((2, "RESULT".into())));
        assert_eq!(menu_fields(&complete, "playable-0047", 1), None);
        assert_eq!(menu_fields(&complete, "playable-004", 0), None);
    }

    #[test]
    fn finds_the_newest_menu_file_with_a_dashed_build() {
        let dir = std::env::temp_dir().join(format!("smashcraft-menus-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(newest_menu(&dir), None);
        let write = |build: &str, slot: u32, epoch: u32| {
            fs::write(dir.join(format!("smashcraft-journal-menu-{build}-s{slot}.txt")),
                format!("call Preload( \"SMASHCRAFT JOURNAL MENU v=1 build={build} epoch={epoch} slot={slot} phase=CHARACTER\" )\nendfunction\n")).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        };
        write("playable-0046", 0, 3);
        write("playable-0047", 1, 0);
        let menu = newest_menu(&dir).unwrap();
        assert_eq!((menu.build.as_str(), menu.slot, menu.epoch), ("playable-0047", 1, 0));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_menu_from_before_this_game_started_is_no_session() {
        let documents = std::env::temp_dir().join(format!("smashcraft-fresh-{}", std::process::id()));
        fs::create_dir_all(documents.join("CustomMapData")).unwrap();
        fs::write(documents.join("CustomMapData/smashcraft-journal-menu-old-s0.txt"),
            "call Preload( \"SMASHCRAFT JOURNAL MENU v=1 build=old epoch=0 slot=0 phase=CHARACTER\" )\nendfunction\n").unwrap();
        let game = |birth| Game { pid: 1, birth, documents: documents.clone(), target: Target::Headless { text_out: "/dev/null".into() } };
        let boot = started_at(0).unwrap();
        let later = (SystemTime::now().duration_since(boot).unwrap().as_millis() / 10) as u64 + 360_000;
        let mut profile = Smashcraft::default();
        assert_eq!(profile.session(&game(later)), None);
        assert!(profile.session(&game(1)).is_some());
        // Another game forgets it until that game publishes one.
        assert_eq!(profile.session(&game(later + 1)), None);
        fs::remove_dir_all(documents).unwrap();
    }

    #[test]
    fn the_pointer_drives_fresh_menus_and_never_play() {
        let documents = Path::new("/prefix/Documents/Warcraft III");
        let mut profile = Smashcraft::default();
        assert!(!profile.pointer_menu());
        let now = SystemTime::now();
        for (phase, pointer) in [("CHARACTER", true), ("STAGE", true), ("RESULT", true), ("BLOCKED", false)] {
            profile.tracker.observe(documents, Menu { phase: phase.into(), modified: now, ..menu("b", 0, 1, 0) });
            assert_eq!(profile.pointer_menu(), pointer, "{phase}");
        }
        // A menu the map stopped refreshing (it closed, or the game froze) is no menu.
        profile.tracker.observe(documents, Menu { modified: now - Duration::from_secs(2), ..menu("b", 0, 1, 0) });
        assert!(!profile.pointer_menu());
    }

    const READY: &str = "function PreloadFiles takes nothing returns nothing\n\tcall Preload( \"BUILD playable-0047\" )\n\tcall Preload( \"INPUT callback PRESENTATION pool-confirmed\" )\n\tcall Preload( \"SCENARIO normal\" )\nendfunction\n";

    #[test]
    fn a_ready_file_names_a_keyboard_build() {
        assert_eq!(keys_build(READY), Some("playable-0047".into()));
        assert_eq!(keys_build(&READY.replace("INPUT callback", "INPUT keyboard-d2-r24")), Some("playable-0047".into()));
        // A journal build's ready file is no keyboard session; nor is a partial file.
        assert_eq!(keys_build(&READY.replace("INPUT callback", "INPUT shadow-d0-r24")), None);
        assert_eq!(keys_build(READY.trim_end_matches("endfunction\n")), None);
    }

    #[test]
    fn a_keyboard_build_is_a_keys_session_until_a_journal_map_publishes_a_menu() {
        let documents = std::env::temp_dir().join(format!("smashcraft-keys-{}", std::process::id()));
        let data = documents.join("CustomMapData");
        fs::create_dir_all(&data).unwrap();
        let game = Game { pid: 1, birth: 1, documents: documents.clone(), target: Target::Headless { text_out: "/dev/null".into() } };
        let mut profile = Smashcraft::default();
        assert_eq!(profile.session(&game), None);
        fs::write(data.join("wc3-melee-ready.txt"), READY).unwrap();
        let first = profile.session(&game).unwrap();
        assert!(profile.keys());
        assert!(first.key.starts_with("playable-0047/keys/"));
        assert!(!profile.pointer_menu());
        assert_eq!(profile.session(&game), Some(first.clone()));
        // The map opened again: a new ready file, a new session.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(data.join("wc3-melee-ready.txt"), READY).unwrap();
        assert_ne!(profile.session(&game).unwrap().key, first.key);
        // This keyboard build refreshes its menus after the ready marker.
        // It keeps its keys session but uses the pointer only in these menus.
        let key_session = profile.session(&game).unwrap().key;
        for (phase, pointing) in [("CHARACTER", true), ("STAGE", true), ("RESULT", true), ("BLOCKED", false)] {
            std::thread::sleep(Duration::from_millis(20));
            fs::write(data.join("smashcraft-journal-menu-playable-0047-s0.txt"),
                format!("call Preload( \"SMASHCRAFT JOURNAL MENU v=1 build=playable-0047 epoch=1 slot=0 phase={phase}\" )\nendfunction\n")).unwrap();
            assert_eq!(profile.session(&game).unwrap().key, key_session);
            assert!(profile.keys());
            assert_eq!(profile.pointer_menu(), pointing, "{phase}");
        }
        // A journal map opened after it publishes a menu: that session wins.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(data.join("smashcraft-journal-menu-typescript-integrity-s0.txt"),
            "call Preload( \"SMASHCRAFT JOURNAL MENU v=1 build=typescript-integrity epoch=0 slot=0 phase=CHARACTER\" )\nendfunction\n").unwrap();
        assert!(profile.session(&game).unwrap().key.starts_with("typescript-integrity/s0/"));
        assert!(!profile.keys());
        fs::remove_dir_all(documents).unwrap();
    }

    #[test]
    fn reads_helper_lifecycle_lines() {
        let profile = Smashcraft::default();
        assert_eq!(profile.event("waiting_for_match build=b slot=0 start_before=final-match-confirmation"), Some(HelperEvent::Ready));
        assert_eq!(profile.event("match_ready epoch=1 neutral_rearm=required"), Some(HelperEvent::InMatch));
        assert_eq!(profile.event("game-eligible=false mono_ns=1 neutral_rearm=required"), Some(HelperEvent::Focus(false)));
        assert_eq!(profile.event("controller_disconnected mono_ns=1"), Some(HelperEvent::PadLost));
        assert_eq!(profile.event("source=/dev/input/event4"), None);
    }
}
