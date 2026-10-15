use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// `padbridge --service`: the always-on controller service (README, "Always-on controller service").
#[cfg(target_os = "linux")]
pub fn service() -> Result<(), String> {
    use padbridge::service::{self, Config, NoMap, Profile, plugin::Plugin};
    let mut config = Config::default();
    let mut plugin = None;
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("missing value for {arg}"));
        match arg.as_str() {
            "--plugin" => plugin = Some(std::path::PathBuf::from(value()?)),
            "--layout" => {
                let path = value()?;
                let text = std::fs::read_to_string(&path).map_err(|error| format!("read the layout {path}: {error}"))?;
                let layout: Vec<padbridge::model::Binding> = serde_json::from_str(&text).map_err(|error| format!("the layout {path}: {error}"))?;
                if let Some(binding) = layout.iter().find(|binding| matches!(&binding.press, padbridge::model::Press::Key(key) if service::any_map::key_of(key).is_none())) {
                    return Err(format!("the layout {path}: {:?} names no key this service can press", binding.press));
                }
                config.layout = Some(layout);
            }
            "--display" => config.display = value()?,
            "--pads" => config.pads = value()?.into(),
            "--status" => config.status_file = Some(value()?.into()),
            "--settings" => config.settings_file = value()?.into(),
            "--headless" => config.headless = Some(value()?.into()),
            "--interface" => config.interface = Some(value()?).filter(|address| address != "off"),
            "--poll-ms" => config.poll = Duration::from_millis(value()?.parse().map_err(|_| "invalid --poll-ms")?),
            _ => return Err(format!("unexpected argument {arg:?}\n{SERVICE_HELP}")),
        }
    }
    let mut profile: Box<dyn Profile> = match plugin {
        // A missing plug-in leaves every map on Any map rather than no controller at all.
        Some(program) if program.exists() => Box::new(Plugin::spawn(&program)?),
        Some(program) => {
            eprintln!("service: no map plug-in at {}; Any map only", program.display());
            Box::new(NoMap)
        }
        None => Box::new(NoMap),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed)).map_err(|error| format!("install interrupt handler: {error}"))?;
    service::run(&config, profile.as_mut(), &stop, |_| {})
}

#[cfg(target_os = "linux")]
const SERVICE_HELP: &str = "padbridge --service [--plugin PROGRAM] [--layout FILE] [--display :0] [--pads DIR] [--status FILE] [--settings FILE] [--headless DOCUMENTS] [--interface ADDRESS|off] [--poll-ms N]";
