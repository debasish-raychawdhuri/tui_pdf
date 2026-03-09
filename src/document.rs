use std::path::Path;

use crate::error::{Result, TuiPdfError};

pub struct Document {
    inner: mupdf::Document,
    page_count: usize,
}

impl Document {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let inner = mupdf::Document::open(path.to_str().unwrap_or_default())?;
        let page_count = inner.page_count()?;
        Ok(Self {
            inner,
            page_count: page_count as usize,
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
        })
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
        Ok(page.search(needle, hit_max)?)
    }
}
