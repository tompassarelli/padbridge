//! The Smashcraft profile: the map publishes
//! `CustomMapData/smashcraft-journal-menu-BUILD-sSLOT.txt` for the local
//! player, naming its build, slot, epoch and menu phase; `wc3-journal
//! --follow-matches` serves it (smashcraft:companion/README.md).
//!
//! Within one map session the epoch only grows, one per match. A newer
//! publication with a lower epoch, or another build or slot, is a new map
//! session (the map was opened again), which a running helper would ignore,
//! so it gets a new session key and a fresh helper.

use super::{Game, HelperEvent, Pad, Profile, Session, Target};
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
        let phase = match menu.phase.as_str() {
            "CHARACTER" => "fighter selection",
            "STAGE" => "stage selection",
            "RESULT" => "results",
            _ => "in a match",
        };
        Some(Session {
            key: format!("{}/s{}/{}", menu.build, menu.slot, self.generation),
            summary: format!("Smashcraft {}, player {}, {phase}", menu.build, menu.slot + 1),
        })
    }

    pub fn menu(&self) -> Option<&Menu> {
        self.current.as_ref().map(|(_, menu)| menu)
    }
}

#[derive(Default)]
pub struct Smashcraft {
    tracker: Tracker,
}

impl Profile for Smashcraft {
    fn name(&self) -> &str {
        "smashcraft"
    }

    fn session(&mut self, game: &Game) -> Option<Session> {
        if let Some(menu) = newest_menu(&game.documents.join("CustomMapData")) {
            self.tracker.observe(&game.documents, menu);
        }
        self.tracker.session()
    }

    fn args(&self, game: &Game, pad: &Pad, _session: &Session) -> Vec<String> {
        let menu = self.tracker.menu().expect("a session has a menu");
        // The helper's epoch is the map's current one, so it drives this
        // menu and adopts the next match.
        let mut args = vec![
            "--follow-matches".into(), "--build".into(), menu.build.clone(), "--slot".into(), menu.slot.to_string(),
            "--epoch".into(), menu.epoch.to_string(), "--device".into(), pad.device.display().to_string(),
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
    fn reads_helper_lifecycle_lines() {
        let profile = Smashcraft::default();
        assert_eq!(profile.event("waiting_for_match build=b slot=0 start_before=final-match-confirmation"), Some(HelperEvent::Ready));
        assert_eq!(profile.event("match_ready epoch=1 neutral_rearm=required"), Some(HelperEvent::InMatch));
        assert_eq!(profile.event("game-eligible=false mono_ns=1 neutral_rearm=required"), Some(HelperEvent::Focus(false)));
        assert_eq!(profile.event("controller_disconnected mono_ns=1"), Some(HelperEvent::PadLost));
        assert_eq!(profile.event("source=/dev/input/event4"), None);
    }
}
