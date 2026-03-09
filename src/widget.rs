use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, StatefulWidget, Widget};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;

use crate::document::Document;
use crate::error::Result;
use crate::links::LinkState;
use crate::renderer::{
    compute_stripe_count, overlay_highlights, render_page_dpi, split_into_stripes, HighlightRect,
    PageCache,
};

pub struct PdfViewState {
    pub global_scroll: usize,
    pub zoom: f32,
    page_count: usize,
    picker: Picker,

    // Geometry: stripe count per page, prefix sums for global offset
    page_stripe_counts: Vec<usize>,
    cumulative_stripes: Vec<usize>,
    total_stripes: usize,

    // Sliding window of rendered pages: page_index -> stripes
    rendered_pages: HashMap<usize, Vec<StatefulProtocol>>,

    cache: PageCache,
    dirty: bool,
    last_key: u32,
    // Track link overlay state to know when to rebuild stripes
    last_link_overlay: Option<(usize, usize)>, // (page, selected_link)
}

impl PdfViewState {
    pub fn new(page_count: usize, picker: Picker) -> Self {
        Self {
            global_scroll: 0,
            zoom: 1.0,
            page_count,
            picker,
            page_stripe_counts: Vec::new(),
            cumulative_stripes: Vec::new(),
            total_stripes: 0,
            rendered_pages: HashMap::new(),
            cache: PageCache::new(),
            dirty: true,
            last_key: 0,
            last_link_overlay: None,
        }
    }

    /// Which page is at the top of the viewport.
    pub fn current_page(&self) -> usize {
        if self.cumulative_stripes.is_empty() {
            return 0;
        }
        match self.cumulative_stripes.binary_search(&self.global_scroll) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    pub fn next_page(&mut self) {
        let cur = self.current_page();
        if cur + 1 < self.page_count {
            self.global_scroll = self.cumulative_stripes[cur + 1];
        }
    }

    pub fn prev_page(&mut self) {
        let cur = self.current_page();
        if cur > 0 {
            self.global_scroll = self.cumulative_stripes[cur - 1];
        }
    }

    pub fn go_to_page(&mut self, page: usize) {
        if page < self.page_count && !self.cumulative_stripes.is_empty() {
            self.global_scroll = self.cumulative_stripes[page];
        }
    }

    pub fn first_page(&mut self) {
        self.go_to_page(0);
    }

    pub fn last_page(&mut self) {
        self.go_to_page(self.page_count.saturating_sub(1));
    }

    pub fn scroll_down(&mut self, rows: usize) {
        self.global_scroll =
            (self.global_scroll + rows).min(self.total_stripes.saturating_sub(1));
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.global_scroll = self.global_scroll.saturating_sub(rows);
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom + 0.25).min(5.0);
        self.dirty = true;
    }

    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom - 0.25).max(0.25);
        self.dirty = true;
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    fn recompute_geometry(&mut self, document: &Document) -> Result<()> {
        let font_height = self.picker.font_size().1 as u32;
        self.page_stripe_counts.clear();
        self.cumulative_stripes.clear();
        let mut cumulative = 0;
        for i in 0..self.page_count {
            self.cumulative_stripes.push(cumulative);
            let count = compute_stripe_count(document, i, self.zoom, font_height)?;
            self.page_stripe_counts.push(count);
            cumulative += count;
        }
        self.total_stripes = cumulative;
        self.rendered_pages.clear();
        Ok(())
    }

    pub fn update_image(
        &mut self,
        document: &Document,
        link_state: Option<&LinkState>,
    ) -> Result<()> {
        let cache_key = (self.zoom * 100.0) as u32;

        if self.dirty || self.last_key != cache_key {
            self.recompute_geometry(document)?;
            self.dirty = false;
            self.last_key = cache_key;
            self.global_scroll = self.global_scroll.min(self.total_stripes.saturating_sub(1));
            self.last_link_overlay = None;
        }

        // Determine current link overlay state
        let link_overlay = link_state.and_then(|ls| {
            if ls.active {
                Some((ls.page, ls.selected))
            } else {
                None
            }
        });

        // If link overlay changed, rebuild stripes for the affected page
        let link_changed = link_overlay != self.last_link_overlay;
        if link_changed {
            // Remove the old overlay page's stripes so they get rebuilt
            if let Some((old_page, _)) = self.last_link_overlay {
                self.rendered_pages.remove(&old_page);
            }
            // Remove the new overlay page's stripes too
            if let Some((new_page, _)) = link_overlay {
                self.rendered_pages.remove(&new_page);
            }
            self.last_link_overlay = link_overlay;
        }

        let current = self.current_page();
        let window_start = current.saturating_sub(1);
        let window_end = (current + 2).min(self.page_count);

        // Evict pages outside the window
        self.rendered_pages
            .retain(|&page_idx, _| page_idx >= window_start && page_idx < window_end);

        // Render pages in the window that aren't yet rendered
        let font_height = self.picker.font_size().1 as u32;
        for page_idx in window_start..window_end {
            if self.rendered_pages.contains_key(&page_idx) {
                continue;
            }

            let img = if let Some(cached) = self.cache.get(page_idx, cache_key) {
                cached.clone()
            } else {
                let rendered = render_page_dpi(document, page_idx, self.zoom)?;
                self.cache.insert(page_idx, cache_key, rendered.clone());
                rendered
            };

            // If this page has link highlights, composite them onto the image
            let img = if let (Some(ls), Some((lp, sel))) = (link_state, link_overlay) {
                if page_idx == lp {
                    let highlights: Vec<HighlightRect> = ls
                        .links
                        .iter()
                        .enumerate()
                        .map(|(i, link)| {
                            let is_selected = i == sel;
                            HighlightRect {
                                x0: link.x0,
                                y0: link.y0,
                                x1: link.x1,
                                y1: link.y1,
                                color: if is_selected {
                                    [255, 220, 50]
                                } else {
                                    [100, 140, 255]
                                },
                                alpha: if is_selected { 0.45 } else { 0.3 },
                            }
                        })
                        .collect();
                    overlay_highlights(&img, self.zoom, &highlights)
                } else {
                    img
                }
            } else {
                img
            };

            let stripe_images = split_into_stripes(&img, font_height);
            let protocols: Vec<StatefulProtocol> = stripe_images
                .into_iter()
                .map(|s| self.picker.new_resize_protocol(s))
                .collect();
            self.rendered_pages.insert(page_idx, protocols);
        }

        Ok(())
    }
}

