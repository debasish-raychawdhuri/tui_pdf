use std::path::{Path, PathBuf};

use crate::error::{Result, TuiPdfError};

pub struct Document {
    inner: mupdf::Document,
    page_count: usize,
    path: PathBuf,
    password: Option<String>,
    /// True when `path` is a Markdown file rendered to PDF in memory; reloads
    /// re-render it so editing the source and pressing `r` refreshes the view.
    is_markdown: bool,
    /// Source-line → PDF-position map for Markdown SyncTeX-style search
    /// (empty for real PDFs). Sorted by page then y.
    md_sourcemap: Vec<crate::markdown::SourceLoc>,
}

/// Whether `path` has a Markdown extension we should render rather than open.
fn is_markdown_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("md") | Some("markdown")
    )
}

impl Document {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if is_markdown_path(path) {
            let (bytes, sourcemap) = crate::markdown::render(path)?;
            let inner = mupdf::Document::from_bytes(&bytes, "application/pdf")?;
            let page_count = inner.page_count()?;
            return Ok(Self {
                inner,
                page_count: page_count as usize,
                path: path.to_path_buf(),
                password: None,
                is_markdown: true,
                md_sourcemap: sourcemap,
            });
        }
        let inner = mupdf::Document::open(path.to_str().unwrap_or_default())?;
        let page_count = inner.page_count()?;
        Ok(Self {
            inner,
            page_count: page_count as usize,
            path: path.to_path_buf(),
            password: None,
            is_markdown: false,
            md_sourcemap: Vec::new(),
        })
    }

    pub fn open_with_password(path: impl AsRef<Path>, password: &str) -> Result<Self> {
        let path = path.as_ref();
        let mut inner = mupdf::Document::open(path.to_str().unwrap_or_default())?;
        inner.authenticate(password)?;
        let page_count = inner.page_count()?;
        Ok(Self {
            inner,
            page_count: page_count as usize,
            path: path.to_path_buf(),
            password: Some(password.to_string()),
            is_markdown: false,
            md_sourcemap: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True when this document was rendered from a Markdown source file.
    pub fn is_markdown(&self) -> bool {
        self.is_markdown
    }

    /// All Markdown source positions (for the keyboard reverse-search probe
    /// grid). Empty for real PDFs.
    pub fn md_positions(&self) -> &[crate::markdown::SourceLoc] {
        &self.md_sourcemap
    }

    /// Forward search: map a Markdown source line to a rendered `(page, y)`.
    /// Picks the last block starting at or before `line`; falls back to the
    /// nearest block by line distance. `y` is in PDF points from the page top.
    pub fn md_forward(&self, line: usize) -> Option<(usize, f32)> {
        if self.md_sourcemap.is_empty() {
            return None;
        }
        let at_or_before = self
            .md_sourcemap
            .iter()
            .filter(|l| l.line <= line)
            .max_by_key(|l| l.line);
        let chosen = at_or_before.or_else(|| {
            self.md_sourcemap
                .iter()
                .min_by_key(|l| l.line.abs_diff(line))
        })?;
        Some((chosen.page, chosen.y))
    }

    /// Reverse search: map a clicked `(page, y)` back to a source line. Picks
    /// the last block on that page at or above `y`; falls back to the nearest
    /// block on the page, then to the whole document.
    pub fn md_reverse(&self, page: usize, y: f32) -> Option<usize> {
        if self.md_sourcemap.is_empty() {
            return None;
        }
        let on_page = self.md_sourcemap.iter().filter(|l| l.page == page);
        let at_or_above = on_page
            .clone()
            .filter(|l| l.y <= y + 0.5)
            .max_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
        let chosen = at_or_above
            .or_else(|| {
                on_page.min_by(|a, b| {
                    (a.y - y)
                        .abs()
                        .partial_cmp(&(b.y - y).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            })
            .or_else(|| self.md_sourcemap.first())?;
        Some(chosen.line)
    }

    pub fn reload(&mut self) -> Result<()> {
        if self.is_markdown {
            let (bytes, sourcemap) = crate::markdown::render(&self.path)?;
            let inner = mupdf::Document::from_bytes(&bytes, "application/pdf")?;
            let page_count = inner.page_count()? as usize;
            self.inner = inner;
            self.page_count = page_count;
            self.md_sourcemap = sourcemap;
            return Ok(());
        }
        let path_str = self.path.to_str().unwrap_or_default();
        let mut inner = mupdf::Document::open(path_str)?;
        if let Some(ref pw) = self.password {
            inner.authenticate(pw)?;
        }
        let page_count = inner.page_count()? as usize;
        self.inner = inner;
        self.page_count = page_count;
        Ok(())
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn page(&self, index: usize) -> Result<mupdf::Page> {
        if index >= self.page_count {
            return Err(TuiPdfError::PageOutOfRange(index, self.page_count));
        }
        Ok(self.inner.load_page(index as i32)?)
    }

    pub fn page_size(&self, index: usize) -> Result<(f32, f32)> {
        let page = self.page(index)?;
        let bounds = page.bounds()?;
        Ok((bounds.width(), bounds.height()))
    }

    pub fn outlines(&self) -> Result<Vec<mupdf::Outline>> {
        Ok(self.inner.outlines()?)
    }

    pub fn page_links(&self, index: usize) -> Result<Vec<mupdf::Link>> {
        let page = self.page(index)?;
        Ok(page.links()?.collect())
    }

    /// Search for `needle` on a given page, returning bounding quads for each match.
    pub fn search_page(&self, index: usize, needle: &str, hit_max: u32) -> Result<Vec<mupdf::Quad>> {
        let page = self.page(index)?;
        Ok(page.search(needle, hit_max)?.to_vec())
    }
}
