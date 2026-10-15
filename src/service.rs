//! The always-on controller service: finds Warcraft III on a display, the
//! controller and the map's session by itself, and keeps one helper process
//! serving them, replacing it when any of them changes.
//!
//! Game, window and pad discovery are map-agnostic. What a map asks of the
//! helper, and the helper's command line, belong to a [`Profile`]: a map
//! plug-in, in this process or another one ([`plugin`]). Without one the
//! service plays every map with the Any map profile ([`NoMap`]).

pub mod any_map;
mod discovery;
pub mod interface;
pub mod plugin;
pub mod pointer;
mod profile;
mod run;
pub mod settings;
mod status;
mod supervisor;

pub use discovery::{documents, find_game, find_pad, niri_game_window, niri_socket, warcraft_processes};
pub use profile::{Game, Helper, HelperEvent, Line, NoMap, Pad, Profile, Session, Target};
pub use run::{Config, lock_path, run};
pub use status::{HelperStatus, Status, snapshot};
pub use supervisor::{Decision, IDLE_POLL, MENU_POLL, PAD_SWAP, RETRY, Supervisor};
