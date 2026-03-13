use std::collections::HashSet;
use std::fs;
use std::io::{self, stdout, BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, StatefulWidget, Widget};
use ratatui::Terminal;
use ratatui_image::picker::Picker;


use tui_pdf::{
    Document, LinkState, PdfViewState, PdfWidget, SearchState, StatusBar, TocState, TocWidget,
    ZoteroLibrary, latest_pdf, load_config, load_library, save_config,
    send_forward, socket_path, synctex_edit, synctex_view, jump_to_neovim,
};

enum AppAction {
    Quit,
    OpenZotero,
    SwitchDoc(usize),
    CloseDoc,
    OpenLatest,
}

struct ProbeCell {
    number: usize,
    page: usize,
    pdf_x: f32,
    pdf_y: f32,
    file: String,
    line: usize,
}

fn compute_probe_grid(
    pdf_state: &PdfViewState,
    pdf_path: &Path,
    area: ratatui::layout::Rect,
) -> Vec<ProbeCell> {
    let cell_h: u16 = 5;
    let cols: u16 = 4;
    let cell_w = area.width / cols;
    if cell_w == 0 || cell_h == 0 {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut cells = Vec::new();
    let mut number = 1;
    let rows = area.height / cell_h;

    for r in 0..rows {
        for c in 0..cols {
            let center_row = area.y + r * cell_h + cell_h / 2;
            let center_col = area.x + c * cell_w + cell_w / 2;
            if let Some((page, pdf_x, pdf_y)) = pdf_state.terminal_to_pdf(center_row, center_col) {
                if let Some(result) = synctex_edit(pdf_path, page + 1, pdf_x, pdf_y) {
                    let key = (result.file.clone(), result.line);
                    if seen.insert(key) {
                        cells.push(ProbeCell {
                            number,
                            page,
                            pdf_x,
                            pdf_y,
                            file: result.file,
                            line: result.line,
                        });
                        number += 1;
                    }
                }
            }
        }
    }
    cells
}

struct OpenDoc {
    path: String,
    scroll: usize,
    zoom: f32,
}

fn main() -> io::Result<()> {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}\n\nBacktrace:\n{}", std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write("/tmp/tui-pdf-panic.log", &msg);
    }));

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: tui-pdf [--forward line:col:file] [--setup-zotero <dir>] [--zotero] <pdf>...");
        std::process::exit(1);
    }

    // Handle --forward: send command to running instance and exit
    if args.len() >= 4 && args[1] == "--forward" {
        let pdf_path = std::path::Path::new(&args[3]);
        let sock = socket_path(pdf_path);
        let message = format!("forward:{}", args[2]);
        if send_forward(&sock, &message) {
            std::process::exit(0);
        } else {
            eprintln!("No running tui-pdf instance for this PDF");
            std::process::exit(1);
        }
    }

    // Handle --setup-zotero: save Zotero directory to config
    if args.len() >= 3 && args[1] == "--setup-zotero" {
        let dir = &args[2];
        let path = std::path::Path::new(dir);
        if !path.join("zotero.sqlite").exists() {
            eprintln!("Error: {}/zotero.sqlite not found", dir);
            std::process::exit(1);
        }
        let mut config = load_config();
        config.zotero_dir = Some(dir.to_string());
        save_config(&config).unwrap_or_else(|e| {
            eprintln!("Failed to save config: {e}");
            std::process::exit(1);
        });
        eprintln!("Zotero directory saved. You can now use: tui-pdf --zotero");
        std::process::exit(0);
    }

    // Handle --zotero: browse Zotero library and open selected PDF
    if args.len() >= 2 && args[1] == "--zotero" {
        let config = load_config();
        let zotero_dir = config.zotero_dir.unwrap_or_else(|| {
            eprintln!("No Zotero directory configured. Run: tui-pdf --setup-zotero <dir>");
            std::process::exit(1);
        });
        let library = load_library(std::path::Path::new(&zotero_dir)).unwrap_or_else(|e| {
            eprintln!("Failed to load Zotero library: {e}");
            std::process::exit(1);
        });
        if library.entries.is_empty() {
            eprintln!("No PDF entries found in Zotero library.");
            std::process::exit(1);
        }
        match run_zotero_browser(&library) {
            Ok(Some(pdf_path)) => {
                let s = pdf_path.to_string_lossy().to_string();
                return open_viewer(&[&s]);
            }
            Ok(None) => std::process::exit(0),
            Err(e) => {
                eprintln!("Browser error: {e}");
                std::process::exit(1);
            }
        }
    }

    // Collect all remaining args as PDF paths
    let pdf_paths: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    open_viewer(&pdf_paths)
}

