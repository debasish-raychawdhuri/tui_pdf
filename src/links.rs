use crate::content::ContentSource;
use crate::error::Result;

const DEFAULT_DPI: f32 = 192.0;

/// A resolved internal link on a page.
#[derive(Debug, Clone)]
pub struct PageLink {
    /// Bounding box in PDF points (x0, y0, x1, y1).
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Target page (0-indexed).
    pub target_page: usize,
    /// Display label (truncated URI or auto-generated).
    pub label: String,
}

/// A saved scroll position for the back-stack.
#[derive(Debug, Clone)]
pub struct ScrollPosition {
    pub global_scroll: usize,
}

/// Extract internal links from a page.
pub fn extract_links(source: &ContentSource, page_index: usize) -> Result<Vec<PageLink>> {
    let links = source.page_links(page_index);
    let mut result = Vec::new();
    let page_count = source.page_count();

    for (i, link) in links.iter().enumerate() {
        let target = match &link.dest {
            Some(d) => d.loc.page_number as usize,
            None => continue,
        };
        // Only keep internal links within the document
        if target < page_count {
            result.push(PageLink {
                x0: link.bounds.x0,
                y0: link.bounds.y0,
                x1: link.bounds.x1,
                y1: link.bounds.y1,
                target_page: target,
                label: if link.uri.is_empty() {
                    format!("Link {}", i + 1)
                } else {
                    link.uri.clone()
                },
            });
        }
    }
    Ok(result)
}

/// Convert a PDF y-coordinate to a stripe (terminal row) index within a page.
pub fn pdf_y_to_row(y: f32, zoom: f32, font_height: u32) -> usize {
    let scale = (DEFAULT_DPI / 72.0) * zoom;
    let pixel_y = (y * scale) as u32;
    (pixel_y / font_height) as usize
}

/// Convert a PDF x-coordinate to a terminal column index.
pub fn pdf_x_to_col(x: f32, zoom: f32, font_width: u32) -> usize {
    let scale = (DEFAULT_DPI / 72.0) * zoom;
    let pixel_x = (x * scale) as u32;
    (pixel_x / font_width) as usize
}

pub struct LinkState {
    /// Whether link mode is active.
    pub active: bool,
    /// Links on the current page.
    pub links: Vec<PageLink>,
    /// Currently selected link index.
    pub selected: usize,
    /// The page these links belong to.
    pub page: usize,
    /// Navigation back-stack.
    pub back_stack: Vec<ScrollPosition>,
}

impl LinkState {
    pub fn new() -> Self {
        Self {
            active: false,
            links: Vec::new(),
            selected: 0,
            page: usize::MAX,
            back_stack: Vec::new(),
        }
    }

    /// Load links for a page if not already loaded.
    pub fn load_for_page(&mut self, source: &ContentSource, page: usize) -> Result<()> {
        if self.page == page {
            return Ok(());
        }
        self.links = extract_links(source, page)?;
        self.selected = 0;
        self.page = page;
        Ok(())
    }

    pub fn activate(&mut self, source: &ContentSource, page: usize) -> Result<()> {
        self.load_for_page(source, page)?;
        if !self.links.is_empty() {
            self.active = true;
            self.selected = 0;
        }
        Ok(())
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }

    pub fn next(&mut self) {
        if !self.links.is_empty() {
            self.selected = (self.selected + 1).min(self.links.len() - 1);
        }
    }

    pub fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_link(&self) -> Option<&PageLink> {
        self.links.get(self.selected)
    }

    pub fn push_position(&mut self, global_scroll: usize) {
        self.back_stack.push(ScrollPosition { global_scroll });
    }

    pub fn pop_position(&mut self) -> Option<ScrollPosition> {
        self.back_stack.pop()
    }

    pub fn has_links(&self) -> bool {
        !self.links.is_empty()
    }
}

impl Default for LinkState {
    fn default() -> Self {
        Self::new()
    }
}
