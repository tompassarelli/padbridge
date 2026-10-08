# Warcraft controller core

UI-less Rust companion targeting Linux, Windows and macOS. SDL3 owns discovery,
normalization and adapter protocols; enigo delivers key transitions. The default
is **observation only**: no keyboard injection, window creation, or focus changes.

## Build and observe

From the checkout's `smashcraft:companion` directory, use Rust 1.96.1 (pinned in
`smashcraft:companion/rust-toolchain.toml`), a C/C++ compiler and CMake. Linux also
needs the libudev and libxkbcommon development libraries. Dependencies, including
SDL 3.4.16 built statically through sdl3 0.20.0, are locked in
`smashcraft:companion/Cargo.lock`. Build output stays in
`smashcraft:companion/target`.

```sh
cd ~/code/smashcraft/worktrees/controller-core-20261003/companion
cargo test --locked --jobs 2
cargo build --locked --jobs 2
cargo run --locked -- --list
cargo run --locked -- --watch-seconds 20
# When several controllers are present, select an ID printed by --list:
cargo run --locked -- --watch-seconds 20 --gamepad 1
```

For this NixOS workstation, the observed build environment is:

```sh
nix-shell -p bun stdenv.cc cmake pkg-config libxkbcommon udev
export PATH="$HOME/.rustup/toolchains/1.96.1-x86_64-unknown-linux-gnu/bin:$PATH"
bun ~/.codex/skills/machine-capacity/scripts/machine-capacity.mjs run \
  --class moderate --owner codex:controller-build --timeout-seconds 900 -- \
  cargo build --locked --jobs 2
```

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

## Xbox mapping

| Control | Action / logical key |
| --- | --- |
| A / X | Attack N / special U |
| B or Y | Jump I |
| Left stick click (L3) | Short hop Z (even held) |
| RB / LB | Grab O / Tilt P |
| LT / RT | Full shield Q / full shield Q |
| Start | Y |
| Left stick left / right / down | W / R / E |
| Left stick up | Space (up only; no tap jump) |
| Right stick up / right / down / left | J / M / H / B |

The standard pad preset keeps B and Y as jump and RB as grab. Select **Z-jump**
on the Controller page to make RB and Y jump and B grab; the other controls stay
the same. Both `wc3-controller` and `wc3-journal` accept `--preset standard|z-jump`.
The journal also accepts `WC3_PAD_PRESET`. Both triggers shield fully by default.
Either trigger can instead light shield: choose Full shield or Light shield for
each trigger on the Controller page, or use `--left-trigger full|light` and
`--right-trigger full|light` on either helper. Light shield presses T and requests
pressure 77; full shield presses Q and requests 255.

The service saves the layout, tap jump and trigger choices together in
`$XDG_CONFIG_HOME/smashcraft/controller.json`, or `~/.config/smashcraft/controller.json`.
It restores them at startup; a connected Controller page follows these choices
instead of replacing them with its own defaults. `wc3-journal --service --settings FILE`
uses another file for an isolated service.

Tap jump is off by default. The Controller page can enable it, or either helper
accepts `--tap-jump on|off` (the journal also reads `WC3_TAP_JUMP`). When enabled,
stick up past 0.6625 requests jump. Holding Tilt plus shield caps the effective
stick at 0.65 before tap jump, so the shield can tilt up without jumping.
Jump buttons keep working while tilting the shield.

