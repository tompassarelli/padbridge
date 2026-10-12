# wc3-controller

Game controller support for any Warcraft III map. An always-on Linux service
finds Warcraft III, the controller and the map being played by itself, and turns
the pad into keys, clicks and pointer motion while the game has focus. Windows
and macOS get the same controller-as-keys mapper as a foreground helper
(`wc3-controller --emit`). SDL3 owns discovery, normalization and adapter
protocols; enigo delivers key transitions. MIT licence (LICENSE).

Two ways for a map to support a controller:

- **A key layout file** (no code): the Any map profile presses the keys your
  map already uses. Most maps need nothing else.
- **A map plug-in** (a program): for a map with its own input protocol, menus
  driven by a pointer, or a helper process that types into the game.

Release binaries for Linux and Windows are attached to every
[release](https://github.com/tompassarelli/wc3-controller/releases); CI
(`.github/workflows/ci.yml`) builds them from each `v*` tag.

## Add controller support to your map

1. Write a layout file: a JSON list of bindings, one per control you use.
   `examples/echo-isles/layout.json` is a complete one for an ordinary melee
   game on Blizzard's Echo Isles (camera on the left stick, pointer on the
   right, hero abilities on the face buttons, control groups on the triggers):

   ```json
   [
     { "control": "a", "action": "Select", "press": "left_click" },
     { "control": "x", "action": "Hero ability 1", "press": { "key": "q" } },
     { "control": "lt", "action": "Army (group 1)", "press": { "key": "1" } },
     { "control": "left_up", "action": "Camera up", "press": { "key": "up" } },
     { "control": "right_up", "action": "Pointer", "press": "pointer" }
   ]
   ```

   - `control`: `a`, `b`, `x`, `y`, `lb`, `rb`, `lt`, `rt`, `start`, `back`,
     `left_stick` (click), `dpad_up|down|left|right`, and each stick direction
     `left_up|down|left|right`, `right_up|down|left|right`. Bind each at most once.
   - `press`: `{"key": NAME}` (a letter or digit, `space`, `escape`, `tab`,
     `enter`, `up`, `down`, `left`, `right`, `f1`..`f24`, `insert`, `delete`,
     `home`, `end`, `pageup`, `pagedown`), `"left_click"`, `"right_click"`, or
     `"pointer"` (the stick moves the mouse pointer).
   - `action`: your map's word for it; windows show it to players.

   A stick direction counts outside 0.28 of full scale, a trigger beyond 4000
   of 32767. Without a layout file the built-in one is used
   (`model::any_map_bindings`, described under "Any map profile").

2. Run the service with it:

   ```sh
   wc3-controller --service --layout examples/echo-isles/layout.json
   ```

   Open your map in Warcraft III on display `:0`: while its window has focus the
   pad presses your keys; focus loss releases everything, and nothing presses
   again until the pad returns to neutral. A layout naming a key the service
   can't press is refused at startup.

3. Check it without the game. `tests/layout.rs` runs the real service on this
   layout with a virtual pad (`/dev/uinput`) and a stand-in game
   (`--headless DOCUMENTS`), whose presses are written to `pressed.txt`; it
   checks that X, RT, A, LB and the left stick press `q`, `2`, left click, `r`
   and the left arrow, in order and nothing else:

   ```sh
   cargo test --locked --test layout
   ```

A window (any program) can also replace the layout live over the local
interface with `{"bindings":[...]}` in the same format.

## Map plug-ins

A map plug-in tells the service when the map runs a session, what it shows
players, and which helper process serves it. Implement
`wc3_controller::service::Profile` in Rust and either run
`wc3_controller::service::run` with it in your own program, or serve it as a
separate program with `wc3_controller::service::plugin::serve` and start the
service with `--plugin PROGRAM`. A plug-in that is missing leaves the service on
Any map.

The service runs `PROGRAM --plugin` once and talks JSON to it, one object per
line. The program first writes its hello, then answers every request on its
standard input with exactly one line on its standard output:

| Service sends | Plug-in answers |
| --- | --- |
| (start) | `{"name":"hero-arena","title":"Hero Arena"}` |
| `{"session":GAME}` (every poll, 100 ms to 1 s) | `{"session":SESSION or null,"keys":false,"pointer_menu":false}` |
| `{"helper":{"game":GAME,"pad":PAD,"session":SESSION}}` | `{"program":"/path/helper","args":[...]}` or `null` |
| `{"line":"text the helper wrote"}` | `{"event":EVENT or null,"problem":"sentence" or null}` |

