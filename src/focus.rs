/// Whether the selected game currently owns keyboard focus. `Ok(false)` is
/// ordinary focus loss; `Err` means the selected target can no longer be
/// identified. Each OS compiles exactly one adapter, exported as `Gate`.
pub trait Foreground {
    fn eligible(&mut self) -> Result<bool, String>;

    /// Why the last check found the game ineligible, when the adapter can tell.
    fn away(&self) -> Option<&str> {
        None
    }
}

#[cfg(any(windows, target_os = "macos", test))]
fn executable_matches(path: &str, name: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .is_some_and(|file| file.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod properties {
    use crate::rng::{Rng, cases};

    #[test]
    fn executable_match_is_the_last_path_component_ignoring_case() {
        let mut rng = Rng::new(11);
        for _ in 0..cases() {
            let name = rng.pick(&["Warcraft III.exe", "Warcraft III"]);
            let file = rng.pick(&[name.to_owned(), name.to_uppercase(), name.to_lowercase(), format!("{name} Launcher"), format!("x{name}"), "launcher.exe".into(), String::new()]);
            let directories: Vec<String> = (0..rng.below(4)).map(|_| rng.pick(&["C:", "Games", "Warcraft III.exe", "Warcraft III", "Applications", "_retail_"]).to_owned()).collect();
            let mut path = String::new();
            for directory in &directories {
                path += directory;
                path.push(rng.pick(&['/', '\\']));
            }
            path += &file;
            assert_eq!(super::executable_matches(&path, name), file.eq_ignore_ascii_case(name), "{path:?} {name:?}");
        }
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Gate, Target};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Gate, Target};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Gate, Target};
