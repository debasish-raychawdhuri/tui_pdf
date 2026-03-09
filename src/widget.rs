use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, StatefulWidget, Widget};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

use crate::document::Document;
use crate::error::Result;
use crate::links::LinkState;
use crate::renderer::{
    compute_stripe_count, decode_png, encode_png, overlay_highlights, render_page_dpi,
    split_into_stripes, HighlightRect, StripeCache,
};
use crate::search::SearchState;

/// Reassemble a full image from stripe PNG bytes.
fn reassemble_stripes(stripe_pngs: &[Vec<u8>]) -> image::DynamicImage {
    let stripes: Vec<image::DynamicImage> = stripe_pngs.iter().map(|p| decode_png(p)).collect();
    if stripes.is_empty() {
        return image::DynamicImage::ImageRgb8(image::RgbImage::new(1, 1));
    }
    let width = stripes[0].width();
    let total_height: u32 = stripes.iter().map(|s| s.height()).sum();
    let mut combined = image::RgbImage::new(width, total_height);
    let mut y = 0;
    for stripe in &stripes {
        let rgb = stripe.to_rgb8();
        for sy in 0..rgb.height() {
            for sx in 0..rgb.width().min(width) {
                combined.put_pixel(sx, y + sy, *rgb.get_pixel(sx, sy));
            }
        }
        y += rgb.height();
    }
    image::DynamicImage::ImageRgb8(combined)
}

pub struct PdfViewState {
    pub global_scroll: usize,
    pub zoom: f32,
    page_count: usize,
    picker: Picker,

    // Geometry: stripe count per page, prefix sums for global offset
    page_stripe_counts: Vec<usize>,
    page_pixel_widths: Vec<u32>,
    cumulative_stripes: Vec<usize>,
    total_stripes: usize,

    // Protocol cache for display: page_index -> stripe protocols (pre-encoded, no resize on render)
    rendered_pages: HashMap<usize, Vec<Protocol>>,

    // The one true cache: stripe PNGs
    cache: StripeCache,

    last_key: u32,
    last_link_overlay: Option<(usize, usize)>,
    last_search_overlay: Option<(String, usize)>,

    // Pre-render: next page to render, renders outward from start
    prerender_queue: Vec<usize>,
    prerender_pos: usize,
}