pub struct PdfWidget;

impl StatefulWidget for PdfWidget {
    type State = PdfViewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if state.total_stripes == 0 {
            return;
        }

        let visible_rows = area.height as usize;
        let global_start = state.global_scroll;
        let global_end = (global_start + visible_rows).min(state.total_stripes);

        let mut screen_row = 0usize;
        let mut g = global_start;

        while g < global_end {
            let page_idx = match state.cumulative_stripes.binary_search(&g) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };

            let page_base = state.cumulative_stripes[page_idx];
            let local_stripe = g - page_base;

            let stripes_left_in_page = state.page_stripe_counts[page_idx] - local_stripe;
            let rows_left_on_screen = global_end - g;
            let count = stripes_left_in_page.min(rows_left_on_screen);

            if let Some(page_stripes) = state.rendered_pages.get_mut(&page_idx) {
                for offset in 0..count {
                    let stripe_local = local_stripe + offset;
                    if stripe_local < page_stripes.len() {
                        let row_rect = Rect {
                            x: area.x,
                            y: area.y + screen_row as u16,
                            width: area.width,
                            height: 1,
                        };
                        StatefulImage::default().render(
                            row_rect,
                            buf,
                            &mut page_stripes[stripe_local],
                        );
                    }
                    screen_row += 1;
                }
            } else {
                screen_row += count;
            }

            g += count;
        }
    }
}

pub struct StatusBar<'a> {
    pub state: &'a PdfViewState,
    pub link_state: Option<&'a LinkState>,
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let is_link_mode = self.link_state.map_or(false, |ls| ls.active);
        let has_back = self
            .link_state
            .map_or(false, |ls| !ls.back_stack.is_empty());

        let page_info = if is_link_mode {
            format!(
                " LINK MODE | Page {}/{} | j/k: select link | Enter: follow | Esc: cancel ",
                self.state.current_page() + 1,
                self.state.page_count(),
            )
        } else {
            let back_hint = if has_back { " | b: back" } else { "" };
            format!(
                " Page {}/{} | Zoom: {:.0}% | j/k: scroll | n/p: page | g: goto | +/-: zoom | l: links | t: toc{} | q: quit ",
                self.state.current_page() + 1,
                self.state.page_count(),
                self.state.zoom * 100.0,
                back_hint,
            )
        };

        let style = if is_link_mode {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        };

        let line = Line::from(vec![Span::styled(page_info, style)]);

        Paragraph::new(line)
            .style(if is_link_mode {
                Style::default().bg(Color::Yellow)
            } else {
                Style::default().bg(Color::DarkGray)
            })
            .render(area, buf);
    }
}
