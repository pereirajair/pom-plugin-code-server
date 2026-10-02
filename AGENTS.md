# Project agent memory

- `runtime/launcher.mjs` runs official code-server behind the authenticated loopback proxy; keep its stdout to one JSON readiness line and never inherit the plugin host's IPC streams into child processes.
- The package pins the official release (`fetch-runtime.sh --metadata-only` -> `CODE_SERVER_URL/SIZE/SHA256/VERSION/ROOT` in `build.rs`) and never embeds it; `src/supervisor.rs` downloads, verifies and unpacks it on the node (`installing` status, `rpc.runtime.status`/`rpc.runtime.retry`). Use `scripts/package.sh` for an installable artifact, not bare Cargo.
- POM models reach the editor chat through the launcher's Ollama-shaped loopback facade (key injected server-side), `chatLanguageModels.json`, `chat.allowAnonymousAccess` and the one-time `_pom/seed.js` that re-enables the built-in chat extension in the browser. See docs/README.md before changing any of these.
- The UI's route and menu are intentionally admin-only because code-server grants terminal and filesystem access as the POM process. The POM plugin proxy handles same-origin browser access and WebSockets.
- Verify with `cargo test --locked`, `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `node --test tests/unit/*.test.mjs`, and `scripts/build-ui.sh`.