fn open_viewer(pdf_paths: &[&str]) -> io::Result<()> {
    let mut open_docs: Vec<OpenDoc> = pdf_paths.iter().map(|p| OpenDoc {
        path: p.to_string(),
        scroll: 0,
        zoom: 1.0,
    }).collect();
    let mut current_idx: usize = 0;
    let mut current_path = open_docs[0].path.clone();
    let mut inverted = false;
    let zotero_dir: Option<String> = load_config().zotero_dir;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        // Save current doc state if we have one open
        if let Some(doc) = open_docs.get_mut(current_idx) {
            // scroll/zoom already saved on switch
            let _ = doc;
        }

        // Find or create entry for current path
        let existing = open_docs.iter().position(|d| d.path == current_path);
        current_idx = match existing {
            Some(i) => i,
            None => {
                open_docs.push(OpenDoc {
                    path: current_path.clone(),
                    scroll: 0,
                    zoom: 1.0,
                });
                open_docs.len() - 1
            }
        };
        let saved_scroll = open_docs[current_idx].scroll;
        let saved_zoom = open_docs[current_idx].zoom;

        let mut document = match Document::open(&current_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Failed to open PDF: {e}");
                break;
            }
        };

        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16)));
        picker.set_background_color([0, 0, 0, 255]);

        let mut pdf_state = PdfViewState::new(document.page_count(), picker);
        pdf_state.zoom = saved_zoom;
        if inverted { pdf_state.toggle_invert(&document); }
        let _ = pdf_state.initial_render(&document);
        pdf_state.global_scroll = saved_scroll;

        let outlines = document.outlines().unwrap_or_default();
        let mut toc_state = TocState::new(&outlines);
        let mut link_state = LinkState::new();
        let mut search_state = SearchState::new();
        let mut goto_input: Option<String> = None;
        let mut search_input: Option<String> = None;

        let sock = socket_path(document.path());
        let _ = fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).ok();
        if let Some(ref l) = listener {
            l.set_nonblocking(true).ok();
        }

        let result = run_app(
            &mut terminal,
            &mut document,
            &mut pdf_state,
            &mut toc_state,
            &mut link_state,
            &mut search_state,
            &mut goto_input,
            &mut search_input,
            listener.as_ref(),
            &open_docs,
            current_idx,
        );

        let _ = fs::remove_file(&sock);

        // Save state before switching
        open_docs[current_idx].scroll = pdf_state.global_scroll;
        open_docs[current_idx].zoom = pdf_state.zoom;
        inverted = pdf_state.inverted();

        match result {
            Ok(AppAction::Quit) => break,
            Ok(AppAction::OpenZotero) => {
                if let Some(ref dir) = zotero_dir {
                    // Reload library fresh from DB each time
                    if let Ok(lib) = load_library(std::path::Path::new(dir)) {
                        let _ = stdout().execute(DisableMouseCapture);
                        let _ = stdout().execute(LeaveAlternateScreen);
                        let _ = disable_raw_mode();

                        match run_zotero_browser(&lib) {
                            Ok(Some(path)) => {
                                current_path = path.to_string_lossy().to_string();
                            }
                            _ => {}
                        }

                        enable_raw_mode()?;
                        stdout().execute(EnterAlternateScreen)?;
                        stdout().execute(EnableMouseCapture)?;
                    }
                }
            }
            Ok(AppAction::SwitchDoc(idx)) => {
                if idx < open_docs.len() {
                    current_path = open_docs[idx].path.clone();
                }
            }
            Ok(AppAction::CloseDoc) => {
                if open_docs.len() <= 1 {
                    break; // last doc, quit
                }
                open_docs.remove(current_idx);
                let switch_to = if current_idx >= open_docs.len() {
                    open_docs.len() - 1
                } else {
                    current_idx
                };
                current_path = open_docs[switch_to].path.clone();
                current_idx = switch_to;
            }
            Ok(AppAction::OpenLatest) => {
                if let Some(ref dir) = zotero_dir {
                    if let Some(path) = latest_pdf(std::path::Path::new(dir)) {
                        current_path = path.to_string_lossy().to_string();
                    }
                }
            }
            Err(e) => {
                let _ = stdout().execute(DisableMouseCapture);
                let _ = disable_raw_mode();
                let _ = stdout().execute(LeaveAlternateScreen);
                return Err(e);
            }
        }
    }

    let _ = stdout().execute(DisableMouseCapture);
    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);
    Ok(())
}

