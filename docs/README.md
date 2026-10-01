# Code-server in POM

This plugin runs the official [code-server](https://github.com/coder/code-server) release and mounts its VS Code web editor in the POM admin UI. The editor opens the POM's shared `workspace_root`; its settings, extensions and application state persist in the plugin's private data directory.

## Access and runtime

The menu and route are **admin-only** because the editor can run terminal commands as the POM process and access files visible on that node. code-server is configured with `--auth=none`, but binds only to a private loopback port. A second loopback proxy requires the per-launch token supplied by POM's `ui.upstream` proxy contract; browser traffic reaches it only through the POM's same-origin, admin-authorized plugin proxy. Neither the code-server port nor its token is sent to the browser.

The official platform-specific release archive is resolved and checksum-verified while building the plugin package, then embedded in the native library. Installing the plugin does not require the POM node to reach GitHub or npm. On first activation the runtime archive is unpacked under the plugin's runtime directory; subsequent starts reuse it. Workspace data and extensions are kept outside that replaceable runtime.

The build follows the latest official release by default. Pin a version to reproduce a package:

```sh
scripts/package.sh --platform linux-x86_64 --version 0.1.0 --code-server-version 4.139.1
```

Supported package targets are Linux x86_64, macOS arm64 and Windows x86_64, matching the official code-server release assets. Build each target on its native runner as the release workflow does.

## Build and verify

Requirements: Rust stable, Node.js 20 or newer, npm, curl, tar, jq.

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
node --test tests/unit/*.test.mjs
scripts/build-ui.sh
```

A plain `cargo build` is useful for ABI/UI development but has no embedded code-server archive. Use `scripts/build.sh` or `scripts/package.sh` for a runnable plugin artifact.

## POM integration

The plugin receives `workspace_root` from `host.configure`; if the POM has not configured a workspace, it uses a plugin-private `data/workspace`. `ui.upstream` publishes only the authenticated loopback proxy. POM mounts that proxy below `/api/ui/plugins/code_server/proxy/`, preserving HTTP and WebSocket requests required by VS Code.
