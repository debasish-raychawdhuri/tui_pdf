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
            let bytes = crate::markdown::render_to_pdf_bytes(path)?;
            let inner = mupdf::Document::from_bytes(&bytes, "application/pdf")?;
            let page_count = inner.page_count()?;
            return Ok(Self {
                inner,
                page_count: page_count as usize,
                path: path.to_path_buf(),
                password: None,
                is_markdown: true,
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
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reload(&mut self) -> Result<()> {
        if self.is_markdown {
            let bytes = crate::markdown::render_to_pdf_bytes(&self.path)?;
            let inner = mupdf::Document::from_bytes(&bytes, "application/pdf")?;
            let page_count = inner.page_count()? as usize;
            self.inner = inner;
            self.page_count = page_count;
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
