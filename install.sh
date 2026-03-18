#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Step 1: Install system dependencies
echo "==> Installing system dependencies..."
"$SCRIPT_DIR/install-deps.sh"

# Step 2: Build and install the binary
echo ""
echo "==> Building and installing tui-pdf..."
cargo install --path "$SCRIPT_DIR"

# Step 3: Install shell completions
echo ""
echo "==> Installing shell completions..."

install_bash_completions() {
    local dir="${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions"
    mkdir -p "$dir"
    tui-pdf --completions bash > "$dir/tui-pdf"
    echo "  Bash completions installed to $dir/tui-pdf"
}

install_fish_completions() {
    local dir="${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions"
    mkdir -p "$dir"
    tui-pdf --completions fish > "$dir/tui-pdf.fish"
    echo "  Fish completions installed to $dir/tui-pdf.fish"
}

install_zsh_completions() {
    local dir="${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions"
    mkdir -p "$dir"
    tui-pdf --completions zsh > "$dir/_tui-pdf"
    echo "  Zsh completions installed to $dir/_tui-pdf"
    echo "  (Make sure $dir is in your fpath)"
}

install_bash_completions
install_fish_completions
install_zsh_completions

echo ""
echo "Done! Restart your shell or source the completions to activate."
