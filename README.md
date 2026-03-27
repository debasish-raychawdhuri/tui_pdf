# tui-pdf

A fast, feature-rich PDF viewer for the terminal. Renders PDF pages as high-fidelity images using the Kitty graphics protocol, Sixel, or iTerm2, with vim-style keyboard navigation.

![Rust](https://img.shields.io/badge/rust-stable-orange)
![License](https://img.shields.io/badge/license-MIT-blue)

## Features

- **High-quality rendering** — PDF pages rendered at 192 DPI via MuPDF, displayed using your terminal's native image protocol (Kitty, Sixel, iTerm2, or halfblock fallback)
- **Smooth scrolling** — continuous vertical scrolling across pages with stripe-based rendering and visible page gap separators
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
- **Zotero integration** — browse your Zotero library and open PDFs directly (`tui-pdf --zotero` or `o` from within the viewer), view metadata (`m`), and copy BibTeX to clipboard (`c`)
- **Virtual document tabs** — switch between previously opened documents while preserving scroll and zoom state (`Tab`)
- **Named sessions** — save all open documents with scroll/zoom state to a named session (`S`), restore with `tui-pdf --session <name>`
- **Portable sessions** — Zotero PDF paths are stored as portable `zotero://` URIs, so sessions synced via cloud storage work across computers
- **reMarkable integration** — send the current PDF to a reMarkable tablet via USB (`R`) or to the reMarkable cloud via [rmapi](https://github.com/ddvk/rmapi) (`C`)
- **Shell completions** — tab completion for bash, fish, and zsh

## Requirements

- A terminal with image support: **Kitty** (recommended), iTerm2, any Sixel-capable terminal, or any terminal for halfblock fallback
- **Rust** toolchain (stable)
- System libraries: clang/libclang, chafa, freetype, fontconfig

## Installation

### Quick install (recommended)

The install script handles system dependencies, builds the binary, and sets up shell completions:

```bash
git clone https://github.com/debasish-raychawdhuri/tui_pdf.git
cd tui_pdf
./install.sh
```

Supports Debian/Ubuntu, Arch, Fedora/RHEL, and openSUSE.

### Manual install

Install system dependencies for your distro:

```bash
# Debian/Ubuntu
sudo apt install build-essential pkg-config libclang-dev libchafa-dev libfreetype6-dev libfontconfig1-dev

# Arch Linux
sudo pacman -S base-devel pkgconf clang chafa freetype2 fontconfig

# Fedora/RHEL
sudo dnf install gcc gcc-c++ make pkg-config clang-devel chafa-devel freetype-devel fontconfig-devel

# openSUSE
sudo zypper install gcc gcc-c++ make pkg-config clang-devel chafa-devel freetype2-devel fontconfig-devel
```

Then build and install:

```bash
cargo install --path .
```

### Shell completions

If you used `install.sh`, completions are already set up for your shell. To install them manually:

```bash
# Bash
tui-pdf --completions bash > ~/.local/share/bash-completion/completions/tui-pdf

# Fish
tui-pdf --completions fish > ~/.config/fish/completions/tui-pdf.fish

# Zsh (make sure the directory is in your fpath)
mkdir -p ~/.local/share/zsh/site-functions
tui-pdf --completions zsh > ~/.local/share/zsh/site-functions/_tui-pdf
```

Restart your shell or source the completion file to activate.

> **Note:** The first build takes a few minutes because MuPDF is compiled from source and statically linked.

## Usage

```bash
tui-pdf <path-to-pdf>

# Open multiple PDFs:
tui-pdf paper1.pdf paper2.pdf paper3.pdf

# Browse Zotero library:
tui-pdf --zotero

# One-time Zotero setup (point to your Zotero data directory):
tui-pdf --setup-zotero ~/Zotero

# Forward search (send from editor to a running instance):
tui-pdf --forward line:col:texfile path-to-pdf

# Restore a saved session:
tui-pdf --session mysession

# List saved sessions:
tui-pdf --list-sessions

# Move session storage to a custom directory (e.g. for cloud sync):
tui-pdf --move-sessions ~/MEGA/tui-pdf-sessions

# Generate shell completions:
tui-pdf --completions bash
tui-pdf --completions fish
tui-pdf --completions zsh
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
| `m` | Show Zotero metadata for current document |
| `c` (in metadata view) | Copy BibTeX to clipboard |
| `o` | Open Zotero browser |
| `O` | Open latest Zotero PDF |
| `s` | SyncTeX probe (numbered overlay for keyboard reverse search) |
| `d` | Document picker |
| `Tab` / `Shift+Tab` | Cycle between open documents |
| `R` | Send PDF to reMarkable via USB |
| `C` | Send PDF to reMarkable cloud (via rmapi) |
| `S` | Save session (prompts for name, or saves to current session) |
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

**Browse library:** Launch with `tui-pdf --zotero` or press `o` from within the viewer. The browser shows your collection hierarchy — navigate into collections with `Enter`, go back with `Backspace`, and type to filter by title/author/year. Select a paper and press `Enter` to open it. Press `m` on a paper to view its metadata.

**Metadata & BibTeX:** Press `m` in the viewer or the Zotero browser to see the title, authors, year, publication details, DOI, URL, and file path for the current document (looked up from the Zotero database). The metadata view also shows the generated BibTeX entry. Press `c` to copy the BibTeX to your clipboard (requires `xclip`, `xsel`, or `wl-copy`).

**Virtual tabs:** Documents you open are remembered with their scroll position and zoom level. Press `Tab` to cycle between them. Documents are reopened on switch rather than kept in memory, so there is no overhead.

### Sessions

Save your workspace with `S` — all open documents, scroll positions, and zoom levels are persisted to a named session file. Restore with `tui-pdf --session <name>`. List saved sessions with `tui-pdf --list-sessions`.

**Custom storage:** Move session files to a cloud-synced directory with `tui-pdf --move-sessions <dir>`. Existing sessions are moved automatically.

**Cross-computer sync:** Zotero PDF paths are stored as portable `zotero://KEY/file.pdf` URIs, so sessions work across computers as long as each machine has `--setup-zotero` configured. Non-Zotero paths remain absolute.

## Library usage

`tui_pdf` is also a library. You can embed a PDF viewer widget in your own ratatui application:

```rust
use tui_pdf::{Document, PdfViewState, PdfWidget, StatusBar};
use ratatui_image::picker::Picker;

let document = Document::open("document.pdf")?;
let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
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
