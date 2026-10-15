use super::{any_map, profile::{Game, HelperEvent, Pad, Session, Target}};
use std::{process::Child, sync::mpsc, time::{Duration, Instant}};

/// How long a helper may stay without its pad while a pad is plugged in
/// before it is replaced: it reconnects the same pad by itself in 100 ms.
pub const PAD_SWAP: Duration = Duration::from_secs(3);
/// How often to look at the map's menu while the pad drives its pointer.
pub const MENU_POLL: Duration = Duration::from_millis(100);
/// How often to look for Warcraft III while none runs.
pub const IDLE_POLL: Duration = Duration::from_secs(1);
/// The wait before starting a helper again after one stopped.
pub const RETRY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Start,
    Stop(String),
}

struct Serving {
    game: Game,
    session: String,
    pad_lost_since: Option<Instant>,
}

/// When to start, keep or replace the helper; no I/O.
#[derive(Default)]
pub struct Supervisor {
    serving: Option<Serving>,
    retry_at: Option<Instant>,
}

impl Supervisor {
    pub fn step(&mut self, game: Option<&Game>, pad: Option<&Pad>, session: Option<&Session>, now: Instant) -> Decision {
        if let Some(serving) = &self.serving {
            let Some(game) = game else { return Decision::Stop("Warcraft III closed".into()) };
            if *game != serving.game {
                return Decision::Stop(if game.pid != serving.game.pid || game.birth != serving.game.birth {
                    "Warcraft III restarted".into()
                } else {
                    "Warcraft III's window changed".into()
                });
            }
            if let Some(session) = session {
                if session.key != serving.session {
                    return Decision::Stop(format!("new map session ({})", session.summary));
                }
            }
            if pad.is_some() && serving.pad_lost_since.is_some_and(|since| now.duration_since(since) >= PAD_SWAP) {
                return Decision::Stop("the controller came back as another device".into());
            }
            return Decision::Keep;
        }
        if self.retry_at.is_some_and(|at| now < at) {
            return Decision::Keep;
        }
        if game.is_some() && pad.is_some() && session.is_some() { Decision::Start } else { Decision::Keep }
    }

    /// A helper now serves this game and session.
    pub fn started(&mut self, game: &Game, session: &Session) {
        self.serving = Some(Serving { game: game.clone(), session: session.key.clone(), pad_lost_since: None });
    }

    /// The helper exited or was stopped.
    pub fn stopped(&mut self, now: Instant) {
        self.serving = None;
        self.retry_at = Some(now + RETRY);
    }

    pub fn event(&mut self, event: HelperEvent, now: Instant) {
        let Some(serving) = &mut self.serving else { return };
        match event {
            HelperEvent::PadLost => serving.pad_lost_since = serving.pad_lost_since.or(Some(now)),
            HelperEvent::PadBack => serving.pad_lost_since = None,
            _ => {}
        }
    }
}

pub(super) struct Running {
    pub(super) child: Child,
    pub(super) lines: mpsc::Receiver<String>,
}

pub(super) fn stop_child(running: Running) {
    let Running { mut child, .. } = running;
    // The edit-box helper holds no keys, so ending it releases nothing.
    let _ = child.kill();
    let _ = child.wait();
}

