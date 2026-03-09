use std::collections::HashMap;

use image::{DynamicImage, RgbImage};
use mupdf::Matrix;

use crate::document::Document;
use crate::error::Result;

const MAX_CACHE_ENTRIES: usize = 10;
/// PDF points are 1/72 inch. Scale to this DPI for rendering.
const DEFAULT_DPI: f32 = 192.0;

pub fn render_page(document: &Document, page_index: usize, target_width: u32) -> Result<DynamicImage> {
    let page = document.page(page_index)?;
    let bounds = page.bounds()?;

    let scale = target_width as f32 / bounds.width();
    let matrix = Matrix::new_scale(scale, scale);

    pixmap_to_image(&page, &matrix)
}

pub fn render_page_dpi(document: &Document, page_index: usize, zoom: f32) -> Result<DynamicImage> {
    let page = document.page(page_index)?;
    let scale = (DEFAULT_DPI / 72.0) * zoom;
    let matrix = Matrix::new_scale(scale, scale);

    pixmap_to_image(&page, &matrix)
}

/// Compute how many stripes a page will produce without rendering it.
pub fn compute_stripe_count(document: &Document, page_index: usize, zoom: f32, font_height: u32) -> Result<usize> {
    let (_, h) = document.page_size(page_index)?;
    let scale = (DEFAULT_DPI / 72.0) * zoom;
    let pixel_height = (h * scale) as u32;
    let count = (pixel_height + font_height - 1) / font_height;
    Ok(count as usize)
}

/// Split an image into horizontal stripes, each `stripe_height` pixels tall.
pub fn split_into_stripes(img: &DynamicImage, stripe_height: u32) -> Vec<DynamicImage> {
    let width = img.width();
    let height = img.height();
    let mut stripes = Vec::new();
    let mut y = 0;
    while y < height {
        let h = stripe_height.min(height - y);
        let stripe = img.crop_imm(0, y, width, h);
        stripes.push(stripe);
        y += h;
    }
    stripes
}

fn pixmap_to_image(page: &mupdf::Page, matrix: &Matrix) -> Result<DynamicImage> {
    // alpha=0.0 renders onto an opaque white background (RGB, 3 bytes/pixel)
    let pixmap = page.to_pixmap(matrix, &mupdf::Colorspace::device_rgb(), 0.0, true)?;

    let width = pixmap.width() as u32;
    let height = pixmap.height() as u32;
    let samples = pixmap.samples().to_vec();

    let img = RgbImage::from_raw(width, height, samples)
        .expect("pixmap dimensions should match sample buffer");

    Ok(DynamicImage::ImageRgb8(img))
}

pub struct PageCache {
    entries: HashMap<(usize, u32), DynamicImage>,
}

impl PageCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn get(&self, page_index: usize, key: u32) -> Option<&DynamicImage> {
        self.entries.get(&(page_index, key))
    }

    pub fn insert(&mut self, page_index: usize, key: u32, image: DynamicImage) {
        if self.entries.len() >= MAX_CACHE_ENTRIES {
            self.entries.clear();
        }
        self.entries.insert((page_index, key), image);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Default for PageCache {
    fn default() -> Self {
        Self::new()
    }
}
