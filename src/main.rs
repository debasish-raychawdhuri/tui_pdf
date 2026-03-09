use std::io::{self, stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::StatefulWidget;
use ratatui::Terminal;
use ratatui_image::picker::Picker;

use tui_pdf::{Document, PdfViewState, PdfWidget, StatusBar, TocState, TocWidget};

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: tui-pdf <path-to-pdf>");
        std::process::exit(1);
    }

    let pdf_path = &args[1];

    let document = Document::open(pdf_path).unwrap_or_else(|e| {
        eprintln!("Failed to open PDF: {e}");
        std::process::exit(1);
    });

    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16)));
    picker.set_background_color([0, 0, 0, 255]);

    let mut pdf_state = PdfViewState::new(document.page_count(), picker);

    let outlines = document.outlines().unwrap_or_default();
    let mut toc_state = TocState::new(&outlines);

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &document, &mut pdf_state, &mut toc_state);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    document: &Document,
    pdf_state: &mut PdfViewState,
    toc_state: &mut TocState,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| {
            let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
                .split(frame.area());

            let main_area = outer[0];
            let status_area = outer[1];

            if toc_state.visible {
                let cols = Layout::horizontal([
                    Constraint::Percentage(30),
                    Constraint::Percentage(70),
                ])
                .split(main_area);

                TocWidget.render(cols[0], frame.buffer_mut(), toc_state);

                if let Err(e) = pdf_state.update_image(document) {
                    let msg =
                        ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                    frame.render_widget(msg, cols[1]);
                } else {
                    frame.render_stateful_widget(PdfWidget, cols[1], pdf_state);
                }
            } else {
                if let Err(e) = pdf_state.update_image(document) {
                    let msg =
                        ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                    frame.render_widget(msg, main_area);
                } else {
                    frame.render_stateful_widget(PdfWidget, main_area, pdf_state);
                }
            }

            frame.render_widget(StatusBar { state: &*pdf_state }, status_area);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if toc_state.visible {
                    // TOC panel is focused
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
                } else {
                    // PDF view is focused
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('t') => {
                            if toc_state.has_entries() {
                                toc_state.toggle();
                            }
                        }
                        KeyCode::Char('n') | KeyCode::Right | KeyCode::PageDown => {
                            pdf_state.next_page()
                        }
                        KeyCode::Char('p') | KeyCode::Left | KeyCode::PageUp => {
                            pdf_state.prev_page()
                        }
                        KeyCode::Char('j') | KeyCode::Down => pdf_state.scroll_down(3),
                        KeyCode::Char('k') | KeyCode::Up => pdf_state.scroll_up(3),
                        KeyCode::Char('+') | KeyCode::Char('=') => pdf_state.zoom_in(),
                        KeyCode::Char('-') => pdf_state.zoom_out(),
                        KeyCode::Home => pdf_state.first_page(),
                        KeyCode::End => pdf_state.last_page(),
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(())
}
