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
    load_session, save_session, list_sessions, move_sessions_dir, lookup_by_path, Session, SessionDoc,
};

fn render_metadata_overlay(
    fields: &[(String, String)],
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    let title_style = Style::default().fg(Color::Black).bg(Color::Cyan);
    let title_area = ratatui::layout::Rect {
        x: area.x, y: area.y, width: area.width, height: 1,
    };
    Paragraph::new(Span::styled(" Zotero Metadata (Esc/m: close) ", title_style))
        .style(title_style)
        .render(title_area, buf);

    let label_style = Style::default().fg(Color::Yellow);
    let value_style = Style::default().fg(Color::White);
    let max_label = fields.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    let prefix_width = 2 + max_label + 2; // "  Label: "
    let value_width = (area.width as usize).saturating_sub(prefix_width);
    let mut row = area.y + 2;
    for (label, value) in fields.iter() {
        if row >= area.y + area.height { break; }
        let prefix = format!("  {:>width$}: ", label, width = max_label);
        let lines: Vec<&str> = if value_width > 0 && value.len() > value_width {
            let mut parts = Vec::new();
            let mut start = 0;
            while start < value.len() {
                let end = (start + value_width).min(value.len());
                parts.push(&value[start..end]);
                start = end;
            }
            parts
        } else {
            vec![value.as_str()]
        };
        for (li, line_text) in lines.iter().enumerate() {
            if row >= area.y + area.height { break; }
            let line_area = ratatui::layout::Rect {
                x: area.x, y: row, width: area.width, height: 1,
            };
            let line = if li == 0 {
                Line::from(vec![
                    Span::styled(prefix.clone(), label_style),
                    Span::styled(line_text.to_string(), value_style),
                ])
            } else {
                Line::from(vec![
                    Span::raw(" ".repeat(prefix_width)),
                    Span::styled(line_text.to_string(), value_style),
                ])
            };
            Paragraph::new(line).render(line_area, buf);
            row += 1;
        }
    }
}

fn build_session(open_docs: &[OpenDoc], current_idx: usize, pdf_state: &PdfViewState) -> Session {
    Session {
        docs: open_docs.iter().enumerate().map(|(i, d)| SessionDoc {
            path: d.path.clone(),
            scroll: if i == current_idx { pdf_state.global_scroll } else { d.scroll },
            zoom: if i == current_idx { pdf_state.zoom } else { d.zoom },
        }).collect(),
        current: current_idx,
    }
}

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

