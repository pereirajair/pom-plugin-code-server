# Code-server in POM

This plugin runs the official [code-server](https://github.com/coder/code-server) release and mounts its VS Code web editor in the POM admin UI. The editor opens the POM's shared `workspace_root`; its settings, extensions and application state persist in the plugin's private data directory.

The Plugins page screenshot gallery uses the two images requested for this plugin: the [VS Code logo](https://miro.medium.com/0*S0gllBsD11p4kfwO.png) and the [editor screenshot](https://user-images.githubusercontent.com/35271042/118224532-3842c400-b438-11eb-923d-a5f66fa6785a.png).
## Access and runtime

The menu and route are **admin-only** because the editor can run terminal commands as the POM process and access files visible on that node. code-server is configured with `--auth=none`, but binds only to a private loopback port. A second loopback proxy requires the per-launch token supplied by POM's `ui.upstream` proxy contract; browser traffic reaches it only through the POM's same-origin, admin-authorized plugin proxy. Neither the code-server port nor its token is sent to the browser.

The package build only resolves the official platform-specific release (`scripts/fetch-runtime.sh --metadata-only`) and embeds its URL, size, GitHub-published SHA-256 and version; the plugin artifact is a few megabytes. On first activation the node downloads that release from GitHub (honouring `HTTPS_PROXY`), verifies the pinned SHA-256 and unpacks it under the plugin's runtime directory; the editor screen shows the download progress through the POM plugin RPC (`rpc.runtime.status`, retried with `rpc.runtime.retry`). Subsequent starts reuse the runtime. The node therefore needs to reach `github.com` once per code-server version. Workspace data and extensions are kept outside that replaceable runtime.

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

## POM models in the editor chat

VS Code's built-in chat (the open-source Copilot Chat bundled with code-server) accepts an Ollama server as a model provider configured by URL only. The launcher serves that Ollama shape (`/api/version`, `/api/tags`, `/api/show`, `/v1/*`) on a loopback port under a random path, and forwards chat requests to the POM's OpenAI-compatible API (`gateway.openai_base_url` from `host.configure`) with the POM API key minted for this plugin. The key stays in the launcher process: it is never written to code-server's settings or sent to the browser.

On each start the launcher writes the `POM` provider group into `user-data/User/chatLanguageModels.json` (other groups are kept) and sets `chat.allowAnonymousAccess: true` in the user settings when the person has not set it. VS Code keeps its built-in chat extension disabled, in browser storage, until a GitHub "chat setup" completes; the workbench page served through the plugin proxy therefore loads `_pom/seed.js` first, which once per browser re-enables the extension and reloads. A person who later disables it keeps that choice. Without a gateway (older POM) the editor works and the chat simply has no POM models.

Known limit: VS Code marks its internal Ollama provider as deprecated and shows a one-time notice; the pinned code-server release supports it. Re-check this path when moving the pin to a newer release.

## POM integration

The plugin receives `workspace_root` from `host.configure`; if the POM has not configured a workspace, it uses a plugin-private `data/workspace`. `ui.upstream` publishes only the authenticated loopback proxy. POM mounts that proxy below `/api/ui/plugins/code_server/proxy/`, preserving HTTP and WebSocket requests required by VS Code.
