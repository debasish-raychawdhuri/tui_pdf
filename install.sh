#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Step 1: Install system dependencies
echo "==> Installing system dependencies..."
"$SCRIPT_DIR/install-deps.sh"

# Step 2: Ensure a Rust toolchain new enough for edition 2024 (rustc 1.85+)
echo ""
echo "==> Checking Rust toolchain..."

# Pick up an existing rustup install that isn't on PATH in this shell yet
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "  Rust not found; installing via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal
    . "$HOME/.cargo/env"
fi

MIN_RUST_MINOR=85
rustc_version="$(rustc --version | awk '{print $2}')"
rustc_minor="$(echo "$rustc_version" | cut -d. -f2)"
if [ "$(echo "$rustc_version" | cut -d. -f1)" -eq 1 ] && [ "$rustc_minor" -lt "$MIN_RUST_MINOR" ]; then
    echo "Error: rustc $rustc_version is too old; this project uses edition 2024 (needs 1.$MIN_RUST_MINOR+)." >&2
    if command -v rustup >/dev/null 2>&1; then
        echo "Run 'rustup update stable' and re-run this script." >&2
    else
        echo "Your Rust appears to be distro-packaged. Install the current stable toolchain" >&2
        echo "via rustup instead: https://rustup.rs" >&2
    fi
    exit 1
fi
echo "  Using rustc $rustc_version"

# Step 3: Build and install the binary
echo ""
echo "==> Building and installing tui-pdf..."
# --locked respects Cargo.lock; without it cargo install re-resolves to the
# latest semver-compatible patch versions, which has shipped rendering
# regressions (e.g. ratatui 0.30.1 / ratatui-image 10.0.8 collapsing pages to
# a single stripe). Keep installs reproducible with the committed lockfile.
cargo install --path "$SCRIPT_DIR" --locked --force

# Step 4: Install shell completions
echo ""
echo "==> Installing shell completions..."

# cargo install puts the binary in ~/.cargo/bin, which may not be on PATH yet
TUI_PDF="$(command -v tui-pdf || echo "$HOME/.cargo/bin/tui-pdf")"

install_bash_completions() {
    local dir="${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions"
    mkdir -p "$dir"
    "$TUI_PDF" --completions bash > "$dir/tui-pdf"
    echo "  Bash completions installed to $dir/tui-pdf"
}

install_fish_completions() {
    local dir="${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions"
    mkdir -p "$dir"
    "$TUI_PDF" --completions fish > "$dir/tui-pdf.fish"
    echo "  Fish completions installed to $dir/tui-pdf.fish"
}

install_zsh_completions() {
    local dir="${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions"
    mkdir -p "$dir"
    "$TUI_PDF" --completions zsh > "$dir/_tui-pdf"
    echo "  Zsh completions installed to $dir/_tui-pdf"
    echo "  (Make sure $dir is in your fpath)"
}

install_bash_completions
install_fish_completions
install_zsh_completions

echo ""
echo "Done! Restart your shell or source the completions to activate."
if ! command -v tui-pdf >/dev/null 2>&1; then
    echo "Note: \$HOME/.cargo/bin is not on your PATH; add it to run tui-pdf."
fi
