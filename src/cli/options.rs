use padbridge::focus;

#[derive(Default)]
pub struct Options {
    pub preset: padbridge::model::PadPreset,
    pub tap_jump: bool,
    pub triggers: padbridge::model::TriggerShields,
    pub right_stick: padbridge::model::RightStick,
    pub remaps: padbridge::model::Remaps,
    pub seconds: Option<u64>,
    pub gamepad: Option<u32>,
    pub emit: bool,
    pub display: Option<String>,
    pub window: Option<u32>,
    pub pid: Option<u32>,
    pub niri_window: Option<u64>,
    pub wlr_app_id: Option<String>,
    pub check_focus: bool,
    pub virtual_pad: bool,
    pub pad_ingress: Option<String>,
    pub cursor_grid: Option<String>,
    pub cursor_calibrate: Option<bool>,
}

#[cfg(target_os = "linux")]
pub const HELP: &str = "padbridge [--list] [--watch-seconds N] [--gamepad ID] [--preset melee|z-jump|tom] [--tap-jump on|off] [--left-trigger full|light] [--right-trigger full|light] [--right-stick smash|tilt] [--remaps CONTROL=MOVE,...]\n\
                    Observation only by default. Live output additionally requires:\n\
                    --emit --display DISPLAY --x11-window DECIMAL_ID --pid PID --niri-window ID\n\
                    Or --private-wlr-app-id ID in the isolated labwc test desktop instead of --niri-window.\n\
                    --pad-ingress digital|keys|cursor selects experimental analog output (requires --emit).\n\
                    --cursor-grid X,Y,W,H,SCREEN_W,SCREEN_H selects cursor cells in logical pixels.\n\
                    --cursor-calibrate start|end holds the corresponding corner marker until exit.\n\
                    --virtual-pad uses stdin SDL axis/button commands instead of a physical pad.\n\
                    --check-focus checks selected target without opening keyboard output. Windows/macOS refuse live output.";

#[cfg(not(target_os = "linux"))]
pub const HELP: &str = "padbridge [--list] [--watch-seconds N] [--gamepad ID] [--preset melee|z-jump|tom] [--tap-jump on|off] [--left-trigger full|light] [--right-trigger full|light] [--right-stick smash|tilt] [--remaps CONTROL=MOVE,...]\n\
                    Observation only by default. --emit sends keys only while Warcraft III is the\n\
                    foreground application; add --pid PID to require one game process.\n\
                    --check-focus checks the foreground game without opening keyboard output.\n\
                    --virtual-pad (testing) replaces hardware with an SDL virtual gamepad driven by stdin.";

#[cfg(target_os = "linux")]
pub fn target(o: &Options) -> Result<focus::Target, String> {
    Ok(focus::Target {
        display: o.display.clone().ok_or("--emit requires --display")?,
        window: o.window.ok_or("--emit requires --x11-window")?,
        pid: o.pid.ok_or("--emit requires --pid")?,
        niri_window: o.niri_window,
        wlr_app_id: o.wlr_app_id.clone(),
    })
}

#[cfg(not(target_os = "linux"))]
pub fn target(o: &Options) -> Result<focus::Target, String> {
    if o.display.is_some()
        || o.window.is_some()
        || o.niri_window.is_some()
        || o.wlr_app_id.is_some()
    {
        return Err(
            "--display, --x11-window, --niri-window and --private-wlr-app-id select Linux desktops"
                .into(),
        );
    }
    Ok(focus::Target { pid: o.pid })
}


pub fn options() -> Result<Options, String> {
    let mut o = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            "--list" => {}
            "--preset" => o.preset = padbridge::model::PadPreset::parse(&args.next().ok_or("missing pad preset")?)?,
            "--tap-jump" => o.tap_jump = match args.next().as_deref() { Some("on") => true, Some("off") => false, _ => return Err("--tap-jump needs on or off".into()) },
            "--left-trigger" => o.triggers.left = padbridge::model::TriggerShield::parse(&args.next().ok_or("--left-trigger needs full or light")?)?,
            "--right-stick" => o.right_stick = padbridge::model::RightStick::parse(&args.next().ok_or("--right-stick needs smash or tilt")?)?,
            "--remaps" => o.remaps = padbridge::model::parse_remaps(&args.next().ok_or("--remaps needs CONTROL=MOVE,...")?)?,
            "--right-trigger" => o.triggers.right = padbridge::model::TriggerShield::parse(&args.next().ok_or("--right-trigger needs full or light")?)?,
            "--emit" => o.emit = true,
            "--check-focus" => o.check_focus = true,
            "--virtual-pad" => o.virtual_pad = true,
            "--pad-ingress" => o.pad_ingress = Some(args.next().ok_or("missing pad ingress")?),
            "--cursor-grid" => o.cursor_grid = Some(args.next().ok_or("missing cursor rectangle")?),
            "--cursor-calibrate" => o.cursor_calibrate = Some(match args.next().as_deref() {
                Some("start") => false, Some("end") => true,
                _ => return Err("cursor calibration must be start or end".into()),
            }),
            "--private-wlr-app-id" => {
                o.wlr_app_id = Some(args.next().ok_or("missing private compositor app ID")?)
            }
            "--watch-seconds" => {
                o.seconds = Some(
                    args.next()
                        .ok_or("missing seconds")?
                        .parse()
                        .map_err(|_| "invalid seconds")?,
                )
            }
            "--gamepad" => {
                o.gamepad = Some(
                    args.next()
                        .ok_or("missing gamepad ID")?
                        .parse()
                        .map_err(|_| "invalid gamepad ID")?,
                )
            }
            "--display" => o.display = Some(args.next().ok_or("missing display")?),
            "--x11-window" => {
                o.window = Some(
                    args.next()
                        .ok_or("missing X11 window")?
                        .parse()
                        .map_err(|_| "invalid decimal X11 window")?,
                )
            }
            "--pid" => {
                o.pid = Some(
                    args.next()
                        .ok_or("missing PID")?
                        .parse()
                        .map_err(|_| "invalid PID")?,
                )
            }
            "--niri-window" => {
                o.niri_window = Some(
                    args.next()
                        .ok_or("missing Niri window")?
                        .parse()
                        .map_err(|_| "invalid Niri window")?,
                )
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if o.emit && o.seconds.is_none() {
        return Err("--emit requires a bounded --watch-seconds duration".into());
    }
    if (o.pad_ingress.is_some() || o.cursor_calibrate.is_some()) && !o.emit {
        return Err("analog output and calibration require --emit".into());
    }
    #[cfg(not(target_os = "linux"))]
    if o.pad_ingress.is_some() || o.cursor_calibrate.is_some() {
        return Err("candidate analog output currently runs on Linux".into());
    }
    Ok(o)
}
