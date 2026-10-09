use crate::model::ControllerSettings;
use std::{fs, path::{Path, PathBuf}};

pub fn default_path() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
    });
    config.join("wc3-controller/settings.json")
}

pub fn load(path: &Path) -> Result<ControllerSettings, String> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| format!("read controller settings {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ControllerSettings::default()),
        Err(error) => Err(format!("read controller settings {}: {error}", path.display())),
    }
}

pub fn save(path: &Path, settings: &ControllerSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).map_err(|error| error.to_string())?; }
    let temporary = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(settings).map_err(|error| error.to_string())?;
    fs::write(&temporary, text).map_err(|error| error.to_string())?;
    fs::rename(temporary, path).map_err(|error| error.to_string())
}