struct AppState<'a> {
    terminal: &'a mut Terminal<CrosstermBackend<io::Stdout>>,
    document: &'a mut Document,
    pdf_state: &'a mut PdfViewState,
    toc_state: &'a mut TocState,
    link_state: &'a mut LinkState,
    search_state: &'a mut SearchState,
    goto_input: &'a mut Option<String>,
    search_input: &'a mut Option<String>,
    listener: Option<&'a UnixListener>,
    open_docs: &'a [OpenDoc],
    current_idx: usize,

    last_mtime: Option<SystemTime>,
    last_mtime_check: Instant,
    status_message: Option<(String, Instant)>,
    doc_picker: Option<usize>,
    synctex_probe: Option<String>,
    last_probe_grid: Vec<ProbeCell>,
}

impl<'a> AppState<'a> {
    fn new(
        terminal: &'a mut Terminal<CrosstermBackend<io::Stdout>>,
        document: &'a mut Document,
        pdf_state: &'a mut PdfViewState,
        toc_state: &'a mut TocState,
        link_state: &'a mut LinkState,
        search_state: &'a mut SearchState,
        goto_input: &'a mut Option<String>,
        search_input: &'a mut Option<String>,
        listener: Option<&'a UnixListener>,
        open_docs: &'a [OpenDoc],
        current_idx: usize,
    ) -> Self {
        let last_mtime = fs::metadata(document.path())
            .and_then(|m| m.modified())
            .ok();
        Self {
            terminal, document, pdf_state, toc_state, link_state,
            search_state, goto_input, search_input, listener,
            open_docs, current_idx,
            last_mtime,
            last_mtime_check: Instant::now(),
            status_message: None,
            doc_picker: None,
            synctex_probe: None,
            last_probe_grid: Vec::new(),
        }
    }

    fn set_status(&mut self, msg: String) {
        self.status_message = Some((msg, Instant::now()));
    }

    fn run(&mut self) -> io::Result<AppAction> {
        loop {
            self.progress_search();
            self.expire_status_message();
            self.check_forward_search();
            self.pdf_state.ensure_visible_rendered(self.document);
            self.draw()?;

            let busy = self.search_state.searching
                || !self.pdf_state.prerender_done()
                || self.status_message.is_some();
            let poll_timeout = if busy {
                Duration::from_millis(16)
            } else {
                Duration::from_secs(1)
            };

            if event::poll(poll_timeout).unwrap_or(false) {
                let ev = match event::read() {
                    Ok(ev) => ev,
                    Err(_) => continue,
                };
                if let Event::Mouse(mouse) = ev {
                    self.handle_mouse(mouse);
                    continue;
                }
                if let Event::Key(key) = ev {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if let Some(action) = self.handle_key(key.code) {
                        return Ok(action);
                    }
                }
            } else {
                self.handle_idle()?;
            }
        }
    }

    fn progress_search(&mut self) {
        if self.search_state.searching {
            let _ = self.search_state.search_tick(self.document);
            if !self.search_state.jumped && !self.search_state.hits.is_empty() {
                self.search_state.jumped = true;
                self.search_state.next_hit_from_page(self.pdf_state.current_page());
                if let Some(hit) = self.search_state.current_hit() {
                    self.pdf_state.scroll_to_point(hit.page, hit.y0);
                }
            }
        }
    }

    fn expire_status_message(&mut self) {
        if let Some((_, created)) = &self.status_message {
            if created.elapsed() > Duration::from_secs(3) {
                self.status_message = None;
            }
        }
    }

