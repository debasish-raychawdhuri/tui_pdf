use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, StatefulWidget, Widget};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

use crate::content::ContentSource;
use crate::error::Result;
use crate::links::LinkState;
use crate::renderer::{
    decode_png, encode_png, overlay_highlights,
    split_into_stripes, HighlightRect, StripeCache,
};
use crate::search::SearchState;

/// Number of blank terminal rows inserted between consecutive pages.
const PAGE_GAP: usize = 1;

/// 5x7 bitmap font for digits 0-9.
const DIGIT_W: u32 = 5;
const DIGIT_H: u32 = 7;
const DIGIT_SCALE: u32 = 3;
const BADGE_PAD: u32 = 5;

#[rustfmt::skip]
const DIGITS: [[u8; 35]; 10] = [
    [0,1,1,1,0, 1,0,0,0,1, 1,0,0,1,1, 1,0,1,0,1, 1,1,0,0,1, 1,0,0,0,1, 0,1,1,1,0],
    [0,0,1,0,0, 0,1,1,0,0, 0,0,1,0,0, 0,0,1,0,0, 0,0,1,0,0, 0,0,1,0,0, 0,1,1,1,0],
    [0,1,1,1,0, 1,0,0,0,1, 0,0,0,0,1, 0,0,0,1,0, 0,0,1,0,0, 0,1,0,0,0, 1,1,1,1,1],
    [0,1,1,1,0, 1,0,0,0,1, 0,0,0,0,1, 0,0,1,1,0, 0,0,0,0,1, 1,0,0,0,1, 0,1,1,1,0],
    [0,0,0,1,0, 0,0,1,1,0, 0,1,0,1,0, 1,0,0,1,0, 1,1,1,1,1, 0,0,0,1,0, 0,0,0,1,0],
    [1,1,1,1,1, 1,0,0,0,0, 1,1,1,1,0, 0,0,0,0,1, 0,0,0,0,1, 1,0,0,0,1, 0,1,1,1,0],
    [0,1,1,1,0, 1,0,0,0,0, 1,1,1,1,0, 1,0,0,0,1, 1,0,0,0,1, 1,0,0,0,1, 0,1,1,1,0],
    [1,1,1,1,1, 0,0,0,0,1, 0,0,0,1,0, 0,0,1,0,0, 0,0,1,0,0, 0,0,1,0,0, 0,0,1,0,0],
    [0,1,1,1,0, 1,0,0,0,1, 1,0,0,0,1, 0,1,1,1,0, 1,0,0,0,1, 1,0,0,0,1, 0,1,1,1,0],
    [0,1,1,1,0, 1,0,0,0,1, 1,0,0,0,1, 0,1,1,1,1, 0,0,0,0,1, 0,0,0,0,1, 0,1,1,1,0],
];

