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
- **Zoom** — adjustable zoom level with immediate re-render (`+`/`-`), fit-to-width (`w`)
- **Go to page** — jump directly to any page number (`g`)
- **Background pre-rendering** — pages are rendered and cached in the background during idle time, so scrolling through large documents is instant
- **Efficient caching** — stripe PNG cache with 100 MB LRU eviction
- **Auto-reload** — detects file changes and reloads automatically, preserving scroll position (great for LaTeX workflows)
- **SyncTeX reverse search** — Ctrl+Click on the PDF or press `s` for a keyboard-driven numbered probe overlay, then type a number to jump to the corresponding source line in neovim (requires `synctex` CLI and `$NVIM` socket)
- **SyncTeX forward search** — integrates with texlab LSP to scroll the PDF to the source position (`tui-pdf --forward line:col:file doc.pdf`)
- **Mouse wheel scrolling** — scroll through the document with the mouse wheel
- **Fit to width** — resize zoom to fit the page width to the terminal (`w`)
- **Zotero integration** — browse your Zotero library and open PDFs directly (`tui-pdf --zotero` or `o` from within the viewer)
- **Virtual document tabs** — switch between previously opened documents while preserving scroll and zoom state (`Tab`)

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

# Browse Zotero library:
tui-pdf --zotero

# One-time Zotero setup (point to your Zotero data directory):
tui-pdf --setup-zotero ~/Zotero

# Forward search (send from editor to a running instance):
tui-pdf --forward line:col:texfile path-to-pdf
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
| `w` | Fit to width |
| `i` | Toggle color inversion |
| `o` | Open Zotero browser |
| `s` | SyncTeX probe (numbered overlay for keyboard reverse search) |
| `Tab` | Switch between open documents |
| `x` | Close current document |
| `q` / `Esc` | Quit |
| Mouse wheel | Scroll up/down |
| Ctrl+Click | SyncTeX reverse search (jump to source in neovim) |

### Search

Press `/` to open the search prompt, type your query, and press `Enter`. The viewer searches incrementally (20 pages per frame tick) starting from the current page, so results appear almost instantly. Use `n` and `p` to jump between matches. The current match is highlighted in orange, other matches in yellow. Press `Esc` to clear the search.

### Links

Press `l` to activate link mode on the current page. Internal links are highlighted in blue, with the selected link in yellow. Use `j`/`k` to select a link, `Enter` to follow it. Press `b` at any time to jump back to where you were before following a link.

### SyncTeX integration

tui-pdf supports bidirectional SyncTeX for LaTeX editing workflows.

**Reverse search (PDF → source):** Ctrl+Click anywhere on the PDF, or press `s` for keyboard-driven reverse search. The `s` key probes synctex at a grid of points across the visible area, finds actual source locations, and overlays numbered badges directly into the page image. Type a number and press `Enter` to jump to that source line in neovim; press `Esc` to cancel. If `synctex` is installed and your PDF was compiled with `-synctex=1`, it jumps to the corresponding source line in a running neovim instance (via the `$NVIM` socket).

**Forward search (source → PDF):** Configure your LSP (e.g. texlab) to use `tui-pdf --forward "%l:1:%f" "%p"` as the forward search command. When triggered from your editor, the running tui-pdf instance scrolls to the corresponding PDF position.

**Auto-reload:** When the PDF file changes on disk (e.g. after recompiling LaTeX), tui-pdf automatically reloads it while preserving your scroll position.

**Requirements:** `synctex` CLI tool (usually bundled with TeX distributions), PDF compiled with `pdflatex -synctex=1`, and `$NVIM` environment variable set for reverse search to jump to neovim.

### Zotero integration

tui-pdf can browse your local Zotero library and open saved PDFs directly.

**One-time setup:** Point tui-pdf to your Zotero data directory:
```bash
tui-pdf --setup-zotero ~/Zotero
```

**Browse library:** Launch with `tui-pdf --zotero` or press `o` from within the viewer. The browser shows your collection hierarchy — navigate into collections with `Enter`, go back with `Backspace`, and type to filter by title/author/year. Select a paper and press `Enter` to open it.

**Virtual tabs:** Documents you open are remembered with their scroll position and zoom level. Press `Tab` to cycle between them. Documents are reopened on switch rather than kept in memory, so there is no overhead.

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
