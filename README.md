# tui-pdf

A fast, feature-rich PDF and Markdown viewer for the terminal. Renders PDF pages as high-fidelity images using the Kitty graphics protocol, Sixel, or iTerm2, with vim-style keyboard navigation.

![Rust](https://img.shields.io/badge/rust-stable-orange)
![License](https://img.shields.io/badge/license-MIT-blue)

## Features

- **High-quality rendering** — PDF pages rendered at 192 DPI via MuPDF, displayed using your terminal's native image protocol (Kitty, Sixel, iTerm2, or halfblock fallback)
- **Markdown with math** — open `.md`/`.markdown` files and view them rendered natively (no browser): GitHub-flavored Markdown plus LaTeX math in `$…$`/`$$…$$` (KaTeX-compatible), laid out on one continuous page. Forward/reverse search and auto-reload work just like LaTeX (see [Markdown](#markdown))
- **Web pages** — pass an `http(s)://` URL and tui-pdf renders the full page as a single scrollable image via headless Chrome (`tui-pdf https://example.com`)
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
- **reMarkable integration** — send the current PDF to a reMarkable tablet over SSH in Developer Mode, opened at your current page (`R`; falls back to the USB web interface for non-developer devices), or to the reMarkable cloud via [rmapi](https://github.com/ddvk/rmapi) (`C`). Bidirectionally sync every saved session's documents and reading positions with the tablet via `tui-pdf --sync-sessions`, which also pulls your handwritten annotations back and draws them over the PDF (`a` to toggle)
- **Shell completions** — tab completion for bash, fish, and zsh

## Requirements

- A terminal with image support: **Kitty** (recommended), iTerm2, any Sixel-capable terminal, or any terminal for halfblock fallback
- **Rust** toolchain (stable)
- System libraries: clang/libclang, chafa, freetype, fontconfig

Optional, per feature:

- **synctex** CLI and `$NVIM` socket — SyncTeX reverse search into neovim
- **Chrome/Chromium** — rendering web pages from a URL
- **xclip**, **xsel**, or **wl-copy** — copying BibTeX to the clipboard
- **ssh**/**scp** — sending to / syncing with a reMarkable in Developer Mode
- **[rmapi](https://github.com/ddvk/rmapi)** — sending to the reMarkable cloud (`C`)

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

# Open a Markdown file (rendered to a PDF, with LaTeX math):
tui-pdf notes.md

# Render a web page as a scrollable image (requires Chrome/Chromium):
tui-pdf https://example.com

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

# Sync sessions to/from a connected reMarkable (all, or only the named ones):
tui-pdf --sync-sessions
tui-pdf --sync-sessions mysession othersession

# Sync over WiFi instead of USB (needs SSH-over-WLAN enabled on the tablet):
tui-pdf --sync-sessions --ip 192.168.1.42

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
| `a` | Toggle reMarkable annotation overlay (when the doc has pulled scribbles) |
| `m` | Show Zotero metadata for current document |
| `c` (in metadata view) | Copy BibTeX to clipboard |
| `u` (in metadata view) | Open the document's URL (or DOI) in a browser |
| `o` | Open Zotero browser |
| `O` | Open latest Zotero PDF |
| `s` | SyncTeX probe (numbered overlay for keyboard reverse search) |
| `d` | Document picker |
| `Tab` / `Shift+Tab` | Cycle between open documents |
| `R` | Send PDF to reMarkable (SSH/Developer Mode, USB web interface fallback) |
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

### Markdown

Open a Markdown file (`.md`/`.markdown`) and tui-pdf renders it to a PDF on the fly — natively in Rust, no browser. It handles GitHub-flavored Markdown (headings, lists, task lists, tables, syntax-highlighted code blocks, blockquotes, images) and **LaTeX math** in `$…$` / `$$…$$`, rendered with a KaTeX-compatible engine. The whole document is laid out on a single continuous page sized to its content, so scrolling never breaks mid-section.

Everything that works for PDFs works here, because it *is* a PDF: scroll, zoom, full-text search, and invert. The SyncTeX workflow works too, built from the Markdown source positions (no `synctex` file needed):

- **Forward search** and **reverse search** (Ctrl+Click / `s`) jump between the source line and the rendered position, exactly like LaTeX.
- **Auto-reload:** editing the `.md` and saving re-renders the view, preserving scroll position.

**neovim:** for source highlighting (including math), install the parsers once with `:TSInstall markdown markdown_inline latex` — `markdown_inline` injects the `latex` parser into math spans. Forward search uses `tui-pdf --forward "<line>:1:<file.md>" "<file.md>"`; open the viewer with `$NVIM` set so Ctrl+Click reverse-jumps back to the source.

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

**Cross-computer sync:** Zotero PDF paths are stored as portable `zotero://KEY/file.pdf` URIs, so sessions work across computers as long as each machine has `--setup-zotero` configured. Paths under your home directory are stored home-relative (`~/…`); other paths remain absolute.

### reMarkable integration

tui-pdf talks to a reMarkable in **Developer Mode** directly over SSH — no cloud round-trip. Enable Developer Mode on the tablet, connect it (USB gives it the default IP `10.11.99.1`), and install your SSH key once:

```bash
ssh-copy-id root@10.11.99.1
```

**Send a single PDF (`R`):** the current document is uploaded into a `tui-pdf` folder on the tablet, named by its Zotero title (when known) and opened at your current page. Documents are de-duplicated by name, so re-sending is a no-op. If SSH isn't reachable (device not in Developer Mode), it falls back to the USB web interface. `C` sends to the reMarkable cloud instead, via [rmapi](https://github.com/ddvk/rmapi).

**Sync whole sessions (`tui-pdf --sync-sessions`):** for each saved session (or only the ones you name), every document is pushed to the tablet and reading positions are reconciled **latest-wins** in both directions — so pages you turned on the reMarkable flow back into your session, and pages you moved on the computer flow to the tablet. Markdown documents are skipped (the tablet can't open them natively). The tablet's UI refreshes once at the end.

**Pull scribbles back:** `--sync-sessions` also pulls handwritten annotations *down* from the tablet (the reMarkable is authoritative for scribbles — pulling is read-only on the device). The device stores strokes as separate v6 `.rm` files, not inside the PDF; tui-pdf parses them, converts them to vector overlays, and stores them under `~/.config/tui-pdf/annotations/`. When you open the document, your handwriting is drawn on top of the PDF. Toggle the overlay with `a`. Pages you *inserted* on the tablet (blank pages with no source page) aren't shown.

Sync defaults to the USB address (`10.11.99.1`). To sync over WiFi, enable SSH-over-WLAN on the tablet (`rm-ssh-over-wlan on`) and pass its WiFi address with `--ip`:

```bash
tui-pdf --sync-sessions --ip 192.168.1.42
```

`--ip` overrides `remarkable_host` from the config for that run; set `remarkable_host` in the config to make WiFi the default.

> The reMarkable's `xochitl` service is never stopped mid-sync — sidecar files are written live and the service is restarted only once at the end, which briefly blanks the screen.

## Configuration

Settings live in `~/.config/tui-pdf/config.toml` (respects `$XDG_CONFIG_HOME`). It is written automatically by `--setup-zotero` and `--move-sessions`, but you can edit it by hand:

```toml
# Zotero data directory (set via `tui-pdf --setup-zotero <dir>`)
zotero_dir = "/home/you/Zotero"

# Where named sessions are stored (set via `tui-pdf --move-sessions <dir>`)
sessions_dir = "/home/you/MEGA/tui-pdf-sessions"

# SSH host of the reMarkable (default: 10.11.99.1, the USB address)
remarkable_host = "10.11.99.1"
```

## Library usage

`tui_pdf` is also a library. You can embed a PDF viewer widget in your own ratatui application:

```rust
use tui_pdf::{ContentSource, Document, PdfViewState, PdfWidget, StatusBar};
use ratatui_image::picker::Picker;

let source = ContentSource::Pdf(Document::open("document.pdf")?);
let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
let mut state = PdfViewState::new(source.page_count(), picker);
state.initial_render(&source)?;

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
