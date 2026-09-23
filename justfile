set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# ---------------------------------------------------------------------------
# Cross-platform recipes (only shell-agnostic tooling — cargo, pnpm).
# ---------------------------------------------------------------------------

# Build the plugin binary and the EXPLAIN parser bundle.
build: build-explain
    cargo build

# Build for release (what the GitHub Actions workflow ships).
release: build-explain
    cargo build --release

# Build the browser-safe Oracle plan parser (IIFE + npm package).
build-explain:
    pnpm --dir explain install --frozen-lockfile
    pnpm --dir explain build

# Typecheck and test the EXPLAIN parser package.
test-explain:
    pnpm --dir explain install --frozen-lockfile
    pnpm --dir explain typecheck
    pnpm --dir explain test

# Run unit tests (tests/live_db.rs skips itself without ORACLE_TEST_HOST).
test:
    cargo test

# Launch the local REPL that simulates Tabularis JSON-RPC calls over stdio.
repl:
    cargo run --bin test_plugin

# Run clippy with warnings denied.
lint:
    cargo clippy --all-targets -- -D warnings

# Format the codebase.
fmt:
    cargo fmt --all

# Start a disposable Oracle Free container for local testing
# (user: system, password: oracle, service: FREEPDB1).
demo-db:
    docker run -d --name tabularis-oracle-demo -p 1521:1521 -e ORACLE_PASSWORD=oracle gvenzl/oracle-free:slim

demo-db-stop:
    docker rm -f tabularis-oracle-demo

# Run the live JSON-RPC integration test against `just demo-db`. Pass the
# Instant Client directory unless it is already on the library path.
live-test client_lib_dir="":
    ORACLE_TEST_HOST=127.0.0.1 ORACLE_CLIENT_LIB_DIR="{{client_lib_dir}}" \
        cargo test --test live_db -- --test-threads=1

# ---------------------------------------------------------------------------
# Platform-specific recipes (file operations + plugin-dir conventions).
#
# Host source: tabularis/src-tauri/src/paths.rs::get_plugins_dir appends
# `plugins` to the app data dir, and plugins/layout.rs installs drivers under
# its `drivers/<id>` kind directory. The resulting roots are
# ${XDG_DATA_HOME:-$HOME/.local/share}/tabularis/plugins/drivers on Linux,
# $HOME/Library/Application Support/tabularis/plugins/drivers on macOS, and
# %APPDATA%\tabularis\plugins\drivers on Windows.
#
# The binary is taken from cargo's resolved target directory, so a shared
# CARGO_TARGET_DIR or build.target-dir config works too.
# ---------------------------------------------------------------------------

[linux]
dev-install: build
    #!/usr/bin/env bash
    set -euo pipefail
    target_dir=$(cargo metadata --format-version 1 --no-deps | grep -o '"target_directory":"[^"]*"' | cut -d'"' -f4)
    dest="${XDG_DATA_HOME:-$HOME/.local/share}/tabularis/plugins/drivers/oracle"
    mkdir -p "$dest/explain/dist"
    cp "$target_dir/debug/oracle-plugin" .tabularium "$dest/"
    cp explain/dist/index.iife.js "$dest/explain/dist/"
    echo "Installed to $dest"
    echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[macos]
dev-install: build
    #!/usr/bin/env bash
    set -euo pipefail
    target_dir=$(cargo metadata --format-version 1 --no-deps | grep -o '"target_directory":"[^"]*"' | cut -d'"' -f4)
    dest="$HOME/Library/Application Support/tabularis/plugins/drivers/oracle"
    mkdir -p "$dest/explain/dist"
    cp "$target_dir/debug/oracle-plugin" .tabularium "$dest/"
    cp explain/dist/index.iife.js "$dest/explain/dist/"
    echo "Installed to $dest"
    echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

# Each recipe line runs in a fresh shell, so this must be one logical command.
[windows]
dev-install: build
    $target = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory; \
    $dest = Join-Path $env:APPDATA "tabularis\plugins\drivers\oracle"; \
    New-Item -ItemType Directory -Force -Path (Join-Path $dest "explain\dist") | Out-Null; \
    Copy-Item (Join-Path $target "debug\oracle-plugin.exe") $dest; \
    Copy-Item ".tabularium" $dest; \
    Copy-Item "explain\dist\index.iife.js" (Join-Path $dest "explain\dist"); \
    Write-Host "Installed to $dest"; \
    Write-Host "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[linux]
uninstall:
    rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/tabularis/plugins/drivers/oracle"

[macos]
uninstall:
    rm -rf "$HOME/Library/Application Support/tabularis/plugins/drivers/oracle"

[windows]
uninstall:
    $dest = Join-Path $env:APPDATA "tabularis\plugins\drivers\oracle"; \
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
