#!/usr/bin/env bash
set -euo pipefail

# Detect package manager and install system dependencies for tui-pdf
#
# Required by:
#   clang/libclang  - bindgen (used by mupdf-sys)
#   chafa >= 1.8.0  - ratatui-image halfblock rendering (linked via pkg-config)
#   glib-2.0        - required by chafa.pc (chafa's own pkg-config dependency)
#   freetype        - font rendering
#   fontconfig      - font discovery
#   sqlite          - rusqlite (bundled, but headers needed)
#   curl + CA certs - bootstrapping the Rust toolchain via rustup

install_debian() {
    echo "Detected Debian/Ubuntu-based system"
    sudo apt-get update || echo "Warning: apt-get update had errors (likely third-party repos), continuing anyway..."
    sudo apt-get install -y \
        build-essential \
        pkg-config \
        libclang-dev \
        libchafa-dev \
        libglib2.0-dev \
        libfreetype6-dev \
        libfontconfig1-dev \
        curl \
        ca-certificates
}

install_arch() {
    echo "Detected Arch-based system"
    sudo pacman -Sy --needed --noconfirm \
        base-devel \
        pkgconf \
        clang \
        chafa \
        glib2 \
        freetype2 \
        fontconfig \
        curl \
        ca-certificates
}

install_fedora() {
    echo "Detected Fedora/RHEL-based system"
    sudo dnf install -y \
        gcc \
        gcc-c++ \
        make \
        pkg-config \
        clang-devel \
        chafa-devel \
        glib2-devel \
        freetype-devel \
        fontconfig-devel \
        curl \
        ca-certificates
}

install_suse() {
    echo "Detected openSUSE-based system"
    sudo zypper install -y \
        gcc \
        gcc-c++ \
        make \
        pkg-config \
        clang-devel \
        chafa-devel \
        glib2-devel \
        freetype2-devel \
        fontconfig-devel \
        curl \
        ca-certificates
}

if [ -f /etc/os-release ]; then
    . /etc/os-release
    case "$ID" in
        debian|ubuntu|linuxmint|pop|elementary|zorin|neon)
            install_debian ;;
        arch|manjaro|endeavouros|garuda)
            install_arch ;;
        fedora|rhel|centos|rocky|alma|nobara)
            install_fedora ;;
        opensuse*|sles)
            install_suse ;;
        *)
            # Try ID_LIKE as fallback
            case "${ID_LIKE:-}" in
                *debian*|*ubuntu*) install_debian ;;
                *arch*)            install_arch ;;
                *fedora*|*rhel*)   install_fedora ;;
                *suse*)            install_suse ;;
                *)
                    echo "Unsupported distribution: $ID"
                    echo "Please install these packages manually:"
                    echo "  - C/C++ compiler and build tools"
                    echo "  - pkg-config"
                    echo "  - clang / libclang (for bindgen)"
                    echo "  - libchafa >= 1.8.0 (development headers)"
                    echo "  - glib-2.0 (development headers; required by chafa.pc)"
                    echo "  - freetype (development headers)"
                    echo "  - fontconfig (development headers)"
                    echo "  - curl and CA certificates (to install rustup)"
                    exit 1
                    ;;
            esac
            ;;
    esac
else
    echo "Cannot detect distribution (/etc/os-release not found)"
    exit 1
fi

# ratatui-image links libchafa (halfblocks rendering) and its build.rs
# requires chafa >= 1.8.0 via pkg-config. chafa.pc in turn Requires glib-2.0,
# so the glib dev package must be present too. Verify the exact probe the
# build runs so a broken setup fails here with a clear message, not mid-build.
echo ""
echo "==> Verifying chafa..."
if ! pkg-config --libs --cflags chafa 'chafa >= 1.8.0' > /dev/null; then
    found="$(pkg-config --modversion chafa 2>/dev/null || echo 'not found')"
    echo "Error: ratatui-image needs chafa >= 1.8.0 (pkg-config found: $found)." >&2
    echo "Above is pkg-config's reason. Common causes:" >&2
    echo "  - chafa dev package missing or older than 1.8.0" >&2
    echo "    (upgrade it, or build from https://hpjansson.org/chafa/download/)" >&2
    echo "  - glib-2.0 dev package missing (chafa.pc requires it)" >&2
    echo "  - chafa.pc installed outside pkg-config's search path (set PKG_CONFIG_PATH)" >&2
    exit 1
fi
echo "  chafa $(pkg-config --modversion chafa) OK"

echo ""
echo "System dependencies installed. Now run:"
echo "  cargo install --path ."