- `GAME` is `{"pid":N,"birth":TICKS,"documents":"…/Documents/Warcraft III","target":T}`,
  where `T` is `{"window":{"display":":0","x11_window":N,"niri_window":N,"niri_socket":PATH}}`
  or, for a stand-in game, `{"headless":{"text_out":PATH}}`. `birth` is the
  process start time (`/proc/PID/stat` field 22): a reused PID is another game.
  Read what your map publishes (for example Preload files in
  `documents/CustomMapData`), and ignore files older than the game.
- `PAD` is `{"link":"/dev/input/by-id/…","device":"/dev/input/eventN","name":"…"}`.
- `SESSION` is `{"key":"…","summary":"…","shown":{"map":"Hero Arena","phase":P,"player":1} or null}`.
  The helper is replaced whenever `key` changes; `summary` is for logs; `shown`
  is what windows display, `P` one of `lobby`, `character_select`,
  `stage_select`, `match`, `results`.
- `keys: true` plays the session on keys: no helper; the service presses the
  fighter layout itself. `pointer_menu: true` makes the left stick the desktop
  pointer, A left click and B right click (`model::menu_pointer_bindings`)
  until the map says otherwise.
- The helper gets the fighter layout's settings appended (`--preset`,
  `--tap-jump`, `--left-trigger`, `--right-trigger`, `--right-stick`, and `--remaps` when there
  are remaps), `WC3_SERVICE_PID`,
  and, for a window, `DISPLAY` and `NIRI_SOCKET`. The service forwards each
  line of its standard error as a `line` request. `EVENT` is `"ready"`,
  `"in_match"`, `{"focus":true|false}`, `"pad_lost"` or `"pad_back"`.

The service starts a helper when it has a game, a pad and a session; replaces
it when Warcraft III restarts or its window changes, when the session key
changes, and when the helper has been without its pad for 3 s while a pad is
plugged in; and starts one again 2 s after a helper exits.

## Build and observe

From the checkout, use Rust 1.96.1 (pinned in
`rust-toolchain.toml`), a C/C++ compiler and CMake. Linux also
needs the libudev and libxkbcommon development libraries. Dependencies, including
SDL 3.4.16 built statically through sdl3 0.20.0, are locked in
`Cargo.lock`. Build output stays in
`target`.

```sh
cargo test --locked --jobs 2
cargo build --locked --jobs 2
cargo run --locked -- --list
cargo run --locked -- --watch-seconds 20
# When several controllers are present, select an ID printed by --list:
cargo run --locked -- --watch-seconds 20 --gamepad 1
```

On NixOS, `nix-shell -p stdenv.cc cmake pkg-config libxkbcommon udev` provides
the build environment.

SDL runs on the main thread with background gamepad acquisition enabled. Logs
include SDL event timestamps, monotonic observation times, device identity and
mapping, normalized samples, and action transitions. Normalized axes are not
original USB packet bytes. Neither these timestamps nor the 4 ms polling sleep
measure physical input latency. Device enumeration may still fail for missing
permissions or an unsupported adapter mode; no driver or permission changes are
performed. Unplugging clears actions. Reconnecting creates a new SDL instance ID;
select it in a fresh run rather than silently switching to another controller.

In `--watch-seconds` runs, stdout is a streaming TSV event history with columns
`record, event_id, capture_ns, dequeue_ns, submit_ns, control, value, action,
pressed, disposition`. A source event row is emitted for every button or axis
event from the selected SDL gamepad; transition rows repeat its event ID and
retain SDL's original `capture_ns`. Event IDs increase in selected event dequeue
order. `capture_ns` is SDL's timestamp in nanoseconds since SDL initialization;
`dequeue_ns` is the helper's monotonic time since watch start when it reads the
event; `submit_ns` is sampled immediately before keyboard delivery (empty for
source-only rows). SDL time and helper monotonic time have separate origins and
must not be subtracted across those clocks. Rows stream directly to stdout, so
the helper does not retain an unbounded in-memory history. Startup queue items
are counted and discarded before the neutral baseline; after focus loss, events
in the recovery batch are recorded as suppressed while a fresh state is used
to rearm. This fixes event collapse in the helper. It does not prove keyboard
delivery preserves a tap or that Warcraft's map-level polling assigns it to the
intended simulation frame.