    fn check_forward_search(&mut self) {
        let listener = match self.listener {
            Some(l) => l,
            None => return,
        };
        while let Ok((stream, _)) = listener.accept() {
            let _ = stream.set_nonblocking(false);
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("forward:") {
                    let parts: Vec<&str> = rest.splitn(3, ':').collect();
                    if parts.len() == 3 {
                        if let (Ok(src_line), Ok(col)) =
                            (parts[0].parse::<usize>(), parts[1].parse::<usize>())
                        {
                            let tex_file = parts[2];
                            if let Some(fwd) =
                                synctex_view(self.document.path(), tex_file, src_line, col)
                            {
                                self.pdf_state.scroll_to_point(fwd.page, fwd.y);
                                self.set_status(format!("Forward: {}:{}", tex_file, src_line));
                            } else {
                                self.set_status("Forward search: no result".to_string());
                            }
                        }
                    }
                }
                let mut stream = stream;
                let _ = writeln!(stream, "ok");
            }
        }
    }

    fn draw(&mut self) -> io::Result<()> {
        let Self {
            terminal, pdf_state, toc_state, link_state,
            search_state, goto_input, search_input, status_message,
            doc_picker, synctex_probe, open_docs, current_idx, ..
        } = self;

        let draw_result = terminal.draw(|frame| {
            let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
                .split(frame.area());
            let main_area = outer[0];
            let status_area = outer[1];

            // Main area
            if doc_picker.is_some() {
                render_doc_picker(frame, main_area, open_docs, *current_idx, doc_picker.unwrap());
            } else {
                let search_opt = if search_state.active { Some(&**search_state) } else { None };
                render_pdf_view(frame, main_area, pdf_state, toc_state, link_state, search_opt, synctex_probe.is_some());
            }

            // Status bar
            render_status_bar(frame, status_area, pdf_state, link_state, search_state, synctex_probe, status_message, goto_input, search_input);
        });

        match draw_result {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left)
                if mouse.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                if let Some((page, pdf_x, pdf_y)) =
                    self.pdf_state.terminal_to_pdf(mouse.row, mouse.column)
                {
                    match synctex_edit(self.document.path(), page + 1, pdf_x, pdf_y) {
                        Some(result) => {
                            if !jump_to_neovim(&result.file, result.line) {
                                self.set_status(format!("SyncTeX: {}:{}", result.file, result.line));
                            }
                        }
                        None => {
                            self.set_status("SyncTeX: no result at click".to_string());
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp => self.pdf_state.scroll_up(5),
            MouseEventKind::ScrollDown => self.pdf_state.scroll_down(5),
            _ => {}
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> Option<AppAction> {
        if self.doc_picker.is_some() {
            return self.handle_doc_picker_key(code);
        }
        if self.synctex_probe.is_some() {
            self.handle_synctex_key(code);
            return None;
        }
        if self.search_input.is_some() {
            self.handle_search_input_key(code);
            return None;
        }
        if self.goto_input.is_some() {
            self.handle_goto_key(code);
            return None;
        }
        if self.toc_state.visible {
            self.handle_toc_key(code);
            return None;
        }
        if self.link_state.active {
            self.handle_link_key(code);
            return None;
        }
        self.handle_normal_key(code)
    }

    fn handle_doc_picker_key(&mut self, code: KeyCode) -> Option<AppAction> {
        let sel = self.doc_picker.as_mut().unwrap();
        match code {
            KeyCode::Esc => { self.doc_picker = None; }
            KeyCode::Char('j') | KeyCode::Down => {
                if *sel + 1 < self.open_docs.len() { *sel += 1; }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *sel = sel.saturating_sub(1);
            }
            KeyCode::Enter => {
                let idx = *sel;
                self.doc_picker = None;
                if idx != self.current_idx {
                    return Some(AppAction::SwitchDoc(idx));
                }
            }
            _ => {}
        }
        None
    }

    fn handle_synctex_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.synctex_probe = None;
                self.last_probe_grid.clear();
                self.pdf_state.clear_probe_markers();
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(input) = self.synctex_probe.as_mut() {
                    input.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.synctex_probe.as_mut() {
                    input.pop();
                }
            }
            KeyCode::Enter => {
                if let Some(input) = self.synctex_probe.take() {
                    self.pdf_state.clear_probe_markers();
                    if let Ok(num) = input.parse::<usize>() {
                        if let Some(cell) = self.last_probe_grid.iter().find(|c| c.number == num) {
                            if !jump_to_neovim(&cell.file, cell.line) {
                                self.set_status(format!("SyncTeX: {}:{}", cell.file, cell.line));
                            }
                        } else {
                            self.set_status(format!("SyncTeX: invalid cell {}", num));
                        }
                    }
                    self.last_probe_grid.clear();
                }
            }
            _ => {}
        }
    }

    fn handle_search_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => { *self.search_input = None; }
            KeyCode::Enter => {
                if let Some(input) = self.search_input.as_ref() {
                    if !input.is_empty() {
                        let query = input.clone();
                        let current_page = self.pdf_state.current_page();
                        self.search_state.start_search(&query, self.document.page_count(), current_page);
                    }
                }
                *self.search_input = None;
            }
            KeyCode::Backspace => {
                if let Some(input) = self.search_input.as_mut() { input.pop(); }
            }
            KeyCode::Char(c) => {
                if let Some(input) = self.search_input.as_mut() { input.push(c); }
            }
            _ => {}
        }
    }

    fn handle_goto_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => { *self.goto_input = None; }
            KeyCode::Enter => {
                if let Some(input) = self.goto_input.as_ref() {
                    if let Ok(page) = input.parse::<usize>() {
                        if page >= 1 && page <= self.pdf_state.page_count() {
                            self.pdf_state.go_to_page(page - 1);
                        }
                    }
                }
                *self.goto_input = None;
            }
            KeyCode::Backspace => {
                if let Some(input) = self.goto_input.as_mut() { input.pop(); }
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(input) = self.goto_input.as_mut() { input.push(c); }
            }
            _ => {}
        }
    }

    fn handle_toc_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => { self.toc_state.visible = false; }
            KeyCode::Char('t') => self.toc_state.toggle(),
            KeyCode::Char('j') | KeyCode::Down => self.toc_state.next(),
            KeyCode::Char('k') | KeyCode::Up => self.toc_state.prev(),
            KeyCode::Enter => {
                if let Some(page) = self.toc_state.selected_page() {
                    self.pdf_state.go_to_page(page);
                    self.toc_state.visible = false;
                }
            }
            _ => {}
        }
    }

    fn handle_link_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.link_state.deactivate(),
            KeyCode::Char('j') | KeyCode::Down => self.link_state.next(),
            KeyCode::Char('k') | KeyCode::Up => self.link_state.prev(),
            KeyCode::Enter => {
                if let Some(link) = self.link_state.selected_link().cloned() {
                    self.link_state.push_position(self.pdf_state.global_scroll);
                    self.pdf_state.go_to_page(link.target_page);
                    self.link_state.deactivate();
                    self.link_state.page = usize::MAX;
                }
            }
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, code: KeyCode) -> Option<AppAction> {
        match code {
            KeyCode::Char('q') => return Some(AppAction::Quit),
            KeyCode::Char('x') => return Some(AppAction::CloseDoc),
            KeyCode::Char('o') => return Some(AppAction::OpenZotero),
            KeyCode::Char('O') => return Some(AppAction::OpenLatest),
            KeyCode::Tab => {
                if self.open_docs.len() > 1 {
                    let next = (self.current_idx + 1) % self.open_docs.len();
                    return Some(AppAction::SwitchDoc(next));
                }
            }
            KeyCode::BackTab => {
                if self.open_docs.len() > 1 {
                    let prev = if self.current_idx == 0 { self.open_docs.len() - 1 } else { self.current_idx - 1 };
                    return Some(AppAction::SwitchDoc(prev));
                }
            }
            KeyCode::Char('d') => { self.doc_picker = Some(self.current_idx); }
            KeyCode::Char('s') => {
                if let Some((ax, ay, aw, ah)) = self.pdf_state.last_render_area {
                    let area = ratatui::layout::Rect::new(ax, ay, aw, ah);
                    let grid = compute_probe_grid(self.pdf_state, self.document.path(), area);
                    if grid.is_empty() {
                        self.set_status("SyncTeX: no results on visible area".to_string());
                    } else {
                        let markers: Vec<_> = grid.iter()
                            .map(|c| (c.page, c.pdf_x, c.pdf_y, c.number))
                            .collect();
                        self.pdf_state.apply_probe_markers(&markers);
                        self.last_probe_grid = grid;
                        self.synctex_probe = Some(String::new());
                    }
                }
            }
            KeyCode::Esc => {
                if self.search_state.active { self.search_state.clear(); }
            }
            KeyCode::Char('/') => { *self.search_input = Some(String::new()); }
            KeyCode::Char('g') => { *self.goto_input = Some(String::new()); }
            KeyCode::Char('t') => {
                if self.toc_state.has_entries() { self.toc_state.toggle(); }
            }
            KeyCode::Char('l') => {
                let page = self.pdf_state.current_page();
                let _ = self.link_state.activate(self.document, page);
            }
            KeyCode::Char('b') => {
                if let Some(pos) = self.link_state.pop_position() {
                    self.pdf_state.global_scroll = pos.global_scroll;
                }
            }
            KeyCode::Char('n') => {
                if self.search_state.active {
                    self.search_state.next_hit();
                    if let Some(hit) = self.search_state.current_hit() {
                        self.pdf_state.scroll_to_point(hit.page, hit.y0);
                    }
                } else {
                    self.pdf_state.next_page();
                }
            }
            KeyCode::Char('p') => {
                if self.search_state.active {
                    self.search_state.prev_hit();
                    if let Some(hit) = self.search_state.current_hit() {
                        self.pdf_state.scroll_to_point(hit.page, hit.y0);
                    }
                } else {
                    self.pdf_state.prev_page();
                }
            }
            KeyCode::Left | KeyCode::PageUp => self.pdf_state.prev_page(),
            KeyCode::Right | KeyCode::PageDown => self.pdf_state.next_page(),
            KeyCode::Char('j') | KeyCode::Down => self.pdf_state.scroll_down(3),
            KeyCode::Char('k') | KeyCode::Up => self.pdf_state.scroll_up(3),
            KeyCode::Char('i') => self.pdf_state.toggle_invert(self.document),
            KeyCode::Char('+') | KeyCode::Char('=') => self.pdf_state.zoom_in(self.document),
            KeyCode::Char('-') => self.pdf_state.zoom_out(self.document),
            KeyCode::Char('w') => self.pdf_state.fit_width(self.document),
            KeyCode::Home => self.pdf_state.first_page(),
            KeyCode::End => self.pdf_state.last_page(),
            _ => {}
        }
        None
    }

    fn handle_idle(&mut self) -> io::Result<()> {
        if self.last_mtime_check.elapsed() >= Duration::from_secs(1) {
            self.last_mtime_check = Instant::now();
            if let Ok(meta) = fs::metadata(self.document.path()) {
                if let Ok(mtime) = meta.modified() {
                    if self.last_mtime.map_or(true, |prev| mtime != prev) {
                        self.last_mtime = Some(mtime);
                        if self.document.reload().is_ok() {
                            let saved_scroll = self.pdf_state.global_scroll;
                            self.pdf_state.on_reload(self.document);
                            let _ = self.pdf_state.initial_render(self.document);
                            self.pdf_state.global_scroll = saved_scroll;
                            self.search_state.clear();
                            self.link_state.deactivate();
                            self.link_state.page = usize::MAX;
                            let outlines = self.document.outlines().unwrap_or_default();
                            *self.toc_state = TocState::new(&outlines);
                            self.set_status("File reloaded".to_string());
                        }
                    }
                }
            }
        }
        if !self.pdf_state.prerender_done() {
            while self.pdf_state.prerender_tick(self.document) {
                if event::poll(Duration::from_millis(0))? {
                    break;
                }
            }
        }
        Ok(())
    }
}