In the map's fighter, opponent settings, stage and results menus, the controller service makes
the left stick a pointer, as the hand cursor is in Smash: it moves the desktop
pointer over the game (the compositor's virtual pointer), A left-clicks to
choose a tile, chip or button, B right-clicks, and Start still sends Y (stage
selection, start). The pointer rests inside 0.12 of full deflection, speeds up
with deflection to the power 1.7, and at full tilt crosses the game window
in one second. It runs only while the map keeps publishing an open menu
(CHARACTER, CPU, STAGE or RESULT, refreshed every 250 ms) and the helper reports no
match, and stops within 100 ms of the map publishing BLOCKED for play, where
the table above applies unchanged (smashcraft:companion/src/service/any_map.rs,
`MenuCurve`; model::smashcraft_menu_bindings).

The assigned offline client's menu check uses
`cargo test --locked --test service a_private_pad_script -- --ignored --nocapture`.
Set `WC3_MENU_DISPLAY`, `WC3_MENU_XID`, `WC3_MENU_PID`, `WC3_MENU_APP_ID`,
`WC3_MENU_DATA` (CustomMapData), `WC3_MENU_WIDTH` (logical pixels), and
`WC3_MENU_SCRIPT`, alongside that private desktop's `XDG_RUNTIME_DIR`,
`WAYLAND_DISPLAY` and `XAUTHORITY`. The script's rows are milliseconds,
left-stick X and Y (-32768..32767), A (0/1), and Start (0/1); `#` lines are
comments. Start with a neutral row. Menu changes use the same driver as the
service and match phases return to the standard fighter keys. The native
executor checks the selected fighter and match receipt after the script.

Both sticks use Melee's conversion on every pad (smashcraft:companion/src/stick.rs,
shared by wc3-controller and wc3-journal). The stick is first clamped radially
to full scale, as Melee does at its 80-unit full scale. Each axis whose magnitude is then at most
**0.28** of full scale reads 0; a value outside it is kept, not rescaled.
Full scale is SDL's and the normalized evdev range, ±32767, standing in for
Melee's 80 units, so a left-stick axis counts from 9175. No resting-offset
calibration is applied; the deadzone absorbs a pad's resting offset. Left,
right and up are active when their axis is outside the deadzone; down needs
the stronger threshold below. The journal's rows carry the deadzoned axes. Right-stick directions
use the flick thresholds below and triggers need 4000, strictly beyond. Shared sources are unioned before emission:
releasing LT while RT is held retains shield; B and Y share one held jump
action. Stick-up is only up: aim, up-special, getup and ledge stand. Hold
duration remains available to the game's jump logic.

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
dodge press still use the deadzone, as does the journal's analog axis.

The right stick presses a direction at Melee's smash-flick thresholds, after the
same clamp and deadzone: **0.8** sideways and **0.6625** up or down (axis
values from about 26215 and 21709). Our design targets: smash side 0.8; smash up and down,
get-up up, ledge-attack up and the C-stick up jump 0.6625; ledge-attack sideways
0.8. Lower thresholds (aerials, throws, side get-up and ledge climb, 0.25 or
0.2) fall inside the 0.28 deadzone, and C-stick roll and spot dodge use 0.7. One digital press per direction takes the smash
values, so an aerial, throw or side get-up by C-stick needs a harder flick than
in Melee. A full-scale diagonal clamps to 0.707 per axis, so it presses up or
down but not sideways, as in Melee.

On Linux the journal reads face buttons by position. Sony's driver reports
positions, but xpad and other Xbox-style drivers report labels: X as `BTN_X`
(0x133, the code also named `BTN_NORTH`) and Y as `BTN_Y` (0x134, `BTN_WEST`).
The journal decides by vendor exactly as SDL's Linux mapping does, so X is
special and Y jumps on both helpers; its log prints `face_labels`.
The Xbox preset is not a claim that GameCube letter labels have the same meaning.

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
`smashcraft:companion/src/focus.rs`; the mapping, rearm rules and enigo output
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
rejected. The same foreground/submission race as on Linux applies. The journal
binary (`wc3-journal`) remains Linux-only; on Windows and macOS the map receives
the controller as ordinary keyboard keys with its standard QWERTY bindings.