## Fighter layout

A platform fighter's moves on fixed keys: the layout `wc3-controller --emit`
presses, and the one the service presses for a plug-in session on keys
(`model::fighter_bindings`).

Three presets choose the buttons; sticks and Start are the same in all of them.
`wc3-controller` and plug-in helpers accept `--preset melee|z-jump|tom`, and
`--list` prints them.

| Control | melee (default) | z-jump | tom |
| --- | --- | --- | --- |
| A | Attack N | Attack N | Attack N |
| B | Special U | Special U | Grab O |
| X | Jump I | Grab O | Special U |
| Y | Jump I | Jump I | Jump I |
| RB | Grab O | Jump I | Meter X (+ Special: EX) |
| LB | — | — | Short hop Z (even held) |
| LT | Shield Q | Shield Q | Shield Q |
| RT | Shield G (right trigger) | Shield G (right trigger) | Shield G (right trigger) |
| Start | Pause Y | Pause Y | Pause Y |
| Left stick left / right / down | W / R / E | W / R / E | W / R / E |
| Left stick up | Space (up only; no tap jump) | Space | Space |
| Right stick up / right / down / left | J / M / H / B | J / M / H / B | J / M / H / B |

**melee**, the default new players get, is the GameCube layout by function:
A attacks, B specials, X and Y jump (tap for a short hop, hold for a full
hop), Z (RB) grabs, either trigger shields (RT on its own key, so pressing it while holding LT is a fresh press) and LB is free. **z-jump** is the common Melee swap: Z (RB) jumps, which frees a face
button, X, for grab. **tom** keeps X special and B grab, makes LB a dedicated
short hop and Y an ordinary jump, and makes RB the Meter button: RB +
Special is EX (Shield + Special still is). No preset has a tilt/walk
modifier; it remains a remap (`tilt`). In every preset a
shield press during jump squat comes out as an air dodge on the first
airborne frame (a wavedash).

The right stick smashes by default. Right stick Tilt attacks (a window's
`{"right_stick":"tilt"}` or `--right-stick smash|tilt`, saved as `"right_stick"`)
also holds Tilt (P) while it is pushed, so it tilts on the ground and throws
aerials in the air, in any preset.

Remaps change single buttons on top of a preset, e.g. grab on LB instead of RB.
A window sends `{"remaps":{"lb":"grab","rb":"none"}}`, which replaces the
previous remaps; the service saves them with the other settings in
`~/.config/wc3-controller/settings.json` as `"remaps"`. Controls are `a`, `b`,
`x`, `y`, `lb`, `rb`, `lt`, `rt` and `left_stick`, named by position as in
layout files (on a GameCube pad after its letter swap: `x` is the printed B);
moves are `attack`, `special`, `jump`, `grab`, `shield`, `tilt`, `short_hop`,
`meter` and `none` (unbound). A trigger remapped to shield keeps its full or light
choice; shield on a button is full shield. `wc3-controller --remaps
lb=grab,rb=none` takes the same remaps, and `model::ControllerSettings::bindings`
gives the remapped layout a window shows.

A fourth, hidden preset, `--preset script`, is the encoding of recorded pad
scripts and their test drivers: A attack, X special, B and Y jump, RB grab, LB
tilt, both triggers shield and L3 short hop. It is not listed by `--list` or
offered to players, so a player preset can change without touching recorded
scripts.

