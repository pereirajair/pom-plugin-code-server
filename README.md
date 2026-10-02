# POM Plugin - Code-server

Runs the official [coder/code-server](https://github.com/coder/code-server) web IDE in the POM admin interface, with its workspace set to the POM shared workspace. Open **Code-server** from the POM sidebar to use the VS Code editor and integrated terminal on the POM node.

The plugin package pins one official code-server release (URL, GitHub-published SHA-256, version) but does not contain it, so it stays a few megabytes. On its first activation the node downloads that release once, verifies the digest and unpacks it; the editor screen shows the progress. User settings and extensions persist separately from the replaceable runtime.

The editor's built-in chat uses the POM's models with no GitHub account: the plugin configures them automatically from the gateway the POM shares with plugins (see [docs/README.md](docs/README.md#pom-models-in-the-editor-chat)).

The menu and route are admin-only: code-server can execute commands as the POM process and access node files. The service binds to loopback with code-server authentication disabled; browser traffic is gated by POM's same-origin, admin-authorized plugin proxy and a private per-launch upstream token.

See [docs/README.md](docs/README.md) for the runtime and security details.

## Build

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
node --test tests/unit/*.test.mjs

# Run on the target platform; the release workflow builds each platform natively.
scripts/package.sh --platform linux-x86_64 --version 0.1.0 --code-server-version latest
```

Supported package platforms are Linux x86_64, macOS arm64 and Windows x86_64. The manual workflow publishes public GitHub release assets and per-platform manifests for POM's **Add from GitHub** flow.