fn render_doc_picker(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    open_docs: &[OpenDoc],
    current_idx: usize,
    sel: usize,
) {
    let title_style = Style::default().fg(Color::Black).bg(Color::Cyan);
    let title_area = ratatui::layout::Rect {
        x: area.x, y: area.y, width: area.width, height: 1,
    };
    Paragraph::new(Span::styled(" Open Documents ", title_style))
        .style(title_style)
        .render(title_area, frame.buffer_mut());

    let list_height = (area.height as usize).saturating_sub(1);
    for (i, doc) in open_docs.iter().enumerate().take(list_height) {
        let label = std::path::Path::new(&doc.path)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| doc.path.clone());
        let marker = if i == current_idx { "* " } else { "  " };
        let text = format!(" {}{}", marker, label);
        let width = area.width as usize;
        let truncated = if text.len() > width {
            format!("{}…", &text[..width - 1])
        } else {
            text
        };
        let style = if i == sel {
            Style::default().fg(Color::Black).bg(Color::White)
        } else {
            Style::default().fg(Color::White).bg(Color::Reset)
        };
        let row_area = ratatui::layout::Rect {
            x: area.x, y: area.y + 1 + i as u16,
            width: area.width, height: 1,
        };
        Paragraph::new(Span::styled(truncated, style))
            .style(style)
            .render(row_area, frame.buffer_mut());
    }
}