A GameCube pad (SDL reports `SDL_GAMEPAD_TYPE_GAMECUBE`, e.g. through a Wii U
or Mayflash adapter) maps by its printed letters: in melee A attacks, B
specials, X and Y jump, Z grabs and L and R shield. `--list` prints each pad's
`kind`. On Windows the Wii U adapter needs the WinUSB driver installed with
[Zadig](https://zadig.akeo.ie/), as
[Dolphin documents](https://dolphin-emu.org/docs/guides/how-use-official-gc-controller-adapter-wii-u/).
GameCube identity applies to the SDL helper (`wc3-controller --watch-seconds`);
the service reads Linux evdev pads and does not apply it.

Either trigger can instead light shield: choose Full shield or Light shield for
each trigger in a window, or use `--left-trigger full|light` and
`--right-trigger full|light`. Light shield presses T and requests
pressure 77; full shield presses Q on either trigger and requests 255. Both
triggers share Q because it is the shield key every saved map binding has;
The model's preset bindings (`model::fighter_bindings_with`, `model::pad_bindings`,
`ControllerSettings::bindings`) are the only binding list: the service presses
them and windows show them. contract/bindings.json maps each action label to the
Smashcraft action its key must reach, and the tests check that against the map's
standard keys and a copy of a saved binding set (contract/tom-saved-bindings.pld).

Tap jump is off by default. A window can enable it, or
`--tap-jump on|off`. When enabled,
stick up past 0.6625 requests jump. Holding Tilt plus shield caps the effective
stick at 0.65 before tap jump, so the shield can tilt up without jumping.
Jump buttons keep working while tilting the shield.

Both sticks use Melee's conversion on every pad (src/stick.rs, also
public for plug-in helpers). The stick is first clamped radially
to full scale, as Melee does at its 80-unit full scale. Each axis whose magnitude is then at most
**0.28** of full scale reads 0; a value outside it is kept, not rescaled.
Full scale is SDL's and the normalized evdev range, ±32767, standing in for
Melee's 80 units, so a left-stick axis counts from 9175. No resting-offset
calibration is applied; the deadzone absorbs a pad's resting offset. Left,
right and up are active when their axis is outside the deadzone; down needs
the stronger threshold below. Right-stick directions
use the flick thresholds below and triggers need 4000, strictly beyond. Shared sources are unioned before emission:
two jump buttons share one held jump action. Stick-up is only up: aim, up-special, getup and ledge stand. Hold
duration remains available to the game's jump logic.

Hold Tilt (P; a `tilt` remap on a pad) with left or right and press Special (U) to use
neutral special facing that direction. Without Tilt, left or right selects
side special. Up and down still select their specials while Tilt is held.

Down is active only at **0.6625** of full scale (53 of Melee's 80 units; a
left-stick axis counts from 21709), after the radial clamp. Melee has no single
down threshold; our design targets are: fast-fall, platform drop,
ledge-catch refusal and down smash at 0.6625 (0.66 for the platform drop and
ledge catch, the same 53 units); crouch (0.6875, strict, 56 units) and spot dodge
(0.7) need slightly more.
The single digital down (E) takes the 53-unit value shared by four of those
six actions, so a slight tilt no longer fast-falls, drops through a platform or
passes a ledge. Crouch and spot dodge trigger about three stick units early.
Up keeps the deadzone: ledge climb 0.25 and getup 0.2 lie
inside it, and the 0.6625 up actions (tap jump, up smash) are not served by
digital up. Up-special (0.55) and the direction sent with a special or
dodge press still use the deadzone.

The right stick presses a direction at Melee's smash-flick thresholds, after the
same clamp and deadzone: **0.8** sideways and **0.6625** up or down (axis
values from about 26215 and 21709). Our design targets: smash side 0.8; smash up and down,
get-up up, ledge-attack up and the C-stick up jump 0.6625; ledge-attack sideways
0.8. Lower thresholds (aerials, throws, side get-up and ledge climb, 0.25 or
0.2) fall inside the 0.28 deadzone, and C-stick roll and spot dodge use 0.7. One digital press per direction takes the smash
values, so an aerial, throw or side get-up by C-stick needs a harder flick than
in Melee. A full-scale diagonal clamps to 0.707 per axis, so it presses up or
down but not sideways, as in Melee.

The service reads Linux face buttons from evdev. Sony's driver reports
positions, but xpad and other Xbox-style drivers report labels: X as `BTN_X`
(0x133, the code also named `BTN_NORTH`) and Y as `BTN_Y` (0x134, `BTN_WEST`).
It decides by vendor exactly as SDL's Linux mapping does, so X is X on both
paths (src/service/interface.rs, `apply_event`).

Startup, loss of game eligibility and disconnect require all mapped controls to
return to neutral while eligible before rearming. Only keys this output backend
pressed are released; there is no blanket keyboard reset. Physical keyboard and
controller use of the same key still requires native testing.

## Explicit Linux output