pub(super) fn output_target(game: Option<&Game>, pad: Option<&Pad>, kind: Option<any_map::Kind>) -> Option<(any_map::Window, u32, any_map::Kind)> {
    match (game, pad, kind) {
        (Some(Game { pid, target: Target::Window { display, niri_window, niri_socket, .. }, .. }), Some(_), Some(kind)) => {
            Some((any_map::Window::Niri { display: display.clone(), niri_socket: niri_socket.clone(), niri_window: *niri_window }, *pid, kind))
        }
        (Some(Game { pid, target: Target::Headless { text_out }, .. }), Some(_), Some(kind)) => {
            Some((any_map::Window::File(text_out.with_file_name("pressed.txt")), *pid, kind))
        }
        _ => None,
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use crate::rng::{Rng, cases};
    use crate::service::discovery::PadDiscovery;

    fn game(pid: u32, birth: u64, window: u32) -> Game {
        Game {
            pid,
            birth,
            documents: "/prefix/Documents/Warcraft III".into(),
            target: Target::Window { display: ":0".into(), x11_window: window, niri_window: 7, niri_socket: "/run/niri.sock".into() },
        }
    }

    fn pad(device: &str) -> Pad {
        Pad { link: "/dev/input/by-id/usb-Microsoft-event-joystick".into(), device: device.into(), name: "Xbox".into() }
    }

    #[test]
    fn the_supervisor_and_pad_discovery_follow_their_rules_over_generated_runs() {
        let mut rng = Rng::new(9);
        let start = Instant::now();
        for _ in 0..cases() {
            let mut supervisor = Supervisor::default();
            let mut discovery = PadDiscovery::default();
            let (mut serving, mut retry_at): (Option<(Game, String, Option<Duration>)>, Option<Duration>) = (None, None);
            let (mut known, mut missing_since): (Option<Pad>, Option<Duration>) = (None, None);
            let mut clock = Duration::ZERO;
            let mut kept_target = None;
            for _ in 0..30 {
                clock += rng.pick(&[Duration::ZERO, Duration::from_millis(500), Duration::from_secs(1), Duration::from_millis(1500), Duration::from_secs(2), Duration::from_secs(3)]);
                let now = start + clock;
                let found = rng.chance(70).then(|| pad(rng.pick(&["/dev/input/event4", "/dev/input/event8"])));
                let retained = discovery.update(found.clone(), now);
                if let Some(found) = found {
                    known = Some(found);
                    missing_since = None;
                } else if known.is_some() {
                    let since = *missing_since.get_or_insert(clock);
                    if clock - since >= Duration::from_secs(2) {
                        known = None;
                    }
                }
                assert_eq!(retained, known, "a missing pad is kept for two seconds");
                let game = rng.chance(85).then(|| game(rng.pick(&[1, 2]), rng.pick(&[10, 20]), rng.pick(&[5, 6])));
                let session = rng.chance(80).then(|| Session { key: rng.pick(&["a", "b"]).into(), summary: "Arena".into(), shown: None });
                let target = output_target(game.as_ref(), retained.as_ref(), Some(any_map::Kind::Keys));
                assert_eq!(target.is_some(), game.is_some() && retained.is_some());
                if let (Some(target), Some(game)) = (&target, &game) {
                    assert_eq!(target.1, game.pid);
                    if let Some((kept_game, kept)) = &kept_target && kept_game == game {
                        assert_eq!(kept, target, "a pad's new device node keeps the output");
                    }
                    kept_target = Some((game.clone(), target.clone()));
                }
                if rng.chance(40) {
                    let event = rng.pick(&[HelperEvent::PadLost, HelperEvent::PadLost, HelperEvent::PadBack, HelperEvent::PadBack, HelperEvent::Ready, HelperEvent::Focus(false)]);
                    supervisor.event(event, now);
                    if let Some((_, _, lost)) = &mut serving {
                        match event {
                            HelperEvent::PadLost => *lost = lost.or(Some(clock)),
                            HelperEvent::PadBack => *lost = None,
                            _ => {}
                        }
                    }
                }
                let decision = supervisor.step(game.as_ref(), retained.as_ref(), session.as_ref(), now);
                let expected = match &serving {
                    Some((serving_game, key, lost)) => match &game {
                        None => Some("closed"),
                        Some(g) if g.pid != serving_game.pid || g.birth != serving_game.birth => Some("restarted"),
                        Some(g) if g != serving_game => Some("window"),
                        Some(_) if session.as_ref().is_some_and(|s| &s.key != key) => Some("new map session"),
                        Some(_) if retained.is_some() && lost.is_some_and(|since| clock - since >= PAD_SWAP) => Some("another device"),
                        Some(_) => None,
                    },
                    None => None,
                };
                match (&serving, expected) {
                    (Some(_), Some(reason)) => {
                        assert!(matches!(&decision, Decision::Stop(said) if said.contains(reason)), "{decision:?} should stop: {reason}");
                        supervisor.stopped(now);
                        serving = None;
                        retry_at = Some(clock + RETRY);
                    }
                    (Some(_), None) => assert_eq!(decision, Decision::Keep),
                    (None, _) => {
                        let ready = game.is_some() && retained.is_some() && session.is_some() && retry_at.is_none_or(|at| clock >= at);
                        assert_eq!(decision, if ready { Decision::Start } else { Decision::Keep });
                        if ready {
                            let (game, session) = (game.unwrap(), session.unwrap());
                            supervisor.started(&game, &session);
                            serving = Some((game, session.key, None));
                        }
                    }
                }
            }
        }
    }
}
