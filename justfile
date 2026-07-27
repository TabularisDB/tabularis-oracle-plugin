set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Build the plugin binary in debug mode.
build:
    cargo build

# Build for release (what the GitHub Actions workflow ships).
release:
    cargo build --release

# Run unit tests.
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

# ---------------------------------------------------------------------------
# Platform-specific install recipes (plugin-dir conventions per OS).
# ---------------------------------------------------------------------------

[linux]
dev-install: build
    mkdir -p ~/.local/share/tabularis/plugins/oracle
    cp target/debug/oracle-plugin ~/.local/share/tabularis/plugins/oracle/
    cp .tabularium ~/.local/share/tabularis/plugins/oracle/
    @echo "Installed to ~/.local/share/tabularis/plugins/oracle"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[macos]
dev-install: build
    mkdir -p "$HOME/Library/Application Support/com.debba.tabularis/plugins/oracle"
    cp target/debug/oracle-plugin "$HOME/Library/Application Support/com.debba.tabularis/plugins/oracle/"
    cp .tabularium "$HOME/Library/Application Support/com.debba.tabularis/plugins/oracle/"
    @echo "Installed to ~/Library/Application Support/com.debba.tabularis/plugins/oracle"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

# Each recipe line runs in a fresh shell, so this must be one logical command.
# Tabularis resolves its plugin dir via the `directories` crate
# (ProjectDirs "com"/"debba"/"tabularis"), which on Windows is
# %APPDATA%\debba\tabularis\data.
[windows]
dev-install: build
    $dest = Join-Path $env:APPDATA "debba\tabularis\data\plugins\oracle"; \
    New-Item -ItemType Directory -Force -Path $dest | Out-Null; \
    Copy-Item "target\debug\oracle-plugin.exe" $dest; \
    Copy-Item ".tabularium" $dest; \
    Write-Host "Installed to $dest"; \
    Write-Host "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[linux]
uninstall:
    rm -rf ~/.local/share/tabularis/plugins/oracle

[macos]
uninstall:
    rm -rf "$HOME/Library/Application Support/com.debba.tabularis/plugins/oracle"

[windows]
uninstall:
    $dest = Join-Path $env:APPDATA "debba\tabularis\data\plugins\oracle"; \
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