The **Niri plus X11/XWayland** eligibility adapter uses a selected
live Warcraft PID, its exact X11 focus window/display, and its Niri window ID.
The PID's command line must identify Warcraft III.exe; its process start time
must remain unchanged. `_NET_WM_PID` and Niri's focused window ID must agree with
the selected target. Niri overview must be closed. Missing/failed replies are
ineligible, and no old successful observation is reused.

For xwayland-satellite, Niri reports the bridge's PID. The adapter verifies its
executable and process birth, then uses XRes to require that this exact PID owns
the selected display's window manager. The selected Niri window must uniquely
match the game's current X11 class and title among that bridge's windows. The
exact game XID must also be the current `_NET_ACTIVE_WINDOW`. Ordinary X11 focus
must address the game; if it is `PointerRoot`, the root pointer's child must be
the game instead. Keep the pointer over the game for this Wine focus mode.
Compositor focus is checked again after the X11 observations. A stale active
window, matching title alone, duplicate native identity, or unrecognized proxy
never grants eligibility. Direct Niri game PIDs and private labwc retain their
ordinary exact-X11-focus requirement.

After selecting the actual IDs in a controlled Niri game session, with one
mapper responsible for delivery, replace the example values:

```sh
cargo run --locked -- --watch-seconds 30 --gamepad 1 --emit \
  --display :0 --x11-window 123456 --pid 12345 --niri-window 678
```

`NIRI_SOCKET` must refer to that same desktop. The tool never focuses Warcraft.
Keyboard output uses logical lowercase keys through enigo, with explicit press
and release; verify the game's keyboard layout/preset. Ctrl-C, the bounded
watch duration, focus loss and disconnect release owned keys. Output errors stop
the run and attempt owned-key cleanup; process crashes or forced termination
cannot guarantee cleanup. Global input delivery has an unavoidable focus-change
race between a foreground check and OS submission. Niri IPC has a 50 ms timeout;
X11/enigo operations rely on their library's connection behavior.

The companion cannot identify map gameplay versus chat or a modal within the
same game window. Keep live trials in the intended map gameplay and stop mapping
before chat/menus that consume these keys. Native focus-away/overview/lock,
physical keyboard overlap, layout and game-consumption checks remain required.

For the owner's isolated labwc test desktops, the alternative
`--private-wlr-app-id steam_app_3516115571` replaces `--niri-window`. A direct
Wayland foreign-toplevel subscription requires exactly one window with that
app ID and title `Warcraft III`, marked activated. It runs a compositor sync
barrier each observation, rather than invoking an external command. The exact
X11 focus window/PID/display and process birth checks still apply. The protocol
has no PID, so this adapter is scoped to the explicitly selected private test
namespace with its unique app ID, not advertised as a general compositor
identity guarantee. `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` must select that
desktop. X11 and Wayland connection operations rely on their libraries' blocking
behavior; a hung display server is not a bounded-release guarantee.

Use `--check-focus` instead of `--emit --watch-seconds N` with those target
arguments to perform a read-only eligibility check without initializing enigo.
Other compositor/session combinations remain unverified. No GameCube adapter
or Steam Deck has been physically tested; SDL handles those protocols without a
custom decoder. Continuous analog delivery into Warcraft, Tauri and packaging
remain separate.

## Windows and macOS output

Each OS has one foreground adapter behind the `Foreground` trait in
`src/focus.rs`; the mapping, rearm rules and enigo output
are shared. `--emit --watch-seconds N` needs no target arguments there:

- **Windows:** the foreground window's process image must be `Warcraft III.exe`
  (`GetForegroundWindow`, `QueryFullProcessImageNameW`). Keys go through
  `SendInput` to that foreground window. A game running elevated cannot receive
  them and is never eligible.
- **macOS:** the frontmost application's executable must be `Warcraft III`
  (`NSWorkspace`). Each check drains the main run loop, which is where macOS
  publishes activation changes. Keys are posted to the HID event stream, which
  needs the Accessibility permission for the app that starts the helper; enigo
  prompts on first use.

`--pid PID` optionally pins one game process. `--check-focus` reports the
eligibility without opening keyboard output. The Linux-only target arguments are
rejected. The same foreground/submission race as on Linux applies. The service
and plug-ins are Linux-only; on Windows and macOS the map receives the
controller as ordinary keyboard keys.