fn render_pdf_view(
    frame: &mut ratatui::Frame,
    main_area: ratatui::layout::Rect,
    pdf_state: &mut PdfViewState,
    toc_state: &mut TocState,
    link_state: &mut LinkState,
    search_opt: Option<&SearchState>,
    probe_active: bool,
) {
    if toc_state.visible {
        let cols = Layout::horizontal([
            Constraint::Percentage(30),
            Constraint::Percentage(70),
        ])
        .split(main_area);
        TocWidget.render(cols[0], frame.buffer_mut(), toc_state);
        if !probe_active {
            if let Err(e) = pdf_state.update_image(Some(link_state), search_opt) {
                let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                frame.render_widget(msg, cols[1]);
                return;
            }
        }
        frame.render_stateful_widget(PdfWidget, cols[1], pdf_state);
    } else {
        if !probe_active {
            if let Err(e) = pdf_state.update_image(Some(link_state), search_opt) {
                let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                frame.render_widget(msg, main_area);
                return;
            }
        }
        frame.render_stateful_widget(PdfWidget, main_area, pdf_state);
    }
}

fn render_status_bar(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    pdf_state: &PdfViewState,
    link_state: &LinkState,
    search_state: &SearchState,
    synctex_probe: &Option<String>,
    status_message: &Option<(String, Instant)>,
    goto_input: &Option<String>,
    search_input: &Option<String>,
) {
    if let Some(input) = synctex_probe {
        let prompt = format!(" SyncTeX probe: {}█  (Enter: jump, Esc: cancel) ", input);
        let line = Line::from(vec![Span::styled(prompt, Style::default().fg(Color::Black).bg(Color::Yellow))]);
        Paragraph::new(line).style(Style::default().bg(Color::Yellow)).render(area, frame.buffer_mut());
    } else if let Some((msg, _)) = status_message {
        let line = Line::from(vec![Span::styled(format!(" {} ", msg), Style::default().fg(Color::White).bg(Color::Magenta))]);
        Paragraph::new(line).style(Style::default().bg(Color::Magenta)).render(area, frame.buffer_mut());
    } else if let Some(input) = search_input.as_ref() {
        let prompt = format!(" /{}█ ", input);
        let line = Line::from(vec![Span::styled(prompt, Style::default().fg(Color::Black).bg(Color::Cyan))]);
        Paragraph::new(line).style(Style::default().bg(Color::Cyan)).render(area, frame.buffer_mut());
    } else if let Some(input) = goto_input.as_ref() {
        let prompt = format!(" Go to page (1-{}): {}█ ", pdf_state.page_count(), input);
        let line = Line::from(vec![Span::styled(prompt, Style::default().fg(Color::Black).bg(Color::Cyan))]);
        Paragraph::new(line).style(Style::default().bg(Color::Cyan)).render(area, frame.buffer_mut());
    } else {
        frame.render_widget(
            StatusBar { state: pdf_state, link_state: Some(link_state), search_state: Some(search_state) },
            area,
        );
    }
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    document: &mut Document,
    pdf_state: &mut PdfViewState,
    toc_state: &mut TocState,
    link_state: &mut LinkState,
    search_state: &mut SearchState,
    goto_input: &mut Option<String>,
    search_input: &mut Option<String>,
    listener: Option<&UnixListener>,
    open_docs: &[OpenDoc],
    current_idx: usize,
) -> io::Result<AppAction> {
    AppState::new(
        terminal, document, pdf_state, toc_state, link_state,
        search_state, goto_input, search_input, listener,
        open_docs, current_idx,
    ).run()
}

