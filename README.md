# tui-pdf

A fast, feature-rich PDF viewer for the terminal. Renders PDF pages as high-fidelity images using the Kitty graphics protocol, Sixel, or iTerm2, with vim-style keyboard navigation.

![Rust](https://img.shields.io/badge/rust-stable-orange)
![License](https://img.shields.io/badge/license-MIT-blue)

## Features

- **High-quality rendering** — PDF pages rendered at 192 DPI via MuPDF, displayed using your terminal's native image protocol (Kitty, Sixel, iTerm2, or halfblock fallback)
- **Smooth scrolling** — continuous vertical scrolling across pages with stripe-based rendering
- **Text search** — incremental search across the entire document with highlighted matches (`/` to search, `n`/`p` to navigate results)
- **Table of contents** — side panel showing document outline with jump-to-page (`t`)
- **Link navigation** — follow internal PDF links and navigate back (`l` to enter link mode, `b` to go back)
- **Zoom** — adjustable zoom level with immediate re-render (`+`/`-`)
- **Go to page** — jump directly to any page number (`g`)
- **Background pre-rendering** — pages are rendered and cached in the background during idle time, so scrolling through large documents is instant
- **Efficient caching** — stripe PNG cache with 1 GB LRU eviction; a 1000-page PDF uses ~500 MB of cache

## Requirements

- A terminal with image support: **Kitty** (recommended), iTerm2, any Sixel-capable terminal, or any terminal for halfblock fallback
- **Rust** toolchain (stable)
- **clang** / **libclang** and a C compiler (required to build MuPDF from source)

### Installing build dependencies

**Debian/Ubuntu:**
```bash
sudo apt install clang libclang-dev build-essential
```

**Fedora:**
```bash
sudo dnf install clang clang-devel gcc
```

**macOS:**
```bash
xcode-select --install
```

**Arch Linux:**
```bash
sudo pacman -S clang
```

## Installation

### From source

```bash
git clone https://github.com/debasish-raychawdhuri/tui_pdf.git
cd tui_pdf
cargo install --path .
```

### Build and run directly

```bash
cargo build --release
./target/release/tui-pdf document.pdf
```

> **Note:** The first build takes a few minutes because MuPDF is compiled from source and statically linked — no runtime dependencies needed.

## Usage

```bash
tui-pdf <path-to-pdf>
```

### Keybindings

| Key | Action |
|-----|--------|
| `j` / `Down` | Scroll down |
| `k` / `Up` | Scroll up |
| `n` / `Right` / `PageDown` | Next page |
| `p` / `Left` / `PageUp` | Previous page |
| `Home` | First page |
| `End` | Last page |
| `g` | Go to page number |
| `+` / `=` | Zoom in |
| `-` | Zoom out |
| `/` | Search text |
| `n` (during search) | Next match |
| `p` (during search) | Previous match |
| `Esc` (during search) | Clear search |
| `t` | Toggle table of contents |
| `l` | Enter link mode |
| `Enter` (in link mode) | Follow selected link |
| `b` | Go back (after following a link) |
| `q` / `Esc` | Quit |

### Search

Press `/` to open the search prompt, type your query, and press `Enter`. The viewer searches incrementally (20 pages per frame tick) starting from the current page, so results appear almost instantly. Use `n` and `p` to jump between matches. The current match is highlighted in orange, other matches in yellow. Press `Esc` to clear the search.

### Links

Press `l` to activate link mode on the current page. Internal links are highlighted in blue, with the selected link in yellow. Use `j`/`k` to select a link, `Enter` to follow it. Press `b` at any time to jump back to where you were before following a link.

## Library usage

`tui_pdf` is also a library. You can embed a PDF viewer widget in your own ratatui application:

```rust
use tui_pdf::{Document, PdfViewState, PdfWidget, StatusBar};
use ratatui_image::picker::Picker;

let document = Document::open("document.pdf")?;
let picker = Picker::from_query_stdio()?;
let mut state = PdfViewState::new(document.page_count(), picker);
state.initial_render(&document)?;

// In your ratatui draw loop:
frame.render_stateful_widget(PdfWidget, area, &mut state);
```

## Architecture

- **MuPDF** renders PDF pages to pixel buffers (statically linked, no runtime dependencies)
- Pages are split into horizontal **stripes** (one terminal row each) and cached as compressed PNGs
- Display **protocols** (Kitty/Sixel/iTerm2) are built on-demand for a window around the viewport
- Background **pre-rendering** fills the PNG cache during idle time using a spiral pattern outward from the current page
- Search highlights are applied as **per-stripe overlays**, so navigating between matches only rebuilds 1-2 stripes instead of entire pages

## License

MIT