fn print_help() {
    println!("tui-pdf — a terminal PDF viewer with image rendering

USAGE:
    tui-pdf [OPTIONS] <pdf>...

ARGUMENTS:
    <pdf>...                    One or more PDF files to open

OPTIONS:
    -h, --help                  Show this help message
    --session <name>            Restore a saved session by name
    --list-sessions             List all saved sessions
    --zotero                    Browse Zotero library and open a PDF
    --setup-zotero <dir>        Configure Zotero data directory (one-time)
    --move-sessions <dir>       Move session storage to a custom directory
    --forward <line:col:file> <pdf>
                                Send forward search to a running instance

KEYBINDINGS:
    j/k, Up/Down                Scroll up/down
    n/p, Right/Left, PgDn/PgUp  Next/previous page
    Home/End                    First/last page
    g                           Go to page number
    +/- or =/−                  Zoom in/out
    w                           Fit to width
    /                           Search text (n/p: next/prev match)
    t                           Toggle table of contents
    l                           Enter link mode (j/k: select, Enter: follow)
    b                           Go back after following a link
    i                           Toggle color inversion
    s                           SyncTeX probe (keyboard reverse search)
    o                           Open Zotero browser
    O                           Open latest Zotero PDF
    S                           Save session (prompts for name first time)
    d                           Document picker
    Tab/Shift+Tab               Cycle between open documents
    x                           Close current document
    q/Esc                       Quit
    Mouse wheel                 Scroll
    Ctrl+Click                  SyncTeX reverse search");
}

fn main() -> io::Result<()> {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}\n\nBacktrace:\n{}", std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write("/tmp/tui-pdf-panic.log", &msg);
    }));

    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args[1] == "--help" || args[1] == "-h" {
        print_help();
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    // Handle --list-sessions
    if args[1] == "--list-sessions" {
        let sessions = list_sessions();
        if sessions.is_empty() {
            println!("No saved sessions.");
        } else {
            println!("Saved sessions:");
            for name in &sessions {
                if let Some(sess) = load_session(name) {
                    println!("  {} ({} doc{})", name, sess.docs.len(), if sess.docs.len() == 1 { "" } else { "s" });
                    for doc in &sess.docs {
                        let short = std::path::Path::new(&doc.path)
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| doc.path.clone());
                        println!("    {}", short);
                    }
                } else {
                    println!("  {}", name);
                }
            }
        }
        std::process::exit(0);
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

    // Handle --move-sessions: move session storage to a custom directory
    if args.len() >= 3 && args[1] == "--move-sessions" {
        let dir = &args[2];
        move_sessions_dir(dir).unwrap_or_else(|e| {
            eprintln!("Failed to move sessions: {e}");
            std::process::exit(1);
        });
        println!("Sessions moved to {}", dir);
        std::process::exit(0);
    }

    // Handle --session: restore a named session
    if args.len() >= 3 && args[1] == "--session" {
        let name = &args[2];
        match load_session(name) {
            Some(session) if !session.docs.is_empty() => {
                let paths: Vec<String> = session.docs.iter().map(|d| d.path.clone()).collect();
                let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
                return open_viewer(&refs, Some(name.clone()), Some(&session));
            }
            _ => {
                eprintln!("Session '{}' not found or empty", name);
                std::process::exit(1);
            }
        }
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
                return open_viewer(&[&s], None, None);
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
    open_viewer(&pdf_paths, None, None)
}

fn open_viewer(pdf_paths: &[&str], session_name: Option<String>, session: Option<&Session>) -> io::Result<()> {
    let mut open_docs: Vec<OpenDoc> = if let Some(sess) = session {
        sess.docs.iter().map(|d| OpenDoc {
            path: d.path.clone(),
            scroll: d.scroll,
            zoom: d.zoom,
        }).collect()
    } else {
        pdf_paths.iter().map(|p| OpenDoc {
            path: p.to_string(),
            scroll: 0,
            zoom: 1.0,
        }).collect()
    };
    let mut current_idx: usize = session.map_or(0, |s| s.current.min(open_docs.len().saturating_sub(1)));
    let mut current_path = open_docs[current_idx].path.clone();
    let mut inverted = false;
    let zotero_dir: Option<String> = load_config().zotero_dir;
    let session_name = session_name;

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
            &session_name,
            &zotero_dir,
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
    session_name: &Option<String>,
    zotero_dir: &Option<String>,
) -> io::Result<AppAction> {
    // Auto-reload state
    let mut last_mtime: Option<SystemTime> = fs::metadata(document.path())
        .and_then(|m| m.modified())
        .ok();
    let mut last_mtime_check = Instant::now();

    // Status message (shown temporarily, expires after 3s)
    let mut status_message: Option<(String, Instant)> = None;

    // Document picker state
    let mut doc_picker: Option<usize> = None; // selected index

    // SyncTeX probe state
    let mut synctex_probe: Option<String> = None;
    let mut last_probe_grid: Vec<ProbeCell> = Vec::new();

    // Session name input
    let mut session_input: Option<String> = None;
    let mut saved_session_name: Option<String> = None;

    // Metadata view
    let mut metadata_view: Option<Vec<(String, String)>> = None;

    loop {
        // Progress incremental search
        if search_state.searching {
            let _ = search_state.search_tick(document);
            if !search_state.jumped && !search_state.hits.is_empty() {
                search_state.jumped = true;
                search_state.next_hit_from_page(pdf_state.current_page());
                if let Some(hit) = search_state.current_hit() {
                    pdf_state.scroll_to_point(hit.page, hit.y0);
                }
            }
        }

        // Expire status message after 3 seconds
        if let Some((_, created)) = &status_message {
            if created.elapsed() > Duration::from_secs(3) {
                status_message = None;
            }
        }

        // Check for forward search commands from the socket
        if let Some(l) = listener {
            while let Ok((stream, _)) = l.accept() {
                let _ = stream.set_nonblocking(false);
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("forward:") {
                        // Parse "line:col:file"
                        let parts: Vec<&str> = rest.splitn(3, ':').collect();
                        if parts.len() == 3 {
                            if let (Ok(src_line), Ok(col)) =
                                (parts[0].parse::<usize>(), parts[1].parse::<usize>())
                            {
                                let tex_file = parts[2];
                                if let Some(fwd) =
                                    synctex_view(document.path(), tex_file, src_line, col)
                                {
                                    pdf_state.scroll_to_point(fwd.page, fwd.y);
                                    status_message = Some((
                                        format!("Forward: {}:{}", tex_file, src_line),
                                        Instant::now(),
                                    ));
                                } else {
                                    status_message = Some((
                                        "Forward search: no result".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                    }
                    // Send ack
                    let mut stream = stream;
                    let _ = writeln!(stream, "ok");
                }
            }
        }

        // Ensure current page is rendered before draw (avoids blank on uncached pages)
        pdf_state.ensure_visible_rendered(document);

        let draw_result = terminal.draw(|frame| {
            let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
                .split(frame.area());

            let main_area = outer[0];
            let status_area = outer[1];

            if let Some(ref fields) = metadata_view {
                render_metadata_overlay(
                    fields,
                    main_area,
                    frame.buffer_mut(),
                );
            } else if doc_picker.is_some() {
                // Document picker: render list in main area
                let sel = doc_picker.unwrap();
                let title_style = Style::default().fg(Color::Black).bg(Color::Cyan);
                let title_area = ratatui::layout::Rect {
                    x: main_area.x, y: main_area.y, width: main_area.width, height: 1,
                };
                Paragraph::new(Span::styled(" Open Documents ", title_style))
                    .style(title_style)
                    .render(title_area, frame.buffer_mut());

                let list_height = (main_area.height as usize).saturating_sub(1);
                for (i, doc) in open_docs.iter().enumerate().take(list_height) {
                    let label = std::path::Path::new(&doc.path)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_else(|| doc.path.clone());
                    let marker = if i == current_idx { "* " } else { "  " };
                    let text = format!(" {}{}", marker, label);
                    let width = main_area.width as usize;
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
                    let area = ratatui::layout::Rect {
                        x: main_area.x, y: main_area.y + 1 + i as u16,
                        width: main_area.width, height: 1,
                    };
                    Paragraph::new(Span::styled(truncated, style))
                        .style(style)
                        .render(area, frame.buffer_mut());
                }
            } else {
                let search_opt = if search_state.active {
                    Some(&*search_state)
                } else {
                    None
                };

                if toc_state.visible {
                    let cols = Layout::horizontal([
                        Constraint::Percentage(30),
                        Constraint::Percentage(70),
                    ])
                    .split(main_area);

                    TocWidget.render(cols[0], frame.buffer_mut(), toc_state);

                    if synctex_probe.is_none() {
                        if let Err(e) = pdf_state.update_image(Some(link_state), search_opt) {
                            let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                            frame.render_widget(msg, cols[1]);
                        } else {
                            frame.render_stateful_widget(PdfWidget, cols[1], pdf_state);
                        }
                    } else {
                        frame.render_stateful_widget(PdfWidget, cols[1], pdf_state);
                    }
                } else {
                    if synctex_probe.is_none() {
                        if let Err(e) = pdf_state.update_image(Some(link_state), search_opt) {
                            let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                            frame.render_widget(msg, main_area);
                        } else {
                            frame.render_stateful_widget(PdfWidget, main_area, pdf_state);
                        }
                    } else {
                        frame.render_stateful_widget(PdfWidget, main_area, pdf_state);
                    }
                }
            }

            // Status bar: session_input > synctex_probe > status_message > search_input > goto_input > normal
            if let Some(ref input) = session_input {
                let prompt = format!(" Session name: {}█  (Enter: save, Esc: cancel) ", input);
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Green),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Green))
                    .render(status_area, frame.buffer_mut());
            } else if let Some(ref input) = synctex_probe {
                let prompt = format!(" SyncTeX probe: {}█  (Enter: jump, Esc: cancel) ", input);
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Yellow),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Yellow))
                    .render(status_area, frame.buffer_mut());
            } else if let Some((ref msg, _)) = status_message {
                let line = Line::from(vec![Span::styled(
                    format!(" {} ", msg),
                    Style::default().fg(Color::White).bg(Color::Magenta),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Magenta))
                    .render(status_area, frame.buffer_mut());
            } else if let Some(input) = search_input.as_ref() {
                let prompt = format!(" /{}█ ", input);
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Cyan))
                    .render(status_area, frame.buffer_mut());
            } else if let Some(input) = goto_input.as_ref() {
                let prompt = format!(
                    " Go to page (1-{}): {}█ ",
                    pdf_state.page_count(),
                    input,
                );
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Cyan))
                    .render(status_area, frame.buffer_mut());
            } else {
                frame.render_widget(
                    StatusBar {
                        state: &*pdf_state,
                        link_state: Some(&*link_state),
                        search_state: Some(&*search_state),
                    },
                    status_area,
                );
            }
        });
        if let Err(e) = draw_result {
            if e.kind() == io::ErrorKind::WouldBlock {
                continue;
            }
            return Err(e);
        }

        // Poll for input: short timeout when there's active work, long when idle
        let busy = search_state.searching
            || !pdf_state.prerender_done()
            || status_message.is_some();
        let poll_timeout = if busy {
            Duration::from_millis(16)
        } else {
            Duration::from_secs(1)
        };
        let poll_result = event::poll(poll_timeout);
        if poll_result.is_err() {
            continue;
        }
        if poll_result.unwrap() {
            let ev = match event::read() {
                Ok(ev) => ev,
                Err(_) => continue,
            };

            // Handle mouse events
            if let Event::Mouse(mouse) = ev {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) if mouse.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                        if let Some((page, pdf_x, pdf_y)) =
                            pdf_state.terminal_to_pdf(mouse.row, mouse.column)
                        {
                            match synctex_edit(document.path(), page + 1, pdf_x, pdf_y) {
                                Some(result) => {
                                    if !jump_to_neovim(&result.file, result.line) {
                                        status_message = Some((
                                            format!("SyncTeX: {}:{}", result.file, result.line),
                                            Instant::now(),
                                        ));
                                    }
                                }
                                None => {
                                    status_message = Some((
                                        "SyncTeX: no result at click".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => pdf_state.scroll_up(5),
                    MouseEventKind::ScrollDown => pdf_state.scroll_down(5),
                    _ => {}
                }
                continue;
            }

            if let Event::Key(key) = ev {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Metadata view mode
                if metadata_view.is_some() {
                    if key.code == KeyCode::Esc || key.code == KeyCode::Char('m') || key.code == KeyCode::Char('q') {
                        metadata_view = None;
                    }
                    continue;
                }

                // Document picker mode
                if let Some(sel) = doc_picker.as_mut() {
                    match key.code {
                        KeyCode::Esc => { doc_picker = None; }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if *sel + 1 < open_docs.len() { *sel += 1; }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            *sel = sel.saturating_sub(1);
                        }
                        KeyCode::Enter => {
                            let idx = *sel;
                            doc_picker = None;
                            if idx != current_idx {
                                return Ok(AppAction::SwitchDoc(idx));
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Session name input mode
                if session_input.is_some() {
                    match key.code {
                        KeyCode::Esc => { session_input = None; }
                        KeyCode::Enter => {
                            if let Some(name) = session_input.take() {
                                let name = name.trim().to_string();
                                if !name.is_empty() {
                                    let sess = build_session(&open_docs, current_idx, &pdf_state);
                                    match save_session(&name, &sess) {
                                        Ok(_) => {
                                            status_message = Some((
                                                format!("Session '{}' saved", name),
                                                Instant::now(),
                                            ));
                                            saved_session_name = Some(name);
                                        }
                                        Err(e) => {
                                            status_message = Some((
                                                format!("Failed to save session: {}", e),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = session_input.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Some(input) = session_input.as_mut() {
                                input.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // SyncTeX probe mode
                if synctex_probe.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            synctex_probe = None;
                            last_probe_grid.clear();
                            pdf_state.clear_probe_markers();
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() => {
                            if let Some(input) = synctex_probe.as_mut() {
                                input.push(c);
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = synctex_probe.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(input) = synctex_probe.take() {
                                pdf_state.clear_probe_markers();
                                if let Ok(num) = input.parse::<usize>() {
                                    if let Some(cell) = last_probe_grid.iter().find(|c| c.number == num) {
                                        if !jump_to_neovim(&cell.file, cell.line) {
                                            status_message = Some((
                                                format!("SyncTeX: {}:{}", cell.file, cell.line),
                                                Instant::now(),
                                            ));
                                        }
                                    } else {
                                        status_message = Some((
                                            format!("SyncTeX: invalid cell {}", num),
                                            Instant::now(),
                                        ));
                                    }
                                }
                                last_probe_grid.clear();
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Search input mode
                if search_input.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            *search_input = None;
                        }
                        KeyCode::Enter => {
                            if let Some(input) = search_input.as_ref() {
                                if !input.is_empty() {
                                    let query = input.clone();
                                    let current_page = pdf_state.current_page();
                                    search_state.start_search(
                                        &query,
                                        document.page_count(),
                                        current_page,
                                    );
                                }
                            }
                            *search_input = None;
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = search_input.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Some(input) = search_input.as_mut() {
                                input.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Go-to-page input mode
                if goto_input.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            *goto_input = None;
                        }
                        KeyCode::Enter => {
                            if let Some(input) = goto_input.as_ref() {
                                if let Ok(page) = input.parse::<usize>() {
                                    if page >= 1 && page <= pdf_state.page_count() {
                                        pdf_state.go_to_page(page - 1);
                                    }
                                }
                            }
                            *goto_input = None;
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = goto_input.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() => {
                            if let Some(input) = goto_input.as_mut() {
                                input.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                if toc_state.visible {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            toc_state.visible = false;
                        }
                        KeyCode::Char('t') => toc_state.toggle(),
                        KeyCode::Char('j') | KeyCode::Down => toc_state.next(),
                        KeyCode::Char('k') | KeyCode::Up => toc_state.prev(),
                        KeyCode::Enter => {
                            if let Some(page) = toc_state.selected_page() {
                                pdf_state.go_to_page(page);
                                toc_state.visible = false;
                            }
                        }
                        _ => {}
                    }
                } else if link_state.active {
                    match key.code {
                        KeyCode::Esc => link_state.deactivate(),
                        KeyCode::Char('j') | KeyCode::Down => link_state.next(),
                        KeyCode::Char('k') | KeyCode::Up => link_state.prev(),
                        KeyCode::Enter => {
                            if let Some(link) = link_state.selected_link().cloned() {
                                link_state.push_position(pdf_state.global_scroll);
                                pdf_state.go_to_page(link.target_page);
                                link_state.deactivate();
                                link_state.page = usize::MAX;
                            }
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Char('q') => return Ok(AppAction::Quit),
                        KeyCode::Char('x') => return Ok(AppAction::CloseDoc),
                        KeyCode::Char('o') => return Ok(AppAction::OpenZotero),
                        KeyCode::Char('O') => return Ok(AppAction::OpenLatest),
                        KeyCode::Char('S') => {
                            if let Some(name) = saved_session_name.as_ref().or(session_name.as_ref()) {
                                let name = name.clone();
                                let sess = build_session(&open_docs, current_idx, &pdf_state);
                                match save_session(&name, &sess) {
                                    Ok(_) => {
                                        status_message = Some((
                                            format!("Session '{}' saved", name),
                                            Instant::now(),
                                        ));
                                    }
                                    Err(e) => {
                                        status_message = Some((
                                            format!("Failed to save session: {}", e),
                                            Instant::now(),
                                        ));
                                    }
                                }
                            } else {
                                session_input = Some(String::new());
                            }
                        }
                        KeyCode::Tab => {
                            if open_docs.len() > 1 {
                                let next = (current_idx + 1) % open_docs.len();
                                return Ok(AppAction::SwitchDoc(next));
                            }
                        }
                        KeyCode::BackTab => {
                            if open_docs.len() > 1 {
                                let prev = if current_idx == 0 { open_docs.len() - 1 } else { current_idx - 1 };
                                return Ok(AppAction::SwitchDoc(prev));
                            }
                        }
                        KeyCode::Char('d') => {
                            doc_picker = Some(current_idx);
                        }
                        KeyCode::Char('m') => {
                            if let Some(dir) = zotero_dir {
                                if let Some(entry) = lookup_by_path(
                                    std::path::Path::new(dir),
                                    document.path(),
                                ) {
                                    let mut fields = vec![
                                        ("Title".to_string(), entry.title),
                                        ("Authors".to_string(), entry.authors),
                                    ];
                                    if !entry.year.is_empty() {
                                        fields.push(("Year".to_string(), entry.year));
                                    }
                                    fields.push(("File".to_string(), entry.pdf_path.display().to_string()));
                                    metadata_view = Some(fields);
                                } else {
                                    status_message = Some((
                                        "No Zotero metadata found for this file".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            } else {
                                status_message = Some((
                                    "Zotero not configured. Run: tui-pdf --setup-zotero <dir>".to_string(),
                                    Instant::now(),
                                ));
                            }
                        }
                        KeyCode::Char('s') => {
                            if let Some((ax, ay, aw, ah)) = pdf_state.last_render_area {
                                let area = ratatui::layout::Rect::new(ax, ay, aw, ah);
                                let grid = compute_probe_grid(pdf_state, document.path(), area);
                                if grid.is_empty() {
                                    status_message = Some((
                                        "SyncTeX: no results on visible area".to_string(),
                                        Instant::now(),
                                    ));
                                } else {
                                    let markers: Vec<_> = grid.iter()
                                        .map(|c| (c.page, c.pdf_x, c.pdf_y, c.number))
                                        .collect();
                                    pdf_state.apply_probe_markers(&markers);
                                    last_probe_grid = grid;
                                    synctex_probe = Some(String::new());
                                }
                            }
                        }
                        KeyCode::Esc => {
                            if search_state.active {
                                search_state.clear();
                            }
                        }
                        KeyCode::Char('/') => {
                            *search_input = Some(String::new());
                        }
                        KeyCode::Char('g') => {
                            *goto_input = Some(String::new());
                        }
                        KeyCode::Char('t') => {
                            if toc_state.has_entries() {
                                toc_state.toggle();
                            }
                        }
                        KeyCode::Char('l') => {
                            let page = pdf_state.current_page();
                            let _ = link_state.activate(document, page);
                        }
                        KeyCode::Char('b') => {
                            if let Some(pos) = link_state.pop_position() {
                                pdf_state.global_scroll = pos.global_scroll;
                            }
                        }
                        KeyCode::Char('n') => {
                            if search_state.active {
                                search_state.next_hit();
                                if let Some(hit) = search_state.current_hit() {
                                    pdf_state.scroll_to_point(hit.page, hit.y0);
                                }
                            } else {
                                pdf_state.next_page();
                            }
                        }
                        KeyCode::Char('p') => {
                            if search_state.active {
                                search_state.prev_hit();
                                if let Some(hit) = search_state.current_hit() {
                                    pdf_state.scroll_to_point(hit.page, hit.y0);
                                }
                            } else {
                                pdf_state.prev_page();
                            }
                        }
                        KeyCode::Left | KeyCode::PageUp => {
                            pdf_state.prev_page()
                        }
                        KeyCode::Right | KeyCode::PageDown => pdf_state.next_page(),
                        KeyCode::Char('j') | KeyCode::Down => pdf_state.scroll_down(3),
                        KeyCode::Char('k') | KeyCode::Up => pdf_state.scroll_up(3),
                        KeyCode::Char('i') => pdf_state.toggle_invert(document),
                        KeyCode::Char('+') | KeyCode::Char('=') => pdf_state.zoom_in(document),
                        KeyCode::Char('-') => pdf_state.zoom_out(document),
                        KeyCode::Char('w') => pdf_state.fit_width(document),
                        KeyCode::Home => pdf_state.first_page(),
                        KeyCode::End => pdf_state.last_page(),
                        _ => {}
                    }
                }
            }
        } else {
            // Idle: check for file changes (~1s interval) and pre-render
            if last_mtime_check.elapsed() >= Duration::from_secs(1) {
                last_mtime_check = Instant::now();
                if let Ok(meta) = fs::metadata(document.path()) {
                    if let Ok(mtime) = meta.modified() {
                        if last_mtime.map_or(true, |prev| mtime != prev) {
                            last_mtime = Some(mtime);
                            if document.reload().is_ok() {
                                let saved_scroll = pdf_state.global_scroll;
                                pdf_state.on_reload(document);
                                let _ = pdf_state.initial_render(document);
                                pdf_state.global_scroll = saved_scroll;
                                // Clear search and link state
                                search_state.clear();
                                link_state.deactivate();
                                link_state.page = usize::MAX;
                                // Rebuild TOC
                                let outlines = document.outlines().unwrap_or_default();
                                *toc_state = TocState::new(&outlines);
                                status_message = Some((
                                    "File reloaded".to_string(),
                                    Instant::now(),
                                ));
                            }
                        }
                    }
                }
            }

            if !pdf_state.prerender_done() {
                while pdf_state.prerender_tick(document) {
                    if event::poll(Duration::from_millis(0))? {
                        break;
                    }
                }
            }
        }
    }

    #[allow(unreachable_code)]
    Ok(AppAction::Quit)
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
    let mut metadata_view: Option<Vec<(String, String)>> = None;

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

            if let Some(ref fields) = metadata_view {
                render_metadata_overlay(
                    fields,
                    chunks[1],
                    frame.buffer_mut(),
                );
            } else {
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
                // Metadata view: any key closes it
                if metadata_view.is_some() {
                    metadata_view = None;
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
                    KeyCode::Char('m') if filter.is_empty() => {
                        if let Some(item) = items.get(selected) {
                            if let BrowserItem::Paper { entry_idx } = item {
                                let e = &library.entries[*entry_idx];
                                let mut fields = vec![
                                    ("Title".to_string(), e.title.clone()),
                                    ("Authors".to_string(), e.authors.clone()),
                                ];
                                if !e.year.is_empty() {
                                    fields.push(("Year".to_string(), e.year.clone()));
                                }
                                fields.push(("File".to_string(), e.pdf_path.display().to_string()));
                                metadata_view = Some(fields);
                            }
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