/// A row in the Zotero browser: either a collection (folder) or a paper.
enum BrowserItem {
    Collection { id: i64, name: String },
    Paper { entry_idx: usize },
}

fn run_zotero_browser(library: &ZoteroLibrary) -> io::Result<Option<std::path::PathBuf>> {
    let mut filter = String::new();
    let mut selected: usize = 0;
    // Stack of collection IDs we've navigated into (None = root)
    let mut path_stack: Vec<Option<i64>> = vec![None];

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        let current_collection = *path_stack.last().unwrap();

        // Build the list of items to display
        let mut items: Vec<BrowserItem> = Vec::new();

        if filter.is_empty() {
            // Show subcollections first, then papers in this collection
            let mut children = library.child_collections(current_collection);
            children.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            for c in children {
                items.push(BrowserItem::Collection { id: c.id, name: c.name.clone() });
            }

            let paper_indices = if current_collection.is_none() {
                // At root: show all papers
                (0..library.entries.len()).collect::<Vec<_>>()
            } else {
                library.entries_in_collection(current_collection.unwrap())
            };
            for idx in paper_indices {
                items.push(BrowserItem::Paper { entry_idx: idx });
            }
        } else {
            // When filtering, search across all papers regardless of collection
            let lower = filter.to_lowercase();
            for (i, e) in library.entries.iter().enumerate() {
                if e.title.to_lowercase().contains(&lower)
                    || e.authors.to_lowercase().contains(&lower)
                    || e.year.contains(&lower)
                {
                    items.push(BrowserItem::Paper { entry_idx: i });
                }
            }
        }

        if selected >= items.len() {
            selected = items.len().saturating_sub(1);
        }

        // Build breadcrumb path
        let breadcrumb = {
            let mut parts = vec!["Library".to_string()];
            for cid in path_stack.iter().skip(1) {
                if let Some(id) = cid {
                    if let Some(c) = library.collections.iter().find(|c| c.id == *id) {
                        parts.push(c.name.clone());
                    }
                }
            }
            parts.join(" > ")
        };

        terminal.draw(|frame| {
            let chunks = Layout::vertical([
                Constraint::Length(1), // breadcrumb / search
                Constraint::Min(1),   // list
                Constraint::Length(1), // status
            ])
            .split(frame.area());

            // Top bar: breadcrumb or search
            if filter.is_empty() {
                Paragraph::new(Line::from(vec![Span::styled(
                    format!(" {}", breadcrumb),
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]))
                .style(Style::default().bg(Color::Cyan))
                .render(chunks[0], frame.buffer_mut());
            } else {
                Paragraph::new(Line::from(vec![Span::styled(
                    format!(" /{}█", filter),
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]))
                .style(Style::default().bg(Color::Cyan))
                .render(chunks[0], frame.buffer_mut());
            }

            // List
            let list_height = chunks[1].height as usize;
            let scroll_offset = if selected >= list_height {
                selected - list_height + 1
            } else {
                0
            };

            for (row, item) in items.iter().skip(scroll_offset).take(list_height).enumerate() {
                let is_selected = scroll_offset + row == selected;
                let width = chunks[1].width as usize;

                let text = match item {
                    BrowserItem::Collection { name, .. } => {
                        format!("[{}]", name)
                    }
                    BrowserItem::Paper { entry_idx } => {
                        let e = &library.entries[*entry_idx];
                        let year_part = if e.year.is_empty() { String::new() } else { format!(" ({})", e.year) };
                        let author_part = if e.authors.is_empty() { String::new() } else { format!(" — {}", e.authors) };
                        format!("  {}{}{}", e.title, author_part, year_part)
                    }
                };

                let truncated = if text.len() > width {
                    format!("{}…", &text[..width.saturating_sub(1)])
                } else {
                    text
                };

                let style = match (is_selected, item) {
                    (true, _) => Style::default().fg(Color::Black).bg(Color::White),
                    (false, BrowserItem::Collection { .. }) => Style::default().fg(Color::Yellow),
                    (false, BrowserItem::Paper { .. }) => Style::default().fg(Color::White),
                };

                let area = ratatui::layout::Rect {
                    x: chunks[1].x,
                    y: chunks[1].y + row as u16,
                    width: chunks[1].width,
                    height: 1,
                };
                Paragraph::new(Span::styled(truncated, style))
                    .style(style)
                    .render(area, frame.buffer_mut());
            }

            // Status bar
            let coll_count = items.iter().filter(|i| matches!(i, BrowserItem::Collection { .. })).count();
            let paper_count = items.len() - coll_count;
            let status = format!(
                " {} collections, {} papers | /: search | Enter: open | Backspace: back | Esc: quit ",
                coll_count, paper_count,
            );
            Paragraph::new(Line::from(vec![Span::styled(
                status,
                Style::default().fg(Color::White).bg(Color::DarkGray),
            )]))
            .style(Style::default().bg(Color::DarkGray))
            .render(chunks[2], frame.buffer_mut());
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Esc => {
                        if !filter.is_empty() {
                            filter.clear();
                            selected = 0;
                        } else {
                            break None;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(item) = items.get(selected) {
                            match item {
                                BrowserItem::Collection { id, .. } => {
                                    path_stack.push(Some(*id));
                                    selected = 0;
                                    filter.clear();
                                }
                                BrowserItem::Paper { entry_idx } => {
                                    break Some(library.entries[*entry_idx].pdf_path.clone());
                                }
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if !filter.is_empty() {
                            filter.pop();
                            selected = 0;
                        } else if path_stack.len() > 1 {
                            path_stack.pop();
                            selected = 0;
                        }
                    }
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        if !items.is_empty() {
                            selected = (selected + 1).min(items.len() - 1);
                        }
                    }
                    KeyCode::Char('/') if filter.is_empty() => {
                        // Enter search mode - just start accepting characters
                        // The filter is already empty, typing will populate it
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        selected = 0;
                    }
                    _ => {}
                }
            }
        }
    };

    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);
    Ok(result)
}
