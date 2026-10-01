# POM Plugin - Code-server

Runs the official [coder/code-server](https://github.com/coder/code-server) web IDE in the POM admin interface, with its workspace set to the POM shared workspace. Open **Code-server** from the POM sidebar to use the VS Code editor and integrated terminal on the POM node.

The code-server release is installed in the plugin package build and embedded in its native library. POM nodes need no first-start internet access. Build/package scripts verify the upstream GitHub SHA-256 digest before embedding the release. User settings and extensions persist separately from the replaceable runtime.

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