impl PdfViewState {
    pub fn new(page_count: usize, picker: Picker) -> Self {
        Self {
            global_scroll: 0,
            zoom: 1.0,
            page_count,
            picker,
            page_stripe_counts: Vec::new(),
            page_pixel_widths: Vec::new(),
            cumulative_stripes: Vec::new(),
            total_stripes: 0,
            rendered_pages: HashMap::new(),
            cache: StripeCache::new(),
            last_key: 0,
            last_link_overlay: None,
            last_search_overlay: None,
            prerender_queue: Vec::new(),
            prerender_pos: 0,
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

    pub fn scroll_to_point(&mut self, page: usize, pdf_y: f32) {
        if page >= self.page_count || self.cumulative_stripes.is_empty() {
            return;
        }
        let font_height = self.picker.font_size().1 as u32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;
        let pixel_y = (pdf_y * scale) as u32;
        let stripe = (pixel_y / font_height) as usize;
        let target = self.cumulative_stripes[page] + stripe;
        self.global_scroll = target.saturating_sub(3).min(self.total_stripes.saturating_sub(1));
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

    pub fn zoom_in(&mut self, document: &Document) {
        self.zoom = (self.zoom + 0.25).min(5.0);
        self.on_zoom_change(document);
    }

    pub fn zoom_out(&mut self, document: &Document) {
        self.zoom = (self.zoom - 0.25).max(0.25);
        self.on_zoom_change(document);
    }

    fn on_zoom_change(&mut self, document: &Document) {
        let _ = self.recompute_geometry(document);
        self.global_scroll = self.global_scroll.min(self.total_stripes.saturating_sub(1));
        self.last_link_overlay = None;
        self.last_search_overlay = None;
        self.rendered_pages.clear();
        // Re-render visible pages immediately at new zoom
        let _ = self.initial_render(document);
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    fn recompute_geometry(&mut self, document: &Document) -> Result<()> {
        let font_height = self.picker.font_size().1 as u32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;
        self.page_stripe_counts.clear();
        self.page_pixel_widths.clear();
        self.cumulative_stripes.clear();
        let mut cumulative = 0;
        for i in 0..self.page_count {
            self.cumulative_stripes.push(cumulative);
            let count = compute_stripe_count(document, i, self.zoom, font_height)?;
            self.page_stripe_counts.push(count);
            let (w, _) = document.page_size(i)?;
            self.page_pixel_widths.push((w * scale) as u32);
            cumulative += count;
        }
        self.total_stripes = cumulative;
        self.rendered_pages.clear();
        Ok(())
    }

    fn page_pixel_size(&self, page_idx: usize) -> (u32, u32) {
        let w = self.page_pixel_widths.get(page_idx).copied().unwrap_or(0);
        let font_height = self.picker.font_size().1 as u32;
        let h = self.page_stripe_counts.get(page_idx).copied().unwrap_or(0) as u32 * font_height;
        (w, h)
    }

    /// Build a pre-render queue that spirals outward from `center_page`.
    fn build_prerender_queue(&mut self, center_page: usize) {
        self.prerender_queue.clear();
        self.prerender_pos = 0;

        let center = center_page.min(self.page_count.saturating_sub(1));
        let mut left = center as isize;
        let mut right = center as isize + 1;
        // Add center first
        self.prerender_queue.push(center);

        loop {
            let mut added = false;
            if left > 0 {
                left -= 1;
                self.prerender_queue.push(left as usize);
                added = true;
            }
            if (right as usize) < self.page_count {
                self.prerender_queue.push(right as usize);
                right += 1;
                added = true;
            }
            if !added {
                break;
            }
        }
    }

    /// Build a pre-encoded Protocol from a DynamicImage stripe.
    fn build_protocol(&self, img: image::DynamicImage) -> Option<Protocol> {
        let font_size = self.picker.font_size();
        let w = (img.width() as f32 / font_size.0 as f32).ceil() as u16;
        let h = (img.height() as f32 / font_size.1 as f32).ceil() as u16;
        let size = Rect::new(0, 0, w, h);
        self.picker.new_protocol(img, size, Resize::Crop(None)).ok()
    }

    /// Build protocols for a single page from cached PNGs.
    fn build_page_protocols(&mut self, page_idx: usize) {
        let cache_key = (self.zoom * 100.0) as u32;
        if self.rendered_pages.contains_key(&page_idx) {
            return;
        }
        if let Some(stripe_pngs) = self.cache.get(page_idx, cache_key) {
            let protocols: Vec<Protocol> = stripe_pngs
                .iter()
                .filter_map(|png| self.build_protocol(decode_png(png)))
                .collect();
            self.rendered_pages.insert(page_idx, protocols);
        }
    }

    /// Render one page from the pre-render queue into stripe PNG cache.
    /// Does NOT build protocols — those are built on-demand in update_image.
    /// Returns true if there is more work to do.
    pub fn prerender_tick(&mut self, document: &Document) -> bool {
        let cache_key = (self.zoom * 100.0) as u32;
        let font_height = self.picker.font_size().1 as u32;
        if font_height == 0 || self.prerender_pos >= self.prerender_queue.len() {
            return false;
        }

        let page_idx = self.prerender_queue[self.prerender_pos];
        self.prerender_pos += 1;

        // Build stripe PNGs if not cached
        if self.cache.get(page_idx, cache_key).is_none() {
            if let Ok(img) = render_page_dpi(document, page_idx, self.zoom) {
                let stripe_images = split_into_stripes(&img, font_height);
                let stripe_pngs: Vec<Vec<u8>> = stripe_images.iter().map(encode_png).collect();
                self.cache.insert(page_idx, cache_key, stripe_pngs);
            }
        }

        self.prerender_pos < self.prerender_queue.len()
    }

    /// Whether pre-rendering is complete.
    pub fn prerender_done(&self) -> bool {
        self.prerender_pos >= self.prerender_queue.len()
    }

    /// Refocus pre-render around the user's current page.
    /// Called when the user scrolls to an area with uncached pages.
    pub fn refocus_prerender(&mut self) {
        let current = self.current_page();
        self.build_prerender_queue(current);
    }

    /// Initial setup: render the first visible pages immediately, then queue the rest.
    pub fn initial_render(&mut self, document: &Document) -> Result<()> {
        let cache_key = (self.zoom * 100.0) as u32;

        if self.last_key != cache_key {
            self.recompute_geometry(document)?;
            self.last_key = cache_key;
            self.global_scroll = self.global_scroll.min(self.total_stripes.saturating_sub(1));
        }

        let font_height = self.picker.font_size().1 as u32;
        let current = self.current_page();

        // Render current page and next page immediately (PNGs + protocols)
        for page_idx in current..(current + 2).min(self.page_count) {
            if self.cache.get(page_idx, cache_key).is_none() {
                let img = render_page_dpi(document, page_idx, self.zoom)?;
                let stripe_images = split_into_stripes(&img, font_height);
                let stripe_pngs: Vec<Vec<u8>> = stripe_images.iter().map(encode_png).collect();
                self.cache.insert(page_idx, cache_key, stripe_pngs);
            }
            if !self.rendered_pages.contains_key(&page_idx) {
                if let Some(pngs) = self.cache.get(page_idx, cache_key) {
                    let protocols: Vec<Protocol> = pngs
                        .iter()
                        .filter_map(|png| self.build_protocol(decode_png(png)))
                        .collect();
                    self.rendered_pages.insert(page_idx, protocols);
                }
            }
        }

        // Queue the rest, spiraling outward from current page
        self.build_prerender_queue(current);

        Ok(())
    }

    /// Display path: build protocols from cached stripe PNGs only.
    /// Never calls MuPDF. Pages not yet in cache are skipped (blank).
    pub fn update_image(
        &mut self,
        link_state: Option<&LinkState>,
        search_state: Option<&SearchState>,
    ) -> Result<()> {
        let cache_key = (self.zoom * 100.0) as u32;

        // Determine current link overlay state
        let link_overlay = link_state.and_then(|ls| {
            if ls.active {
                Some((ls.page, ls.selected))
            } else {
                None
            }
        });

        let link_changed = link_overlay != self.last_link_overlay;
        if link_changed {
            if let Some((old_page, _)) = self.last_link_overlay {
                self.rendered_pages.remove(&old_page);
            }
            if let Some((new_page, _)) = link_overlay {
                self.rendered_pages.remove(&new_page);
            }
            self.last_link_overlay = link_overlay;
        }

        // Determine current search overlay state
        let search_overlay = search_state.and_then(|ss| {
            if ss.active {
                Some((ss.query.clone(), ss.current))
            } else {
                None
            }
        });

        let search_changed = search_overlay != self.last_search_overlay;
        if search_changed {
            match (&self.last_search_overlay, &search_overlay) {
                (None, Some(_)) | (Some(_), None) => {
                    self.rendered_pages.clear();
                }
                (Some((old_q, old_idx)), Some((new_q, new_idx))) => {
                    if old_q != new_q {
                        self.rendered_pages.clear();
                    } else if let Some(ss) = search_state {
                        if let Some(old_hit) = ss.hits.get(*old_idx) {
                            self.rendered_pages.remove(&old_hit.page);
                        }
                        if let Some(new_hit) = ss.hits.get(*new_idx) {
                            self.rendered_pages.remove(&new_hit.page);
                        }
                    }
                }
                _ => {}
            }
            self.last_search_overlay = search_overlay.clone();
        }

        let current = self.current_page();
        let window_start = current.saturating_sub(5);
        let window_end = (current + 6).min(self.page_count);

        // If current page isn't cached, refocus pre-render around it
        if self.cache.get(current, cache_key).is_none() {
            self.build_prerender_queue(current);
        }

        // Evict protocols for pages far from the viewport to bound Kitty image memory
        let evict_start = current.saturating_sub(15);
        let evict_end = (current + 16).min(self.page_count);
        self.rendered_pages.retain(|&page, _| page >= evict_start && page < evict_end);

        // Build protocols for pages in the visible window (from cached PNGs)
        for page_idx in window_start..window_end {
            self.build_page_protocols(page_idx);
        }

        // Build highlight overlays for pages that need them
        for page_idx in window_start..window_end {
            // Collect highlights for this page
            let mut highlights: Vec<HighlightRect> = Vec::new();

            if let (Some(ls), Some((lp, sel))) = (link_state, link_overlay) {
                if page_idx == lp {
                    for (i, link) in ls.links.iter().enumerate() {
                        let is_selected = i == sel;
                        highlights.push(HighlightRect {
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
                        });
                    }
                }
            }

            if let Some(ss) = search_state {
                if ss.active {
                    let current_hit = ss.current_hit();
                    for hit in ss.hits_on_page(page_idx) {
                        let is_current = current_hit.map_or(false, |ch| {
                            ch.page == hit.page
                                && (ch.x0 - hit.x0).abs() < 0.1
                                && (ch.y0 - hit.y0).abs() < 0.1
                        });
                        highlights.push(HighlightRect {
                            x0: hit.x0,
                            y0: hit.y0,
                            x1: hit.x1,
                            y1: hit.y1,
                            color: if is_current {
                                [255, 140, 0]
                            } else {
                                [255, 255, 50]
                            },
                            alpha: if is_current { 0.5 } else { 0.3 },
                        });
                    }
                }
            }

            if !highlights.is_empty() {
                // Highlighted page: rebuild protocols from PNGs + overlay
                if let Some(stripe_pngs) = self.cache.get(page_idx, cache_key) {
                    let full_img = reassemble_stripes(stripe_pngs);
                    let highlighted = overlay_highlights(&full_img, self.zoom, &highlights);
                    let font_height = self.picker.font_size().1 as u32;
                    let stripes = split_into_stripes(&highlighted, font_height);
                    let protocols: Vec<Protocol> = stripes
                        .into_iter()
                        .filter_map(|s| self.build_protocol(s))
                        .collect();
                    self.rendered_pages.insert(page_idx, protocols);
                }
            }
            // No highlights + already in rendered_pages = nothing to do (instant)
            // No highlights + not in rendered_pages = blank until prerender catches up
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

            // Compute horizontal centering offset for this page
            let font_width = state.picker.font_size().0 as u16;
            let img_cols = if font_width > 0 {
                let (pw, _) = state.page_pixel_size(page_idx);
                ((pw as u16) + font_width - 1) / font_width
            } else {
                area.width
            };
            let x_offset = if img_cols < area.width {
                (area.width - img_cols) / 2
            } else {
                0
            };

            if let Some(page_stripes) = state.rendered_pages.get_mut(&page_idx) {
                for offset in 0..count {
                    let stripe_local = local_stripe + offset;
                    if stripe_local < page_stripes.len() {
                        let row_rect = Rect {
                            x: area.x + x_offset,
                            y: area.y + screen_row as u16,
                            width: area.width - x_offset,
                            height: 1,
                        };
                        Image::new(&mut page_stripes[stripe_local]).render(
                            row_rect,
                            buf,
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
    pub search_state: Option<&'a SearchState>,
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let is_link_mode = self.link_state.map_or(false, |ls| ls.active);
        let has_back = self
            .link_state
            .map_or(false, |ls| !ls.back_stack.is_empty());
        let has_search = self
            .search_state
            .map_or(false, |ss| ss.active);

        let page_info = if is_link_mode {
            format!(
                " LINK MODE | Page {}/{} | j/k: select link | Enter: follow | Esc: cancel ",
                self.state.current_page() + 1,
                self.state.page_count(),
            )
        } else if has_search {
            let ss = self.search_state.unwrap();
            let progress = if ss.searching {
                format!(" (searching {}/{}...)", ss.pages_searched(), ss.total_pages())
            } else {
                String::new()
            };
            let pos = if ss.hit_count() > 0 {
                format!("{}/{}", ss.current + 1, ss.hit_count())
            } else {
                "0".to_string()
            };
            format!(
                " Search: \"{}\" | {} matches{} | n/p: next/prev | Esc: clear | Page {}/{} ",
                ss.query,
                pos,
                progress,
                self.state.current_page() + 1,
                self.state.page_count(),
            )
        } else {
            let back_hint = if has_back { " | b: back" } else { "" };
            format!(
                " Page {}/{} | Zoom: {:.0}% | j/k: scroll | n/p: page | g: goto | /: search | +/-: zoom | l: links | t: toc{} | q: quit ",
                self.state.current_page() + 1,
                self.state.page_count(),
                self.state.zoom * 100.0,
                back_hint,
            )
        };

        let style = if is_link_mode {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else if has_search {
            Style::default().fg(Color::Black).bg(Color::Green)
        } else {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        };

        let line = Line::from(vec![Span::styled(page_info, style)]);

        let bg = if is_link_mode {
            Style::default().bg(Color::Yellow)
        } else if has_search {
            Style::default().bg(Color::Green)
        } else {
            Style::default().bg(Color::DarkGray)
        };

        Paragraph::new(line).style(bg).render(area, buf);
    }
}
