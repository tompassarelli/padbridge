# Architecture: where code lives

padbridge turns a game pad into key, click and pointer events for any Warcraft III map, runs a map's plug-in, and reports status over TCP.
It names no map, fighter, phase or person: a map's layouts and words reach it through the map's plug-in.
Paths are repo-relative; the tables say where things are today, and "moves in Pn" names the slice that relocates them (last section).

## Modules and use direction

A module may use only the modules in its "may use" column. Nothing below the service does I/O it does not name.

| module | today | responsibility | may use |
|---|---|---|---|
| protocol | `model/src/lib.rs` (crate `padbridge-model`), `src/service/plugin.rs` (`Hello`, `Request`) | Everything that crosses a process boundary: wire, plug-in, settings, status; serde only, no I/O | none |
| bindings | `model/src/lib.rs` (`Control`, `Press`, `Binding`), `model/src/any_map.rs` (`Mapper`), `src/stick.rs` | Pad state + bindings -> key, click and pointer events | protocol |
| emit | `src/lib.rs` (`Action`), `src/output.rs`, `--emit` half of `src/main.rs` | The helper core: SDL sample, event mapping, enigo delivery | bindings, protocol |
| focus | `src/focus.rs`, `src/wlr.rs` | Foreground gate per OS | none |
| service | `src/service.rs`, `src/service/` | Warcraft/pad/window discovery, plug-in supervision, TCP interface, Any-map desktop output | emit, focus, bindings, protocol |
| cli | `src/main.rs`, `src/bin/standin.rs` | Entry points | service, emit, focus, protocol |

Known breaks still in the tree (removed by slices, never copied): fighter presets, `CharacterSelect`/`StageSelect` and `contract/` in the model (P1);
Smashcraft's analog packet in `model/src/pad.rs` and `src/pad_ingress.rs` (P2); `src/service.rs` and `src/main.rs` each doing several jobs (P3).

## Where things live

| change | edit today | target |
|---|---|---|
| A generic binding control or press kind | `model/src/lib.rs` (`Control`, `Press`, `Binding`), `src/lib.rs` (`Action::key`) | `protocol/src/bindings/vocab.rs` in P1; `Action` -> `KeyOrClick` in `src/emit/` in P3 |
| A fighter key or game action | `model/src/lib.rs` (`fighter_bindings*`), `contract/bindings.json`, `src/service/any_map.rs` contract tests | not here: smashcraft `ts/src/game/input/keyBindings.ts` + `bun run generate` after K1; padbridge copies go in P1, tests move to smashcraft in P2 |
| A new pad preset | `model/src/lib.rs` (`PadPreset`, `fighter_bindings_for`) | a layout JSON the map ships (smashcraft `contracts/layouts/<name>.json`), carried by the plug-in `Hello`, in P1 |
| Any-map default layout | `model/src/lib.rs` (`any_map_bindings`), sample `examples/echo-isles/layout.json` | `protocol/src/bindings/` in P1; the sample stays |
| Stick, trigger and pointer defaults | `src/stick.rs`, `model/src/any_map.rs` | one copy in `protocol/src/bindings/stick.rs` in P1 |
| Service TCP protocol (127.0.0.1:47631) | framing `src/service/interface.rs`; messages `model/src/lib.rs` (`ServiceMessage`, `ClientMessage`, `InputView`) | messages to `protocol/src/wire.rs` in P1; framing stays |
| Plug-in protocol | `src/service/plugin.rs` (`Hello`, `Request`, `serve`), `Profile` in `src/service.rs`; prose in README "Map plug-ins" | `protocol/src/plugin.rs` in P1 (`Hello` gains `layouts`); supervision stays in `src/service/plugin.rs` |
| Settings JSON | `model/src/lib.rs` (`ControllerSettings`), `src/service/settings.rs` (load/save) | `protocol/src/settings.rs` in P1 |
| Status snapshot, phase, lights | `model/src/lib.rs` (`Snapshot`, `Phase`, `View`) | `protocol/src/status.rs` in P1; `Phase` becomes `Idle/Menu/Playing` + plug-in `label` |
| Sony/Xbox face positions | `model/src/lib.rs` (`PadKind`, `Button`) | `protocol` in P1; plug-ins read them from `input` |
| Warcraft, pad and window discovery | `src/service.rs` | `src/service/{discovery,supervisor,run}.rs` in P3 |
| Desktop keys, clicks, pointer | `src/service/any_map.rs`, `src/service/pointer.rs` | stays |
| The `--emit` helper | `src/main.rs`, `src/lib.rs`, `src/output.rs` | `src/bin/padbridge-emit.rs`, `src/emit/` in P3 |
| Foreground gate | `src/focus.rs`, `src/wlr.rs` | `src/focus/` in P3 |
| Smashcraft analog packet | `model/src/pad.rs`, `src/pad_ingress.rs`, `contract/tom-saved-bindings.pld` | smashcraft `controller/` in P2 |
| Client's controller page, records/replays, launching Warcraft, online play | not here: smashcraft-client `ui/src/pages/controller.ts`, `src-tauri/src/records.rs`, `src-tauri/src/replays.rs`, `src-tauri/src/play.rs`, `src-tauri/src/online.rs` | see smashcraft-client `docs/architecture.md` |
| Release | `Cargo.toml`, `model/Cargo.toml`, an annotated `vX.Y.Z` tag | `protocol` changes bump the minor and list messages in `CHANGELOG.md` from P1 |

Tests: unit tests beside their code; `tests/e2e.rs`, `tests/layout.rs`, `tests/service.rs` are the boundary checks.

## Contract owners

| contract | owner | padbridge gets it by |
|---|---|---|
| Game actions, key map, `.pld` bindings, analog packet, carrier keys | Smashcraft `contracts/controller.json` (K1) | nothing: only smashcraft `controller/` reads it |
| Fighter layouts, their thresholds | Smashcraft `contracts/layouts/*.json` (K1) | the plug-in `Hello` (P1) |
| Wire, plug-in, settings, status types; generic stick defaults; face positions | padbridge `protocol` | consumers pin a tag; the client generates its TypeScript from these types (C1) |

## Target and slices

- Crates: `protocol/` (renamed from `model/`, crate `padbridge-protocol`: `wire.rs`, `plugin.rs`, `settings.rs`, `status.rs`, `bindings/{vocab,mapper,stick}.rs`), then `src/{emit,focus,service}/`, then `src/bin/{padbridge,padbridge-emit,standin}.rs`.
- README documents the protocol and links the layout schema; smashcraft `docs/controller-platforms.md` owns the fighter layout text.
- Each slice moves files and every caller in one change, with no re-export shims.

1. K1 (smashcraft first): `contracts/controller.json` and `contracts/layouts/*.json` from `ts/codegen/controllerContract.ts`.
2. P1: model -> `protocol` + `bindings`; layouts from the plug-in `Hello`; fighter presets, fighter phases and `contract/` removed; smashcraft controller pin bumps in the same window (the client stays on v0.3.6 until C1).
3. P2: `model/src/pad.rs`, `src/pad_ingress.rs` and the contract tests move to smashcraft `controller/`.
4. P3: `src/service.rs` and `src/main.rs` split; `Action` -> `KeyOrClick`; one `Control`, one `Mapper`.

Checks: `cargo test --locked --workspace` (with `tests/e2e.rs`, `tests/layout.rs`, `tests/service.rs`); smashcraft `bun test ts/test/pad-contract.test.ts` and `controller/tests/service.rs`.
Payoff: a key or preset change edits `keyBindings.ts` in Smashcraft only, instead of 3 repos and 5+ files.
