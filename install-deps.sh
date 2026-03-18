#!/usr/bin/env bash
set -euo pipefail

# Detect package manager and install system dependencies for tui-pdf
#
# Required by:
#   clang/libclang  - bindgen (used by mupdf-sys)
#   chafa           - ratatui-image terminal graphics
#   freetype        - font rendering
#   fontconfig      - font discovery
#   sqlite          - rusqlite (bundled, but headers needed)

install_debian() {
    echo "Detected Debian/Ubuntu-based system"
    sudo apt-get update || echo "Warning: apt-get update had errors (likely third-party repos), continuing anyway..."
    sudo apt-get install -y \
        build-essential \
        pkg-config \
        libclang-dev \
        libchafa-dev \
        libfreetype6-dev \
        libfontconfig1-dev
}

install_arch() {
    echo "Detected Arch-based system"
    sudo pacman -Sy --needed --noconfirm \
        base-devel \
        pkgconf \
        clang \
        chafa \
        freetype2 \
        fontconfig
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
        freetype-devel \
        fontconfig-devel
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
        freetype2-devel \
        fontconfig-devel
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
                    echo "  - libchafa (development headers)"
                    echo "  - freetype (development headers)"
                    echo "  - fontconfig (development headers)"
                    exit 1
                    ;;
            esac
            ;;
    esac
else
    echo "Cannot detect distribution (/etc/os-release not found)"
    exit 1
fi

echo ""
echo "System dependencies installed. Now run:"
echo "  cargo install --path ."
