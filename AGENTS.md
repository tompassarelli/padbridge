# padbridge

- Keep it map-agnostic: nothing names a particular map; map behaviour belongs in a plug-in (README, "Map plug-ins") kept with its map.
- Keep the model crate free of I/O and SDL so windows depend on it alone.
- Build and test with the pinned toolchain: `cargo test --locked --workspace` (tests/layout.rs and tests/service.rs need a writable /dev/uinput).
- Keep tests to the five kinds in north:harness/skills/testing/SKILL.md.
- Respect Blizzard's IP (README, "Principles"): if a task seems to need memory reads, disassembly, decompilation or Blizzard or Nintendo files in the repo, stop and measure from outside instead.
- Release by pushing an annotated `vX.Y.Z` tag; CI attaches the Linux and Windows builds.
- Keep the model crate to the wire protocol and the binding vocabulary: a map's layouts, presets and phase names come from the contract its plug-in publishes, never from code here.
- Take controller constants (action keys, carrier keys, packet levels, saved bindings) from smashcraft `contracts/controller.json`; never retype them here.
- Change a type that crosses a process boundary only in the model crate, bump the minor version and name the message change in the release.
- Place a change by its row in docs/architecture.md; when a slice moves a file, move every caller in the same change, with no re-export shims.

Commands and formats: README.md. Where code lives and where it is moving: docs/architecture.md.
