# wc3-controller

profile: prototype

- Keep it map-agnostic: nothing names a particular map; map behaviour belongs in a plug-in (README, "Map plug-ins") kept with its map.
- Keep the model crate free of I/O and SDL so windows depend on it alone.
- Build and test with the pinned toolchain: `cargo test --locked --workspace` (tests/layout.rs and tests/service.rs need a writable /dev/uinput).
- Release by pushing an annotated `vX.Y.Z` tag; CI attaches the Linux and Windows builds.

Commands and formats: README.md.
