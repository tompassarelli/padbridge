use super::profile::{Game, Pad, Session, Target};
use crate::model;
use std::{fs, path::Path};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HelperStatus {
    pub pid: u32,
    pub ready: bool,
    pub in_match: bool,
    pub focused: Option<bool>,
    pub pad_connected: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub profile: String,
    pub game: Option<Game>,
    pub pad: Option<Pad>,
    pub session: Option<Session>,
    pub helper: Option<HelperStatus>,
    /// The pad's keys reach a session that plays on keys.
    pub keys: bool,
    /// Why the last helper stopped, or what is missing.
    pub problem: Option<String>,
}

impl Status {
    /// One `key=value` per line, for the status file.
    pub fn lines(&self) -> String {
        let mut text = format!("service_pid={}\nprofile={}\n", std::process::id(), self.profile);
        let state = match (&self.helper, &self.game, &self.pad, &self.session) {
            (Some(helper), ..) if helper.ready => "serving",
            (None, Some(_), Some(_), Some(_)) if self.keys => "serving",
            (Some(_), ..) => "starting",
            (None, None, ..) => "no-game",
            (None, _, None, _) => "no-controller",
            (None, _, _, None) => "no-session",
            (None, ..) => "starting",
        };
        text += &format!("state={state}\n");
        if let Some(game) = &self.game {
            text += &format!("game_pid={}\ngame_documents={}\n", game.pid, game.documents.display());
            if let Target::Window { display, x11_window, niri_window, .. } = &game.target {
                text += &format!("display={display}\nx11_window={x11_window}\nniri_window={niri_window}\n");
            }
        }
        if let Some(pad) = &self.pad {
            text += &format!("pad={}\npad_device={}\n", pad.name, pad.device.display());
        }
        if let Some(session) = &self.session {
            text += &format!("session={}\nsession_summary={}\n", session.key, session.summary);
        }
        if let Some(helper) = &self.helper {
            text += &format!("helper_pid={}\nin_match={}\npad_connected={}\n", helper.pid, helper.in_match, helper.pad_connected);
            if let Some(focused) = helper.focused {
                text += &format!("focused={focused}\n");
            }
        }
        if let Some(problem) = &self.problem {
            text += &format!("problem={}\n", problem.replace('\n', " "));
        }
        text
    }
}

/// What windows see: the service's state in the shared model.
/// `any_map` is the Any map output's focus while it runs.
pub fn snapshot(status: &Status, choice: model::ProfileChoice, profile: model::Profile, any_map: Option<bool>) -> model::Snapshot {
    model::Snapshot {
        settings: model::ControllerSettings::default(),
        pad: status.pad.as_ref().map(|pad| model::Pad {
            name: pad.name.clone(),
            id: pad.link.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        }),
        game: status.game.as_ref().map(|game| model::Game { pid: game.pid, window: matches!(game.target, Target::Window { .. }) }),
        session: status.session.as_ref().and_then(|session| session.shown.clone()),
        profile,
        choice,
        map: None,
        output: if let Some(focused) = any_map { model::Output { running: true, ready: true, focused } } else { model::Output {
            running: status.helper.is_some(),
            ready: status.helper.as_ref().is_some_and(|helper| helper.ready),
            // The helper reports focus changes; it starts focused until it says otherwise.
            focused: status.helper.as_ref().is_some_and(|helper| helper.focused.unwrap_or(true)),
        } },
        problem: (status.helper.is_none() && status.problem.is_some())
            .then(|| "Controller support hit a problem and is starting again.".to_owned()),
    }
}

pub(super) fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let temporary = path.with_extension("tmp");
    if fs::write(&temporary, text).is_ok() {
        let _ = fs::rename(&temporary, path);
    }
}
