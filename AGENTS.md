# Project agent memory

- `runtime/launcher.mjs` runs official code-server behind the authenticated loopback proxy; keep its stdout to one JSON readiness line and never inherit the plugin host's IPC streams into child processes.
- `scripts/fetch-runtime.sh` checksum-verifies the platform's official GitHub release; `build.rs` embeds the archive only when `CODE_SERVER_ARCHIVE` is set. Use `scripts/package.sh` for an installable artifact, not bare Cargo.
- The UI's route and menu are intentionally admin-only because code-server grants terminal and filesystem access as the POM process. The POM plugin proxy handles same-origin browser access and WebSockets.
- Verify with `cargo test --locked`, `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `node --test tests/unit/*.test.mjs`, and `scripts/build-ui.sh`.