`cargo test --locked --features e2e --test e2e -- --nocapture` runs the real
helper in a Windows or macOS desktop session against a scripted pad (on Windows
a ViGEmBus virtual DualShock 4, so ViGEmBus must be installed; on macOS the
helper's `--virtual-pad`) and two `wc3-standin` windows, one copied to the
Warcraft III executable name. It checks every mapped action, that the game
window's whole key sequence is exactly the expected one, both
ordered jump-button overlaps and the trigger overlap, that no key reaches either
window while the other one is focused, that focus loss and disconnect release
held keys in the operating system's key state, and that a control held through
refocus must return to neutral. `smashcraft:.github/workflows/companion.yml`
runs it on GitHub's `windows-latest` and `macos-latest` runners.

The SDL virtual helper also accepts `button back 0|1`, preserving View edges in
its input history. The playable keyboard layout has no View action; journal
View saves remain on the journal route. `bun scripts/nativeKeyboardPad.ts` in
`ts/` replays physical pad deadlines through this helper for native draw timing
and records independent CLOCK_MONOTONIC write brackets. Use Ctrl+H after
capture to export the response probe.

## Observed evidence

The Niri bridge repair passed seven tests (five mapping/rearm tests and two
focused identity/recipient tests) and rebuilt successfully. Read-only native
checking established the bridge's XRes ownership and returned false while
Chrome was focused. The parent trial then reported true for the selected
Warcraft window after focusing it and moving the pointer over it. Neither check
opened keyboard output; game consumption and physical-controller delivery are
separate acceptance steps.

Linux x86_64: `cargo test` passed all five tests covering the requested bindings,
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

`wc3-journal --service` takes no arguments. It finds Warcraft III on display
`:0` (a `Warcraft III.exe` process whose `DISPLAY` is `:0`; its Wine prefix
gives Documents/Warcraft III), the game's X11 window (`_NET_WM_PID`) and its
niri window (unique title and class), and the controller by its stable
`/dev/input/by-id/*-event-joystick` link, an Xbox pad first. The Smashcraft
profile reads the newest menu publication in CustomMapData for the build,
slot and current epoch, and the service runs one `--follow-matches` helper for
them (`--epoch` passes the map's current epoch, so a helper started mid-session
drives that menu). It replaces the helper when Warcraft III restarts or its
window changes, when a newer menu publication starts a new map session (a
lower epoch, or another build or slot), when the helper has been without its
pad for 3 s while a pad is plugged in (a pad that came back as another
device), and 2 s after a helper exits for any reason. A lock in
`$XDG_RUNTIME_DIR` keeps one service per display; its helpers end with it.
`~/.local/state/smashcraft/controller-service.txt` holds its current state
(`state=serving`, `game_pid`, `session`, `helper_pid`, ...). Discovery and
the helper's lifecycle are map-agnostic in smashcraft:companion/src/service.rs;
map-specific knowledge is a `Profile` (smashcraft:companion/src/service/smashcraft.rs).
Windows talk to it on TCP 127.0.0.1:47631 (smashcraft:companion/src/service/interface.rs),
one JSON object per line in the types of smashcraft:companion/model: the current
`{"status":...}` on connect and on every change, `{"input":...}` for every change
of the pad's state (read without grabbing it), and from the window
`{"profile":"auto"|"smashcraft"|"any_map"|"off"}` and `{"bindings":[...]}`.
`auto` (the default) runs Smashcraft while this game has published a
Smashcraft menu since it started, and Any map otherwise.

A build that reads keyboard input (the playable build, #166)
publishes menu phases too: its ready file `CustomMapData/wc3-melee-ready.txt` names
the build and `INPUT keyboard-d2-r24` (the development build uses `INPUT callback`). Newer than the game's start and any different build's menu,
it is a Smashcraft session on keys (`session=BUILD/keys/N`): the service
runs no helper, uses the pointer in fighter, stage and results menus, and presses the pad's keys during matches through the same mapper as
`--emit` (the table above: A n, X u, B/Y i, RB o, LB p, either trigger q,
Start y, left stick w/r/e/space, right stick b/m/j/h), into the game's window
while niri focuses it. Focus loss releases them, and nothing presses again
until the pad is neutral. The status reads `state=serving` once that output runs.

The Any map profile (smashcraft:companion/src/service/any_map.rs) plays any
map, melee included: `model::any_map_bindings()` puts the camera arrows on the
left stick, the pointer on the right stick, left and right click on A and B,
Q/W/E/R on X, Y, RT and LT, control groups 1 and 2 on the bumpers, 3 and 4 on
D-pad up and right, F1 (hero) and Tab on D-pad down and left, F10 on Start and
Escape on Back. Keys, clicks and pointer motion go through XTEST on the game's
display only while niri's focused window is the game's; losing focus releases
everything, and nothing presses again until the pad is neutral. Any other
profile stops the Smashcraft helper, and Smashcraft stops Any map. Holding
the port is part of being single-instance; `--interface off` opens none.
`--display`, `--pads`, `--status` and `--headless DOCUMENTS` (a stand-in game
whose `game` file names it; the helper types into `typed.txt`) serve tests.
smashcraft:companion/tests/service.rs drives a fake session through a new map
session and a game restart with a virtual pad.

## Experimental Linux original-frame journal

`wc3-journal` is a separate Linux evdev acquisition executable. It uses kernel
CLOCK_MONOTONIC event timestamps rather than SDL's clock conversion, retains
per-frame edges/analog state, and atomically publishes immutable I4 preload
files for the production journal input source. Build it with the same pinned
environment above using `cargo build --locked --jobs 2 --bin wc3-journal`.

For the editbox map, start one helper **in character selection** and
leave it running through results and rematches:

```sh
~/code/smashcraft/worktrees/playable-integration-20261005/companion/target/debug/wc3-journal \
  --follow-matches --build BUILD --slot 0 \
  --device /dev/input/eventN --out '/absolute/Warcraft III/CustomMapData' \
  --editbox-display :N --x11-window DECIMAL_ID --pid PID \
  --niri-window WINDOW_ID --trace
```

Select the exact device, build, slot and focus target. The Linux Xbox axis set
is required. The helper opens the device and tracks its physical state before
announcing readiness. It accepts only new complete readiness publications after
it starts (or the preceding match ends), and only increasing within-map epochs.
Old map-session receipts are not adopted. In character selection, left-stick
left/right cycles the character, A selects the current character, X recalls
your selection, and Start opens stage selection once everyone has selected.
In stage selection, left/right changes the stage, X returns to characters,
and A or Start begins the match. At results, A or Start confirms your rematch.
Up/down visits fighter cards and CPU Opponent settings. In that panel,
left/right changes the focused value, up/down visits Opponent, Difficulty and
Done, A advances or chooses Done, X goes back, and Start closes without starting
a match. One stick deflection or button press produces one menu action; release before
the next action. The controller service starts the helper with `--menu-keys
start`: then only Start reaches the outer menus, and the service's menu pointer
does the rest, including in the CPU opponent settings panel. Held controls require neutral after startup, a menu phase
change, focus loss, and entering gameplay.

The map publishes `smashcraft-journal-menu-BUILD-sSLOT.txt`, containing
`SMASHCRAFT JOURNAL MENU v=1 build=BUILD epoch=EPOCH slot=SLOT phase=PHASE` in
a complete native preload file. PHASE is CHARACTER, CPU, STAGE, RESULT or BLOCKED.
Eligible menus refresh every 15 map ticks (normally 250 ms); the helper requires
a matching publication newer than its startup/previous match and no older than
one second. Initial character selection uses epoch 0. Results become eligible
only after all helpers have stopped and the text box has closed. BLOCKED,
missing, partial or stale receipts suppress menu actions.

Each accepted menu action uses the existing exact-window Enigo text boundary
to send a finite W/R/Space/E/N/U/Y press-release pair. It holds no menu keys, rechecks
map permission and game focus per tap, and never activates a window. The map's
journal menus give those keys fixed menu meanings independent of combat key
rebindings and mouse position. This covers ordinary fighter/stage/result menus;
mouse-only slot modes, settings, chat and other Warcraft screens are outside it.

Every human helper announces readiness through the ordered text ingress. The
map synchronizes those announcements before publishing local START. Capture
uses that complete file's stable modification timestamp, converted to the local
monotonic clock with measured uncertainty, just as resume does. START/END files
older than the accepted readiness publication are ignored. A delayed or partial
START read retains original input events; the helper never substitutes its read
time for the publication boundary. This defines a **local publication grid**;
it does not align clocks between machines or remove their inter-client offset.

At results, the map publishes END. The helper stops generating capture rows,
discards unfinished terminal rows, drains already queued old-epoch records, and
sends an ordered final marker. The map consumes those terminal records without
combat and flushes the final marker receipt. The helper observes that receipt
and publishes its fixed local quiescence acknowledgment; only then does the map
close the editbox. The helper clears an earlier acknowledgment before announcing
readiness, so it cannot satisfy a new match. Results controls unlock after every human helper has stopped. The same process
then follows the next fresh epoch with neutral rearming and empty match queues.
This supports within-map rematches; automatic map reload remains unsupported.
Logs identify `waiting_for_match`, `match_ready`, `match_start`, `match_end`, and
`match_quiescent`, with the epoch and startup timestamp uncertainty.

On controller removal, the journal retains earlier captured input and emits a
neutral release at the first unassigned frame at detection. It continues neutral
rows while disconnected. Reconnect discovery reads kernel identity files and
opens only a unique match for the selected Linux input ID, name, physical path
and unique name, then rechecks the opened device. At least one physical/unique
discriminator is required; missing or ambiguous identity never selects another
pad. Changing USB ports can change the physical path. Controls held on return
must become neutral before new gameplay, menu or pause inputs are accepted.
`controller_disconnected` records detection; `controller_release frame=N`
records its assigned release; `controller_reconnected source=...` identifies
the recovered event path. Bounded native recovery is recorded in
smashcraft:evidence/controller-reconnect-native-20261005/README.md.

Each record the helper types is an envelope of about 30 characters around
its payload, and it types at most 16 records past what the map's receipt says
it consumed. Warcraft takes typed text into the edit box at a cost that grows
with how much it takes at once: in 0.0.48's native bot session, the 16 records
(608 characters) typed after a 2 s stop held the client about 180 ms, and its
input stayed 15–25 frames late for 5 s
(smashcraft:evidence/bot-session-0048-native-20261006/). So the helper types
at most 160 characters past the record the receipt says arrived. While a
row packet waits untyped, consecutive packets from the same epoch combine
using the existing `I5` message encoding, up to 64 frames and only while the
whole envelope stays within those 160 characters. Holds keep buttons, axes
and triggers; every press, release and other edge retains its original frame.
Already typed packets and control records stay separate. The native #86
capture's 28 neutral frames used 155 characters as joined `I4` packets;
the same frames use 37 characters in one `I5` envelope.
The map writes a dirty text receipt every two ticks (at most 30 per client
per second), so the smaller window can drain promptly. The typing cost
model bounds 160 characters at 12.8 ms; native receipt-write cost remains
a separate check.

For Wisp's headless clients, `--text-out FILE` replaces `--editbox-display`
and the focus arguments: every text the helper would type into the game's
window (journal envelopes and menu keys) is appended to FILE as one line, and
`--out` names the folder the headless client writes its files to. There is
no window, so focus never suspends output; chat's Return key needs a game
window and stops the helper. `bun wisp integrity headless` starts it this way
(smashcraft:docs/typescript.md).

The explicit `--ready-file PATH --epoch-monotonic-ns NS` mode remains for native
diagnostic drivers. `--first-frame N` (default 1) and `--stop-frame N` belong to
that mode; it also accepts explicit build/epoch/slot/delay arguments. Its supplied
clock is a diagnostic assumption, never a cross-machine timing guarantee. A
diagnostic producer's first I4 row may announce readiness to the map.

Pause prepares each helper's input frontier, synchronizes the highest frontier
across humans, and commits the shared stop frame. Resume opens a new local
capture segment at the stable publication timestamp, retaining original tags.
Enter in the active controller receiver requests that same shared pause before
opening native chat. The helper drains all retained records through their
consumed receipts, then publishes its chat quiescence symbol. Only then does
the map hide and release the receiver and the helper press Return. Controller
actions remain suppressed until native chat closes and the map restores its
receiver. Closing chat leaves the match paused; neutral controls and a fresh
Start press resume it. No player can resume while another player is typing.

At match admission, the helper removes that epoch and slot's old end and chat
acknowledgements before announcing readiness. A new game reuses epoch and chat
request numbers, so these files must not satisfy a new handshake or collide
with its immutable publication. Other builds, epochs and slots are preserved.

The existing text receipt exposes `chat` (a within-epoch request number),
`chatState` (0 receiver restored, 1 draining, 2 focus released, 3 native chat
observed visible), and `chatFrame` (whether `ChatEditBar` was found). State 0
with the same nonzero request number follows observed native closure and
receiver restoration. Helper logs report `chat_state`, `chat_quiescent`, and
`chat_return`. Native acceptance must establish editbox Enter delivery,
`ChatEditBar` visibility, and the injected Return opening actual chat; source
tests alone do not establish those engine behaviors.

The helper waits for the native writer's closing line before parsing controls.
Native timing and graphical acceptance are distinct from the focused source
tests. This path remains Linux-only and separate from the digital keyboard mapper.
On kernel SYN_DROPPED or an event for an already-published frame, acquisition
stops with a diagnostic instead of inventing input or moving its original frame.

Current evidence and remaining acceptance are in
`roadmap #16`. The Linux journal path passed bounded tap/stall,
focus and pause/resume trials. Automatic start/results/rematch with the same
helpers passed two native match lifecycles; keyboard confirmed the menus.
See `smashcraft:evidence/match-lifecycle-native-20261005/README.md` for exact builds,
failed attempts and limits. Physical controller-to-screen timing, cross-machine
clock agreement, chat, physical reconnect and other-platform acceptance remain open.

### Journal keyboard focus boundary

Keyboard ingress (`--editbox-display` or `--mailbox-display`) additionally
requires `--x11-window DECIMAL_ID`, `--pid PID`, and exactly one foreground
adapter: `--niri-window ID` or `--private-wlr-app-id ID`. The latter uses the
selected private desktop's `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY`. These are
the same process/window/compositor checks used by the mapper, implemented in
smashcraft:companion/src/focus.rs and smashcraft:companion/src/wlr.rs. Missing or
failed target identity is an error; ordinary focus loss suspends keyboard output.

Focus loss leaves assigned rows and queued I4/ACK1/JP1 records in order. A logical
neutral release is assigned after any already-assigned open row; subsequent
unfocused input is explicitly suppressed. The 60 Hz capture segment continues,
so neutral rows retain the elapsed original frame numbers. Focus away for less
than 200 ms is a blip: input stays armed and held, and only typing waits for
focus. Focus checks have reported such blips of a few milliseconds during
play; each loss logs `focus_away` with what held focus instead (the Niri focused
window's id, app ID and PID, or the X11 active, focus and pointer windows), and
`focus_back` with its duration. A loss of 200 ms or more releases as above. On return, queued
records resume without retargeting, while fresh gameplay requires all mapped
controls (including Start) to become neutral. A held-through-return stick or
button cannot reactivate by itself. Start remains usable during game pause after
focus rearming; gameplay separately requires neutral after pause.

The queue permits at most 120 records and 2048 bytes including delimiters,
roughly four seconds of ordinary two-frame records (less for larger records or
control traffic). Exceeding either bound stops with an explicit error; records
are not overwritten. This is bounded focus recovery, not indefinite background
capture. Trace output distinguishes `game-eligible`, `focus_release`,
`input_armed`, `gameplay_armed`, suppressed kernel events and actual emissions.

Eligibility is sampled before queue capture and each keyboard API emission;
it is not an atomic compositor/keyboard transaction or a history of OS focus at
every kernel event timestamp. Already assigned rows are retained; unassigned
items observed while ineligible or before the recovery boundary are suppressed
and logged. Map-internal chat/editbox focus is a separate acceptance boundary.
No game activation or focus change is performed by the helper. The editbox path
uses Enigo's X11 `text_to_window`, including directed modifier events, so a
focus switch cannot route its text to another application's window. This does
not guarantee Warcraft consumes those events: the native focus trial retained
zero sink events but exposed a missing-frame gap on return. Native acknowledgment
and replay remain required before claiming focus-safe delivery; see
smashcraft:evidence/native-focus-20261005/README.md.

The editbox path holds no global transport keys; the legacy mailbox retains its existing signal state
until eligible and gates its cleanup too, so unfocused teardown does not emit
key releases to a different application.

## Status model and the Any map profile

smashcraft:companion/model (crate `wc3-controller-model`, re-exported as
`wc3_controller::model`) holds what the controller service knows (pad, game,
map session, active profile, whether presses reach the game), the
newline-delimited JSON messages on its local interface (`{"status":...}`,
`{"input":...}` from the service; `{"profile":"auto"|"smashcraft"|"any_map"|"off"}`
and `{"bindings":[...]}` to it), the plain-language rows and status light a
window shows, both profiles' binding tables, and the Any map profile's mapper
(keys, clicks and pointer from configurable bindings). It has no I/O and builds
without SDL, so windows such as smashcraft:client depend on it alone.
smashcraft:companion is a self-contained Cargo workspace (`cargo test --workspace`)
so it can move to its own repository; consumers then switch their path
dependency on `wc3-controller-model` to a git one.


## Analog comparison candidates (#204)

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