fn draw_number_badge(img: &mut image::RgbaImage, cx: i32, cy: i32, number: usize) {
    let text = number.to_string();
    let num_digits = text.len() as u32;
    let scaled_w = DIGIT_W * DIGIT_SCALE;
    let scaled_h = DIGIT_H * DIGIT_SCALE;
    let gap = DIGIT_SCALE;
    let badge_w = num_digits * scaled_w + num_digits.saturating_sub(1) * gap + BADGE_PAD * 2;
    let badge_h = scaled_h + BADGE_PAD * 2;
    let bx = cx - badge_w as i32 / 2;
    let by = cy - badge_h as i32 / 2;
    let (iw, ih) = (img.width() as i32, img.height() as i32);

    // Background + border
    for dy in 0..badge_h as i32 {
        for dx in 0..badge_w as i32 {
            let px = bx + dx;
            let py = by + dy;
            if px >= 0 && px < iw && py >= 0 && py < ih {
                let is_border = dx == 0 || dy == 0
                    || dx == badge_w as i32 - 1
                    || dy == badge_h as i32 - 1;
                let color = if is_border {
                    image::Rgba([40, 40, 40, 255])
                } else {
                    image::Rgba([255, 220, 50, 240])
                };
                img.put_pixel(px as u32, py as u32, color);
            }
        }
    }

    // Digits
    let mut digit_x = bx + BADGE_PAD as i32;
    for ch in text.chars() {
        let d = (ch as u8 - b'0') as usize;
        if d < 10 {
            let bitmap = &DIGITS[d];
            for row in 0..DIGIT_H {
                for col in 0..DIGIT_W {
                    if bitmap[(row * DIGIT_W + col) as usize] != 0 {
                        for sy in 0..DIGIT_SCALE {
                            for sx in 0..DIGIT_SCALE {
                                let px = digit_x + (col * DIGIT_SCALE + sx) as i32;
                                let py = by + BADGE_PAD as i32 + (row * DIGIT_SCALE + sy) as i32;
                                if px >= 0 && px < iw && py >= 0 && py < ih {
                                    img.put_pixel(
                                        px as u32,
                                        py as u32,
                                        image::Rgba([20, 20, 20, 255]),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        digit_x += (scaled_w + gap) as i32;
    }
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
    /// Stripes that currently have highlight overlays: (page, stripe_index)
    dirty_highlight_stripes: Vec<(usize, usize)>,

    // Pre-render: next page to render, renders outward from start
    prerender_queue: Vec<usize>,
    prerender_pos: usize,

    inverted: bool,

    /// Last render area for terminal-to-PDF coordinate conversion.
    pub last_render_area: Option<(u16, u16, u16, u16)>,

    /// Stripes modified by probe markers: (page, stripe_index)
    probe_dirty_stripes: Vec<(usize, usize)>,

    /// Terminal area width in columns (for centering oversized pages)
    render_cols: u16,
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
            dirty_highlight_stripes: Vec::new(),
            prerender_queue: Vec::new(),
            prerender_pos: 0,
            inverted: false,
            last_render_area: None,
            probe_dirty_stripes: Vec::new(),
            render_cols: 0,
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

    pub fn zoom_in(&mut self, source: &ContentSource) {
        self.zoom = (self.zoom + 0.25).min(5.0);
        self.on_zoom_change(source);
    }

    pub fn zoom_out(&mut self, source: &ContentSource) {
        self.zoom = (self.zoom - 0.25).max(0.25);
        self.on_zoom_change(source);
    }

    pub fn fit_width(&mut self, source: &ContentSource) {
        let (_, _, aw, _) = self.last_render_area.unwrap_or((0, 0, 80, 24));
        let font_width = self.picker.font_size().0 as f32;
        let terminal_px = aw as f32 * font_width;
        let page = self.current_page();
        if let Ok((page_w, _)) = source.page_size(page) {
            let dpi_scale = crate::renderer::DEFAULT_DPI / 72.0;
            let new_zoom = terminal_px / (page_w * dpi_scale);
            self.zoom = new_zoom.clamp(0.25, 5.0);
            self.on_zoom_change(source);
        }
    }

    fn cache_key(&self) -> u32 {
        let k = (self.zoom * 100.0) as u32;
        if self.inverted { k | (1 << 31) } else { k }
    }

    pub fn inverted(&self) -> bool {
        self.inverted
    }

    pub fn toggle_invert(&mut self, source: &ContentSource) {
        self.inverted = !self.inverted;
        self.last_link_overlay = None;
        self.last_search_overlay = None;
        self.rendered_pages.clear();
        self.dirty_highlight_stripes.clear();
        let _ = self.initial_render(source);
    }

    fn on_zoom_change(&mut self, source: &ContentSource) {
        let _ = self.recompute_geometry(source);
        self.global_scroll = self.global_scroll.min(self.total_stripes.saturating_sub(1));
        self.last_link_overlay = None;
        self.last_search_overlay = None;
        self.rendered_pages.clear();
        self.dirty_highlight_stripes.clear();
        // Re-render visible pages immediately at new zoom
        let _ = self.initial_render(source);
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// Reset state after the document has been reloaded (e.g. file changed on disk).
    pub fn on_reload(&mut self, source: &ContentSource) {
        self.page_count = source.page_count();
        let _ = self.recompute_geometry(source);
        self.rendered_pages.clear();
        self.cache = StripeCache::new();
        self.dirty_highlight_stripes.clear();
        self.prerender_queue.clear();
        self.prerender_pos = 0;
        self.last_link_overlay = None;
        self.last_search_overlay = None;
        self.global_scroll = self.global_scroll.min(self.total_stripes.saturating_sub(1));
    }

    /// Returns (page_0indexed, pdf_y_points) for the current scroll position.
    pub fn current_pdf_position(&self) -> (usize, f32) {
        let page = self.current_page();
        if page >= self.cumulative_stripes.len() {
            return (0, 0.0);
        }
        let page_base = self.cumulative_stripes[page];
        let page_stripes = self.page_stripe_counts.get(page).copied().unwrap_or(0);
        let local_stripe = self.global_scroll.saturating_sub(page_base).min(page_stripes.saturating_sub(1));
        let font_height = self.picker.font_size().1 as f32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;
        let pdf_y = (local_stripe as f32 * font_height) / scale;
        (page, pdf_y)
    }

    /// Returns the page width in PDF points for the given page.
    pub fn page_width_points(&self, page: usize) -> f32 {
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;
        self.page_pixel_widths
            .get(page)
            .copied()
            .unwrap_or(0) as f32
            / scale
    }

    /// Convert a terminal (row, col) position to PDF (page, x, y) coordinates.
    pub fn terminal_to_pdf(&self, term_row: u16, term_col: u16) -> Option<(usize, f32, f32)> {
        let (ax, ay, aw, _ah) = self.last_render_area?;
        if term_col < ax || term_row < ay {
            return None;
        }
        let screen_row = (term_row - ay) as usize;
        let g = self.global_scroll + screen_row;
        if g >= self.total_stripes {
            return None;
        }

        let page_idx = match self.cumulative_stripes.binary_search(&g) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };

        let page_base = self.cumulative_stripes[page_idx];
        let page_stripes = self.page_stripe_counts.get(page_idx).copied().unwrap_or(0);
        let local_stripe = (g - page_base).min(page_stripes.saturating_sub(1));

        let font_height = self.picker.font_size().1 as f32;
        let font_width = self.picker.font_size().0 as f32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;

        let pw = self.page_pixel_widths.get(page_idx).copied().unwrap_or(0) as u16;
        let img_cols = if font_width > 0.0 { (pw + font_width as u16 - 1) / font_width as u16 } else { aw };
        let x_offset = if img_cols < aw { (aw - img_cols) / 2 } else { 0 };

        let col_in_page = (term_col - ax).saturating_sub(x_offset);
        let pdf_x = (col_in_page as f32 * font_width) / scale;
        let pdf_y = (local_stripe as f32 * font_height) / scale;

        Some((page_idx, pdf_x, pdf_y))
    }

    /// Convert PDF (page, x, y) coordinates to terminal (row, col).
    /// Returns None if the position is not currently visible on screen.
    pub fn pdf_to_terminal(&self, page: usize, pdf_x: f32, pdf_y: f32) -> Option<(u16, u16)> {
        let (ax, ay, aw, ah) = self.last_render_area?;
        if page >= self.cumulative_stripes.len() {
            return None;
        }

        let font_height = self.picker.font_size().1 as f32;
        let font_width = self.picker.font_size().0 as f32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;

        // PDF y → stripe within page
        let stripe_in_page = (pdf_y * scale / font_height) as usize;
        let page_base = self.cumulative_stripes[page];
        let global_stripe = page_base + stripe_in_page;

        // Check if visible
        if global_stripe < self.global_scroll || global_stripe >= self.global_scroll + ah as usize {
            return None;
        }
        let screen_row = (global_stripe - self.global_scroll) as u16;

        // PDF x → column
        let pw = self.page_pixel_widths.get(page).copied().unwrap_or(0) as u16;
        let img_cols = if font_width > 0.0 { (pw + font_width as u16 - 1) / font_width as u16 } else { aw };
        let x_offset = if img_cols < aw { (aw - img_cols) / 2 } else { 0 };
        let col_in_page = (pdf_x * scale / font_width) as u16;
        let term_col = ax + x_offset + col_in_page;

        Some((ay + screen_row, term_col))
    }

    fn recompute_geometry(&mut self, source: &ContentSource) -> Result<()> {
        let font_height = self.picker.font_size().1 as u32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;
        self.page_stripe_counts.clear();
        self.page_pixel_widths.clear();
        self.cumulative_stripes.clear();
        let mut cumulative = 0;
        for i in 0..self.page_count {
            if i > 0 {
                cumulative += PAGE_GAP;
            }
            self.cumulative_stripes.push(cumulative);
            let count = source.compute_stripe_count(i, self.zoom, font_height)?;
            self.page_stripe_counts.push(count);
            let (w, _) = source.page_size(i)?;
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
    /// Limited to a window around center to avoid thrashing the cache.
    fn build_prerender_queue(&mut self, center_page: usize) {
        self.prerender_queue.clear();
        self.prerender_pos = 0;

        // Pre-render up to 30 pages around center (fits comfortably in cache)
        const MAX_PRERENDER: usize = 30;

        let center = center_page.min(self.page_count.saturating_sub(1));
        let mut left = center as isize;
        let mut right = center as isize + 1;
        // Add center first
        self.prerender_queue.push(center);

        while self.prerender_queue.len() < MAX_PRERENDER {
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
    /// If the stripe is wider than the terminal, crop it to center horizontally.
    fn build_protocol(&self, img: image::DynamicImage) -> Option<Protocol> {
        let font_size = self.picker.font_size();
        let area_pixel_width = self.render_cols as u32 * font_size.0 as u32;
        let img = if area_pixel_width > 0 && img.width() > area_pixel_width {
            let crop_left = (img.width() - area_pixel_width) / 2;
            img.crop_imm(crop_left, 0, area_pixel_width, img.height())
        } else {
            img
        };
        let w = (img.width() as f32 / font_size.0 as f32).ceil() as u16;
        let h = (img.height() as f32 / font_size.1 as f32).ceil() as u16;
        let size = Rect::new(0, 0, w, h);
        self.picker.new_protocol(img, size, Resize::Crop(None)).ok()
    }

    /// Build protocols for a single page from cached PNGs.
    fn build_page_protocols(&mut self, page_idx: usize) {
        let cache_key = self.cache_key();
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
    pub fn prerender_tick(&mut self, source: &ContentSource) -> bool {
        let cache_key = self.cache_key();
        let font_height = self.picker.font_size().1 as u32;
        if font_height == 0 || self.prerender_pos >= self.prerender_queue.len() {
            return false;
        }

        let page_idx = self.prerender_queue[self.prerender_pos];
        self.prerender_pos += 1;

        // Build stripe PNGs if not cached
        if self.cache.get(page_idx, cache_key).is_none() {
            if let Ok(mut img) = source.render_page_dpi(page_idx, self.zoom) {
                if self.inverted {
                    img.invert();
                }
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
    pub fn initial_render(&mut self, source: &ContentSource) -> Result<()> {
        let cache_key = self.cache_key();

        if self.last_key != cache_key {
            self.recompute_geometry(source)?;
            self.last_key = cache_key;
            self.global_scroll = self.global_scroll.min(self.total_stripes.saturating_sub(1));
        }

        let font_height = self.picker.font_size().1 as u32;
        let current = self.current_page();

        // Render current page and next page immediately (PNGs + protocols)
        for page_idx in current..(current + 2).min(self.page_count) {
            if self.cache.get(page_idx, cache_key).is_none() {
                let mut img = source.render_page_dpi(page_idx, self.zoom)?;
                if self.inverted {
                    img.invert();
                }
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

    /// Ensure the current visible pages are rendered and cached.
    /// Call before draw to avoid blank pages when scrolling to uncached regions.
    pub fn ensure_visible_rendered(&mut self, source: &ContentSource) {
        let cache_key = self.cache_key();
        let font_height = self.picker.font_size().1 as u32;
        if font_height == 0 {
            return;
        }
        let current = self.current_page();
        let mut refocus = false;
        for page_idx in current..(current + 2).min(self.page_count) {
            if self.cache.get(page_idx, cache_key).is_none() {
                if let Ok(mut img) = source.render_page_dpi(page_idx, self.zoom) {
                    if self.inverted {
                        img.invert();
                    }
                    let stripe_images = split_into_stripes(&img, font_height);
                    let stripe_pngs: Vec<Vec<u8>> = stripe_images.iter().map(encode_png).collect();
                    self.cache.insert(page_idx, cache_key, stripe_pngs);
                    refocus = true;
                }
            }
        }
        if refocus {
            self.build_prerender_queue(current);
        }
    }

    /// Display path: build protocols from cached stripe PNGs only.
    /// Never calls MuPDF. Pages not yet in cache are skipped (blank).
    pub fn update_image(
        &mut self,
        link_state: Option<&LinkState>,
        search_state: Option<&SearchState>,
        area_cols: u16,
    ) -> Result<()> {
        // Invalidate protocols if terminal width changed (affects centering crop)
        if area_cols != self.render_cols {
            self.render_cols = area_cols;
            self.rendered_pages.clear();
        }
        let cache_key = self.cache_key();

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
                self.dirty_highlight_stripes.retain(|(p, _)| *p != old_page);
            }
            if let Some((new_page, _)) = link_overlay {
                self.rendered_pages.remove(&new_page);
                self.dirty_highlight_stripes.retain(|(p, _)| *p != new_page);
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
                    self.dirty_highlight_stripes.clear();
                }
                (Some((old_q, _)), Some((new_q, _))) if old_q != new_q => {
                    self.rendered_pages.clear();
                    self.dirty_highlight_stripes.clear();
                }
                _ => {
                    // Same query, just moved current index — handled by per-stripe rebuild below
                }
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

        // Restore previously highlighted stripes to their base (unhighlighted) versions
        let font_height = self.picker.font_size().1 as u32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;

        if link_changed || search_changed {
            let old_dirty = std::mem::take(&mut self.dirty_highlight_stripes);
            for (page_idx, stripe_idx) in old_dirty {
                if let Some(stripe_pngs) = self.cache.get(page_idx, cache_key) {
                    if let Some(png) = stripe_pngs.get(stripe_idx) {
                        if let Some(proto) = self.build_protocol(decode_png(png)) {
                            if let Some(page_protos) = self.rendered_pages.get_mut(&page_idx) {
                                if stripe_idx < page_protos.len() {
                                    page_protos[stripe_idx] = proto;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Build highlight overlays — only rebuild individual stripes that overlap highlights
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

            if highlights.is_empty() {
                continue;
            }

            // Determine which stripe indices are touched by any highlight
            let stripe_count = self.page_stripe_counts.get(page_idx).copied().unwrap_or(0);
            let mut dirty_stripes = vec![false; stripe_count];
            for hl in &highlights {
                let s0 = ((hl.y0 * scale) as u32 / font_height.max(1)) as usize;
                let s1 = (((hl.y1 * scale) as u32 + font_height - 1) / font_height.max(1)) as usize;
                for s in s0..s1.min(stripe_count) {
                    dirty_stripes[s] = true;
                    self.dirty_highlight_stripes.push((page_idx, s));
                }
            }

            // Only rebuild the dirty stripes, keep the rest from base protocols
            if let Some(stripe_pngs) = self.cache.get(page_idx, cache_key) {
                // Ensure we have base protocols for this page
                let has_protocols = self.rendered_pages.contains_key(&page_idx)
                    && self.rendered_pages[&page_idx].len() == stripe_count;

                if !has_protocols {
                    // Build full base protocols first
                    let protocols: Vec<Protocol> = stripe_pngs
                        .iter()
                        .filter_map(|png| self.build_protocol(decode_png(png)))
                        .collect();
                    self.rendered_pages.insert(page_idx, protocols);
                }

                // Now selectively rebuild only dirty stripes
                for (s, is_dirty) in dirty_stripes.iter().enumerate() {
                    if !*is_dirty {
                        continue;
                    }
                    if s >= stripe_pngs.len() {
                        break;
                    }
                    // Decode this one stripe, overlay highlights onto it, re-encode protocol
                    let base_stripe = decode_png(&stripe_pngs[s]);
                    let stripe_y_offset = (s as u32 * font_height) as f32 / scale;
                    let stripe_h = font_height as f32 / scale;

                    // Shift highlight coords relative to this stripe
                    let local_highlights: Vec<HighlightRect> = highlights
                        .iter()
                        .filter(|hl| hl.y1 > stripe_y_offset && hl.y0 < stripe_y_offset + stripe_h)
                        .map(|hl| HighlightRect {
                            x0: hl.x0,
                            y0: (hl.y0 - stripe_y_offset).max(0.0),
                            x1: hl.x1,
                            y1: (hl.y1 - stripe_y_offset).min(stripe_h),
                            color: hl.color,
                            alpha: hl.alpha,
                        })
                        .collect();

                    let highlighted = overlay_highlights(&base_stripe, self.zoom, &local_highlights);
                    if let Some(proto) = self.build_protocol(highlighted) {
                        if let Some(page_protos) = self.rendered_pages.get_mut(&page_idx) {
                            if s < page_protos.len() {
                                page_protos[s] = proto;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Draw numbered markers into the rendered stripe protocols.
    /// Each marker is (page_0indexed, pdf_x, pdf_y, number).
    /// Call once when entering probe mode. Call `clear_probe_markers` to undo.
    pub fn apply_probe_markers(&mut self, markers: &[(usize, f32, f32, usize)]) {
        let cache_key = self.cache_key();
        let font_height = self.picker.font_size().1 as u32;
        let scale = (crate::renderer::DEFAULT_DPI / 72.0) * self.zoom;

        self.probe_dirty_stripes.clear();

        // Group markers by page
        let mut by_page: HashMap<usize, Vec<(f32, f32, usize)>> = HashMap::new();
        for &(page, pdf_x, pdf_y, number) in markers {
            by_page.entry(page).or_default().push((pdf_x, pdf_y, number));
        }

        for (page_idx, page_markers) in &by_page {
            let stripe_count = self.page_stripe_counts.get(*page_idx).copied().unwrap_or(0);

            // Ensure base protocols exist
            self.build_page_protocols(*page_idx);

            // Group markers by stripe — each marker may span multiple stripes
            let scaled_h = DIGIT_H * DIGIT_SCALE;
            let badge_h = scaled_h + BADGE_PAD * 2;
            let half_h = badge_h as i32 / 2 + 1;
            let mut stripe_markers: HashMap<usize, Vec<(i32, i32, usize)>> = HashMap::new();
            for &(pdf_x, pdf_y, number) in page_markers {
                let pixel_x = (pdf_x * scale).round() as i32;
                let pixel_y = (pdf_y * scale).round() as i32;
                let top_y = pixel_y - half_h;
                let bot_y = pixel_y + half_h;
                let s0 = (top_y.max(0) as u32 / font_height.max(1)) as usize;
                let s1 = (bot_y.max(0) as u32 / font_height.max(1)) as usize;
                for s in s0..=s1.min(stripe_count.saturating_sub(1)) {
                    let local_y = pixel_y - (s as i32 * font_height as i32);
                    stripe_markers
                        .entry(s)
                        .or_default()
                        .push((pixel_x, local_y, number));
                }
            }

            // Now borrow cache for the stripe PNGs
            let stripe_pngs = match self.cache.get(*page_idx, cache_key) {
                Some(p) => p,
                None => continue,
            };

            for (stripe_idx, smarkers) in &stripe_markers {
                if *stripe_idx >= stripe_pngs.len() {
                    continue;
                }
                let base = decode_png(&stripe_pngs[*stripe_idx]);
                let mut rgba = base.to_rgba8();
                for &(x, y, num) in smarkers {
                    draw_number_badge(&mut rgba, x, y, num);
                }
                let modified = image::DynamicImage::ImageRgba8(rgba);
                if let Some(proto) = self.build_protocol(modified) {
                    if let Some(page_protos) = self.rendered_pages.get_mut(page_idx) {
                        if *stripe_idx < page_protos.len() {
                            page_protos[*stripe_idx] = proto;
                            self.probe_dirty_stripes.push((*page_idx, *stripe_idx));
                        }
                    }
                }
            }
        }
    }

    /// Restore stripes that were modified by probe markers back to clean cache versions.
    pub fn clear_probe_markers(&mut self) {
        let cache_key = self.cache_key();
        let dirty = std::mem::take(&mut self.probe_dirty_stripes);
        for (page_idx, stripe_idx) in dirty {
            if let Some(stripe_pngs) = self.cache.get(page_idx, cache_key) {
                if let Some(png) = stripe_pngs.get(stripe_idx) {
                    if let Some(proto) = self.build_protocol(decode_png(png)) {
                        if let Some(page_protos) = self.rendered_pages.get_mut(&page_idx) {
                            if stripe_idx < page_protos.len() {
                                page_protos[stripe_idx] = proto;
                            }
                        }
                    }
                }
            }
        }
    }
}

pub struct PdfWidget;

impl StatefulWidget for PdfWidget {
    type State = PdfViewState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        state.last_render_area = Some((area.x, area.y, area.width, area.height));
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
            let page_end = page_base + state.page_stripe_counts[page_idx];

            // If g is past the page's stripes, we're in the gap region
            if g >= page_end {
                let next_page_start = if page_idx + 1 < state.cumulative_stripes.len() {
                    state.cumulative_stripes[page_idx + 1]
                } else {
                    state.total_stripes
                };
                let gap_remaining = next_page_start.saturating_sub(g);
                let gap_rows = gap_remaining.min(global_end - g);
                // Leave gap rows as default background (blank)
                screen_row += gap_rows;
                g += gap_rows;
                continue;
            }

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
                " Page {}/{} | Zoom: {:.0}%{} | j/k: scroll | n/p: page | g: goto | /: search | +/-: zoom | w: fit | i: invert | l: links | t: toc | s: synctex | o: zotero | Tab/d: switch | x: close{} | q: quit ",
                self.state.current_page() + 1,
                self.state.page_count(),
                self.state.zoom * 100.0,
                if self.state.inverted() { " [INV]" } else { "" },
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
