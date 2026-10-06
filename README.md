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
bun ~/.codex/skills/machine-capacity-distilled/scripts/machine-capacity.mjs run \
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
| RB / LB | Grab O / walk P |
| LT or RT | Shield Q |
| Start | Y |
| Left stick left / right / down | W / R / E |
| Left stick up | Space (up only; no tap jump) |
| Right stick up / right / down / left | J / M / H / B |

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
helper in a Windows or macOS desktop session against an SDL virtual gamepad
(`--virtual-pad`, driven by stdin) and two `wc3-standin` windows, one copied to
the Warcraft III executable name. It checks the requested layout, both
ordered jump-button overlaps and the trigger overlap, that no key reaches either
window while the other one is focused, that focus loss and disconnect release
held keys in the operating system's key state, and that a control held through
refocus must return to neutral. `smashcraft:.github/workflows/companion.yml`
runs it on GitHub's `windows-latest` and `macos-latest` runners.

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
One stick deflection or button press produces one menu action; release before
the next action. Held controls require neutral after startup, a menu phase
change, focus loss, and entering gameplay.

The map publishes `smashcraft-journal-menu-BUILD-sSLOT.txt`, containing
`SMASHCRAFT JOURNAL MENU v=1 build=BUILD epoch=EPOCH slot=SLOT phase=PHASE` in
a complete native preload file. PHASE is CHARACTER, STAGE, RESULT or BLOCKED.
Eligible menus refresh every 15 map ticks (normally 250 ms); the helper requires
a matching publication newer than its startup/previous match and no older than
one second. Initial character selection uses epoch 0. Results become eligible
only after all helpers have stopped and the text box has closed. BLOCKED,
missing, partial or stale receipts suppress menu actions.

Each accepted menu action uses the existing exact-window Enigo text boundary
to send a finite W/R/N/U/Y press-release pair. It holds no menu keys, rechecks
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

For Wisp's headless clients, `--text-out FILE` replaces `--editbox-display`
and the focus arguments: every text the helper would type into the game's
window (journal envelopes and menu keys) is appended to FILE as one line, and
`--out` names the folder the headless client writes its files to. There is
no window, so focus never suspends output; chat's Return key needs a game
window and stops the helper. `bun wisp parity headless` starts it this way
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
