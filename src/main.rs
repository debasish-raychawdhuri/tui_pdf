use std::fs;
use std::io::{self, stdout, BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
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
    send_forward, socket_path, synctex_edit, synctex_view, jump_to_neovim,
};

fn main() -> io::Result<()> {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}\n\nBacktrace:\n{}", std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write("/tmp/tui-pdf-panic.log", &msg);
    }));

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: tui-pdf [--forward line:col:file] <path-to-pdf>");
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

    let pdf_path = &args[1];

    let mut document = Document::open(pdf_path).unwrap_or_else(|e| {
        eprintln!("Failed to open PDF: {e}");
        std::process::exit(1);
    });

    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16)));
    picker.set_background_color([0, 0, 0, 255]);

    let mut pdf_state = PdfViewState::new(document.page_count(), picker);

    // Render first 2 pages immediately so the PDF shows right away
    pdf_state.initial_render(&document).unwrap_or_else(|e| {
        eprintln!("Failed to render PDF: {e}");
        std::process::exit(1);
    });

    let outlines = document.outlines().unwrap_or_default();
    let mut toc_state = TocState::new(&outlines);
    let mut link_state = LinkState::new();
    let mut search_state = SearchState::new();
    let mut goto_input: Option<String> = None;
    let mut search_input: Option<String> = None;

    // Create Unix socket for forward search commands
    let sock = socket_path(document.path());
    let _ = fs::remove_file(&sock); // clean up stale socket
    let listener = UnixListener::bind(&sock).ok();
    if let Some(ref l) = listener {
        l.set_nonblocking(true).ok();
    }

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

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
    );

    let _ = stdout().execute(DisableMouseCapture);
    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);

    // Clean up socket
    let _ = fs::remove_file(&sock);

    result
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
) -> io::Result<()> {
    // Auto-reload state
    let mut last_mtime: Option<SystemTime> = fs::metadata(document.path())
        .and_then(|m| m.modified())
        .ok();
    let mut last_mtime_check = Instant::now();

    // Status message (shown temporarily, expires after 3s)
    let mut status_message: Option<(String, Instant)> = None;

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

                if let Err(e) = pdf_state.update_image(Some(link_state), search_opt) {
                    let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                    frame.render_widget(msg, cols[1]);
                } else {
                    frame.render_stateful_widget(PdfWidget, cols[1], pdf_state);
                }
            } else {
                if let Err(e) = pdf_state.update_image(Some(link_state), search_opt) {
                    let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                    frame.render_widget(msg, main_area);
                } else {
                    frame.render_stateful_widget(PdfWidget, main_area, pdf_state);
                }
            }

            // Status bar: status_message > search_input > goto_input > normal
            if let Some((ref msg, _)) = status_message {
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
                        KeyCode::Char('q') => break,
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

    Ok(())
}
