use image::DynamicImage;

use crate::document::Document;
use crate::error::{Result, TuiPdfError};
use crate::renderer;
use crate::web::WebContent;

pub enum ContentSource {
    Pdf(Document),
    Web(WebContent),
}

impl ContentSource {
    pub fn page_count(&self) -> usize {
        match self {
            ContentSource::Pdf(doc) => doc.page_count(),
            ContentSource::Web(_) => 1,
        }
    }

    /// Page size in "PDF points". For web content, pixels are treated as points.
    pub fn page_size(&self, index: usize) -> Result<(f32, f32)> {
        match self {
            ContentSource::Pdf(doc) => doc.page_size(index),
            ContentSource::Web(web) => {
                if index != 0 {
                    return Err(TuiPdfError::PageOutOfRange(index, 1));
                }
                // Convert pixels to PDF points (72 DPI). The renderer uses
                // DEFAULT_DPI/72 as scale, so expressing dimensions in points
                // makes zoom behave consistently with PDFs.
                let scale = renderer::DEFAULT_DPI / 72.0;
                Ok((
                    web.image.width() as f32 / scale,
                    web.image.height() as f32 / scale,
                ))
            }
        }
    }

    /// Render a page at the given zoom level.
    pub fn render_page_dpi(&self, index: usize, zoom: f32) -> Result<DynamicImage> {
        match self {
            ContentSource::Pdf(doc) => renderer::render_page_dpi(doc, index, zoom),
            ContentSource::Web(web) => {
                if index != 0 {
                    return Err(TuiPdfError::PageOutOfRange(index, 1));
                }
                Ok(renderer::render_image_dpi(&web.image, zoom))
            }
        }
    }

    /// Compute stripe count for a page without rendering.
    pub fn compute_stripe_count(&self, index: usize, zoom: f32, font_height: u32) -> Result<usize> {
        match self {
            ContentSource::Pdf(doc) => renderer::compute_stripe_count(doc, index, zoom, font_height),
            ContentSource::Web(web) => {
                if index != 0 {
                    return Err(TuiPdfError::PageOutOfRange(index, 1));
                }
                Ok(renderer::compute_stripe_count_from_image(&web.image, zoom, font_height))
            }
        }
    }

    pub fn path_or_url(&self) -> &str {
        match self {
            ContentSource::Pdf(doc) => doc.path().to_str().unwrap_or_default(),
            ContentSource::Web(web) => &web.url,
        }
    }

    pub fn reload(&mut self) -> Result<()> {
        match self {
            ContentSource::Pdf(doc) => doc.reload(),
            ContentSource::Web(web) => {
                let new = crate::web::capture_url(&web.url)?;
                web.image = new.image;
                Ok(())
            }
        }
    }

    pub fn outlines(&self) -> Vec<mupdf::Outline> {
        match self {
            ContentSource::Pdf(doc) => doc.outlines().unwrap_or_default(),
            ContentSource::Web(_) => Vec::new(),
        }
    }

    pub fn page_links(&self, index: usize) -> Vec<mupdf::Link> {
        match self {
            ContentSource::Pdf(doc) => doc.page_links(index).unwrap_or_default(),
            ContentSource::Web(_) => Vec::new(),
        }
    }

    pub fn search_page(&self, index: usize, needle: &str, hit_max: u32) -> Vec<mupdf::Quad> {
        match self {
            ContentSource::Pdf(doc) => doc.search_page(index, needle, hit_max).unwrap_or_default(),
            ContentSource::Web(_) => Vec::new(),
        }
    }

    pub fn is_pdf(&self) -> bool {
        matches!(self, ContentSource::Pdf(_))
    }

    pub fn is_web(&self) -> bool {
        matches!(self, ContentSource::Web(_))
    }

    /// Access the underlying Document (PDF only).
    pub fn as_document(&self) -> Option<&Document> {
        match self {
            ContentSource::Pdf(doc) => Some(doc),
            ContentSource::Web(_) => None,
        }
    }
}
