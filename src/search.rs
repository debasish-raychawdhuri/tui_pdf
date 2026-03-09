use crate::document::Document;
use crate::error::Result;

/// Pages to search per frame tick to keep the UI responsive.
const PAGES_PER_TICK: usize = 20;

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
    /// All hits found so far.
    pub hits: Vec<SearchHit>,
    /// Index of the currently focused hit.
    pub current: usize,
    /// Whether search highlights should be shown.
    pub active: bool,
    /// Ordered list of page indices to search.
    search_order: Vec<usize>,
    /// How many pages from search_order we've processed.
    pages_done: usize,
    /// Whether the search is still in progress.
    pub searching: bool,
    /// Whether we've jumped to the first result yet.
    pub jumped: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            hits: Vec::new(),
            current: 0,
            active: false,
            search_order: Vec::new(),
            pages_done: 0,
            searching: false,
            jumped: false,
        }
    }

    /// Start a new search. Does not block — call `search_tick()` each frame.
    pub fn start_search(&mut self, query: &str, page_count: usize, start_page: usize) {
        self.query = query.to_string();
        self.hits.clear();
        self.current = 0;
        self.jumped = false;

        if query.is_empty() {
            self.active = false;
            self.searching = false;
            return;
        }

        // Build search order: start_page, start_page+1, ..., end, 0, 1, ..., start_page-1
        self.search_order = (0..page_count)
            .map(|i| (start_page + i) % page_count)
            .collect();
        self.pages_done = 0;
        self.searching = true;
        self.active = true;
    }

    /// Search a batch of pages. Returns true if new hits were found this tick.
    pub fn search_tick(&mut self, document: &Document) -> Result<bool> {
        if !self.searching {
            return Ok(false);
        }

        let mut found_new = false;
        let end = (self.pages_done + PAGES_PER_TICK).min(self.search_order.len());

        for i in self.pages_done..end {
            let page_idx = self.search_order[i];
            let quads = document.search_page(page_idx, &self.query, 100)?;
            for quad in quads {
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
                found_new = true;
            }
        }

        self.pages_done = end;
        if self.pages_done >= self.search_order.len() {
            self.searching = false;
        }

        if found_new {
            // Keep hits sorted by page for consistent navigation
            self.hits.sort_by(|a, b| {
                a.page
                    .cmp(&b.page)
                    .then_with(|| a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal))
                    .then_with(|| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal))
            });
        }

        if self.hits.is_empty() && !self.searching {
            self.active = false;
        }

        Ok(found_new)
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
        if let Some(idx) = self.hits.iter().position(|h| h.page >= page) {
            self.current = idx;
        } else {
            self.current = 0;
        }
    }

    /// Get the current hit, if any.
    pub fn current_hit(&self) -> Option<&SearchHit> {
        self.hits.get(self.current)
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
        self.searching = false;
        self.jumped = false;
    }

    pub fn hit_count(&self) -> usize {
        self.hits.len()
    }

    pub fn total_pages(&self) -> usize {
        self.search_order.len()
    }

    pub fn pages_searched(&self) -> usize {
        self.pages_done
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}
