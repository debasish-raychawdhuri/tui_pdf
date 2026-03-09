use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget};

/// A flattened TOC entry with indentation depth.
#[derive(Debug, Clone)]
pub struct TocEntry {
    pub title: String,
    pub page: Option<u32>,
    pub depth: usize,
}

/// Flatten the mupdf outline tree into a list of entries with depth.
pub fn flatten_outlines(outlines: &[mupdf::Outline]) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    fn walk(outlines: &[mupdf::Outline], depth: usize, entries: &mut Vec<TocEntry>) {
        for outline in outlines {
            entries.push(TocEntry {
                title: outline.title.clone(),
                page: outline.page,
                depth,
            });
            if !outline.down.is_empty() {
                walk(&outline.down, depth + 1, entries);
            }
        }
    }
    walk(outlines, 0, &mut entries);
    entries
}

pub struct TocState {
    pub entries: Vec<TocEntry>,
    pub list_state: ListState,
    pub visible: bool,
}

impl TocState {
    pub fn new(outlines: &[mupdf::Outline]) -> Self {
        let entries = flatten_outlines(outlines);
        let mut list_state = ListState::default();
        if !entries.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            entries,
            list_state,
            visible: false,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self.list_state.selected().map_or(0, |i| {
            if i + 1 < self.entries.len() { i + 1 } else { i }
        });
        self.list_state.select(Some(i));
    }

    pub fn prev(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self.list_state.selected().map_or(0, |i| i.saturating_sub(1));
        self.list_state.select(Some(i));
    }

    /// Returns the target page of the currently selected entry.
    pub fn selected_page(&self) -> Option<usize> {
        let idx = self.list_state.selected()?;
        self.entries.get(idx)?.page.map(|p| p as usize)
    }
}

pub struct TocWidget;

impl StatefulWidget for TocWidget {
    type State = TocState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let items: Vec<ListItem> = state
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let indent = "  ".repeat(entry.depth);
                let page_str = entry
                    .page
                    .map(|p| format!(" (p.{})", p + 1))
                    .unwrap_or_default();
                let text = format!("{}{}{}", indent, entry.title, page_str);

                let style = if state.list_state.selected() == Some(i) {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                ListItem::new(Line::from(vec![Span::styled(text, style)]))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::RIGHT)
            .title(" Contents [t] ")
            .style(Style::default().bg(Color::DarkGray));

        let list = List::new(items).block(block);
        StatefulWidget::render(list, area, buf, &mut state.list_state);
    }
}
