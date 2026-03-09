use crate::document::Document;
use crate::error::Result;

/// A single search hit: page index + bounding box in PDF points.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub page: usize,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

pub struct SearchState {
    /// The current search query.
    pub query: String,
    /// All hits across the document.
    pub hits: Vec<SearchHit>,
    /// Index of the currently focused hit.
    pub current: usize,
    /// Whether search highlights should be shown.
    pub active: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            hits: Vec::new(),
            current: 0,
            active: false,
        }
    }

    /// Run search across all pages of the document.
    pub fn search(&mut self, document: &Document, query: &str) -> Result<()> {
        self.query = query.to_string();
        self.hits.clear();
        self.current = 0;

        if query.is_empty() {
            self.active = false;
            return Ok(());
        }

        for page_idx in 0..document.page_count() {
            let quads = document.search_page(page_idx, query, 100)?;
            for quad in quads {
                // Convert quad to axis-aligned bounding box
                let x0 = quad.ul.x.min(quad.ll.x);
                let y0 = quad.ul.y.min(quad.ur.y);
                let x1 = quad.ur.x.max(quad.lr.x);
                let y1 = quad.ll.y.max(quad.lr.y);
                self.hits.push(SearchHit {
                    page: page_idx,
                    x0,
                    y0,
                    x1,
                    y1,
                });
            }
        }

        self.active = !self.hits.is_empty();
        Ok(())
    }

    /// Navigate to the next hit.
    pub fn next_hit(&mut self) {
        if !self.hits.is_empty() {
            self.current = (self.current + 1) % self.hits.len();
        }
    }

    /// Navigate to the previous hit.
    pub fn prev_hit(&mut self) {
        if !self.hits.is_empty() {
            self.current = (self.current + self.hits.len() - 1) % self.hits.len();
        }
    }

    /// Jump to the next hit on or after the given page.
    pub fn next_hit_from_page(&mut self, page: usize) {
        if self.hits.is_empty() {
            return;
        }
        // Find first hit on or after `page`
        if let Some(idx) = self.hits.iter().position(|h| h.page >= page) {
            self.current = idx;
        } else {
            // Wrap around to start
            self.current = 0;
        }
    }

    /// Get the current hit, if any.
    pub fn current_hit(&self) -> Option<&SearchHit> {
        self.hits.get(self.current)
    }

    /// Get the page of the current hit.
    pub fn current_page(&self) -> Option<usize> {
        self.current_hit().map(|h| h.page)
    }

    /// Get all hits on a specific page.
    pub fn hits_on_page(&self, page: usize) -> Vec<&SearchHit> {
        self.hits.iter().filter(|h| h.page == page).collect()
    }

    /// Clear the search.
    pub fn clear(&mut self) {
        self.query.clear();
        self.hits.clear();
        self.current = 0;
        self.active = false;
    }

    pub fn hit_count(&self) -> usize {
        self.hits.len()
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}