`cargo test --locked --features e2e --test e2e -- --nocapture` runs the real
helper in a Windows or macOS desktop session against a scripted pad (on Windows
a ViGEmBus virtual DualShock 4, so ViGEmBus must be installed; on macOS the
helper's `--virtual-pad`) and two `wc3-standin` windows, one copied to the
Warcraft III executable name. It checks every mapped action, that the game
window's whole key sequence is exactly the expected one, both
ordered jump-button overlaps and the trigger overlap, that no key reaches either
window while the other one is focused, that focus loss and disconnect release
held keys in the operating system's key state, and that a control held through
refocus must return to neutral. `.github/workflows/ci.yml` runs it on
GitHub's `windows-latest` runner.

The SDL virtual helper also accepts `button back 0|1`, preserving View edges in
its input history. The fighter layout has no View action.

## Observed evidence

The Niri bridge repair passed its mapping/rearm and focused identity/recipient
tests and rebuilt successfully. Read-only native
checking established the bridge's XRes ownership and returned false while
Chrome was focused. The parent trial then reported true for the selected
Warcraft window after focusing it and moving the pointer over it. Neither check
opened keyboard output; game consumption and physical-controller delivery are
separate acceptance steps.

Linux x86_64: `cargo test` passed the tests covering the requested bindings,
overlapping triggers/jump sources, focus/disconnect release and neutral rearm.
`cargo build --locked --jobs 2` succeeded. Read-only `--list` using SDL 3.4.16
found one **Xbox One S Controller**, VID 045e/PID 02ea, at `/dev/input/event1`,
with SDL's Linux mapping and live normalized stick/trigger values. A two-second
observation run armed after the neutral sample. No physical button manipulation
was performed during those observations. Read-only `--check-focus` returned true
for the parent's selected A and B Warcraft windows in their separate labwc
sessions, with the exact supplied PID/XID/display and unique app IDs. This proves
foreground identity acquisition in those sessions, not gameplay eligibility:
the games were at login screens. Focus-away and key-delivery trials remain
unperformed. No keyboard injection or live Warcraft acceptance was run
while implementing this core. Third-party code is consumed as dependencies;
no Blizzard Controller Support, W3Champions, Dolphin or Slippi source was copied
or translated. SDL is zlib-licensed; sdl3 and enigo are MIT-licensed. Any future
distribution must retain dependency notices and satisfy transitive licenses.

## Always-on controller service (Linux)

`wc3-controller --service [--plugin PROGRAM] [--layout FILE]` finds Warcraft III
on display `:0` (a `Warcraft III.exe` process whose `DISPLAY` is `:0`; its Wine
prefix gives Documents/Warcraft III), the game's X11 window (`_NET_WM_PID`) and
its niri window (unique title and class), and the controller by its stable
`/dev/input/by-id/*-event-joystick` link, an Xbox pad first. A lock in
`$XDG_RUNTIME_DIR` keeps one service per display; its helpers end with it.
`~/.local/state/wc3-controller/service.txt` holds its current state
(`state=serving`, `game_pid`, `session`, `helper_pid`, ...). Discovery and the
helper's lifecycle are map-agnostic (src/service.rs); map-specific knowledge is
a plug-in (src/service/plugin.rs, "Map plug-ins" above).

Windows talk to it on TCP 127.0.0.1:47631 (src/service/interface.rs), one JSON
object per line in the types of `model` (crate `wc3-controller-model`): the
current `{"status":...}` on connect and on every change, `{"input":...}` for
every change of the pad's state (read without grabbing it), and from the window
`{"profile":"auto"|"map"|"any_map"|"off"}`, `{"bindings":[...]}`,
`{"pad_preset":...}`, `{"tap_jump":...}`, `{"trigger_shields":...}`, `{"right_stick":...}` and
`{"remaps":...}`. `auto`
(the default) runs the map plug-in while it reports a session, and Any map
otherwise. Any other profile stops the plug-in's helper, and the plug-in stops
Any map. Holding the port is part of being single-instance; `--interface off`
opens none. `--display`, `--pads`, `--status`, `--settings` and
`--headless DOCUMENTS` (a stand-in game whose `game` file names it; a helper
types into `typed.txt` and pad output goes to `pressed.txt`) serve tests.

A login unit runs it, for example with systemd:

```ini
[Service]
ExecStart=%h/.local/share/wc3-controller/bin/wc3-controller --service --plugin %h/path/to/your-plugin
Restart=always
```

Install it from a tag with
`cargo install --locked --git https://github.com/tompassarelli/wc3-controller --tag vX.Y.Z --root ~/.local/share/wc3-controller wc3-controller`.

### Any map profile

`model::any_map_bindings()` puts the camera arrows on the left stick, the
pointer on the right stick, left and right click on A and B, Q/W/E/R on X, Y,
RT and LT, control groups 1 and 2 on the bumpers, 3 and 4 on D-pad up and
right, F1 (hero) and Tab on D-pad down and left, F10 on Start and Escape on
Back. A layout file or a window's bindings replace it. Keys, clicks and pointer
motion go through XTEST on the game's display only while niri's focused window
is the game's; losing focus releases everything, and nothing presses again
until the pad is neutral.

### Settings

The service saves the fighter layout's preset, tap jump, trigger choices, right stick mode and remaps in
`$XDG_CONFIG_HOME/wc3-controller/settings.json`, or
`~/.config/wc3-controller/settings.json`, and restores them at startup; a
connected window follows these choices instead of replacing them with its own
defaults. `--settings FILE` uses another file for an isolated service.

## Status model

`model/` (crate `wc3-controller-model`, re-exported as `wc3_controller::model`)
holds what the controller service knows (pad, game, map session, active
profile, the plug-in's map title, whether presses reach the game), the
newline-delimited JSON messages on its local interface, the plain-language rows
and status light a window shows, the built-in binding tables, and the Any map
profile's mapper (keys, clicks and pointer from configurable bindings). It has
no I/O and builds without SDL, so windows depend on it alone:

```toml
wc3-controller-model = { git = "https://github.com/tompassarelli/wc3-controller", tag = "vX.Y.Z" }
```

## Analog comparison candidates

The production default stays digital until the native comparison chooses a
channel. The Linux service can opt into either candidate with
`WC3_PAD_INGRESS=keys` or `WC3_PAD_INGRESS=cursor`; cursor also needs
`WC3_PAD_CURSOR_GRID=X,Y,W,H,SCREEN_W,SCREEN_H`. The numbers are a rectangle and
compositor output extent in logical pixels. Use the same output containing the
game window and choose a rectangle inside its flat-ground diagnostic view.
Restart the service after changing these variables.

For a bounded native test, `wc3-controller --emit --watch-seconds N` accepts
`--pad-ingress keys|cursor` and `--cursor-grid X,Y,W,H,SCREEN_W,SCREEN_H`, with the
usual exact game-window and foreground arguments. Add `--virtual-pad` to feed
SDL acquisition from stdin, for example `axis leftx 16384`, `axis leftx 32767`,
`axis lefttrigger -7068` (SDL normalizes trigger joystick units to 0..32767),
`button a 1`, `button a 0`, and `quit`. Keep stdin open between commands. A
physical pad uses the same SDL capture path; the service uses evdev InputView.

Both candidates first apply `melee_stick` radial clamp/deadzone and then use
17 signed stick levels and four trigger levels from `model::pad`. The 14-bit
payload packs X index in bits 0..4, Z index in 5..9, LT in 10..11 and RT in
12..13. Key output holds F13..F24, Insert and Delete for those bits. Home stays held while the pad
is armed and focused; End marks a complete payload. Payload changes release End, change the bits, then press
End. Cursor output sends low seven bits as the horizontal cell and high seven
bits as the vertical cell across a 128 by 128 grid, with Home and End active.
The map keeps the previous complete payload while Home is held and End is
released during a write, and clears it when Home is released. Existing
action keys are still emitted. Focus loss and disconnect release the carrier;
controls must return to neutral before it becomes active again. Experimental
output logs each submitted `pad_ingress payload=... x=... z=... left=... right=...`
record to stderr alongside the ordinary timestamped action history on stdout.

Cursor calibration is a separate bounded operation, requiring no controller:
run `wc3-controller --emit --watch-seconds N --cursor-calibrate start` with
`--cursor-grid` and the usual foreground arguments after the diagnostic map's
camera is fixed. It holds Home, End and PageUp and moves to cell (0,0). Wait for the
map's calibration observation, then allow the helper to exit. Repeat with
`--cursor-calibrate end` for PageDown and cell (127,127). Each operation releases
its markers when it exits or loses focus. The native runner must observe both
calibration callbacks before starting cursor playback; the duration alone is
not a calibration acknowledgment.
