use std::collections::HashMap;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use mupdf::Matrix;

use crate::document::Document;
use crate::error::Result;

/// Maximum total bytes for the stripe PNG cache (1 GB).
const MAX_CACHE_BYTES: usize = 100 * 1024 * 1024;

/// PDF points are 1/72 inch. Scale to this DPI for rendering.
pub const DEFAULT_DPI: f32 = 192.0;

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
/// The last stripe is padded to full height with white so page boundaries
/// render consistently across documents.
pub fn split_into_stripes(img: &DynamicImage, stripe_height: u32) -> Vec<DynamicImage> {
    let width = img.width();
    let height = img.height();
    let mut stripes = Vec::new();
    let mut y = 0;
    while y < height {
        let h = stripe_height.min(height - y);
        let stripe = img.crop_imm(0, y, width, h);
        if h < stripe_height {
            // Pad short last stripe to full height, sampling the background
            // color from the bottom-left pixel so it works with inverted mode
            let bg = *img.to_rgb8().get_pixel(0, height - 1);
            let mut padded = RgbImage::from_pixel(width, stripe_height, bg);
            image::imageops::overlay(&mut padded, &stripe.to_rgb8(), 0, 0);
            stripes.push(DynamicImage::ImageRgb8(padded));
        } else {
            stripes.push(stripe);
        }
        y += h;
    }
    stripes
}

/// Encode a DynamicImage as PNG bytes.
pub fn encode_png(img: &DynamicImage) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png)
        .expect("PNG encoding should not fail");
    buf.into_inner()
}

/// Decode PNG bytes back to a DynamicImage.
pub fn decode_png(data: &[u8]) -> DynamicImage {
    image::load_from_memory_with_format(data, ImageFormat::Png)
        .expect("cached PNG should be valid")
}

/// Scale an already-rendered image by zoom (relative to DEFAULT_DPI baseline).
/// At zoom=1.0 the image is returned as-is; other values resize proportionally.
pub fn render_image_dpi(img: &DynamicImage, zoom: f32) -> DynamicImage {
    if (zoom - 1.0).abs() < 0.001 {
        return img.clone();
    }
    let new_w = (img.width() as f32 * zoom) as u32;
    let new_h = (img.height() as f32 * zoom) as u32;
    img.resize_exact(new_w.max(1), new_h.max(1), image::imageops::FilterType::Lanczos3)
}

/// Compute stripe count from an image's pixel dimensions (for web content).
pub fn compute_stripe_count_from_image(img: &DynamicImage, zoom: f32, font_height: u32) -> usize {
    let pixel_height = (img.height() as f32 * zoom) as u32;
    let count = (pixel_height + font_height - 1) / font_height;
    count as usize
}

fn pixmap_to_image(page: &mupdf::Page, matrix: &Matrix) -> Result<DynamicImage> {
    // alpha=0.0 renders onto an opaque white background (RGB, 3 bytes/pixel)
    let pixmap = page.to_pixmap(matrix, &mupdf::Colorspace::device_rgb(), false, true)?;

    let width = pixmap.width() as u32;
    let height = pixmap.height() as u32;
    let samples = pixmap.samples().to_vec();

    let img = RgbImage::from_raw(width, height, samples)
        .expect("pixmap dimensions should match sample buffer");

    Ok(DynamicImage::ImageRgb8(img))
}

/// A rectangle to highlight on the image, in PDF point coordinates.
pub struct HighlightRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub color: [u8; 3],
    pub alpha: f32, // 0.0 = transparent, 1.0 = opaque
}

/// Draw semi-transparent highlight rectangles onto a copy of the image.
/// Coordinates are in PDF points, scaled by zoom.
pub fn overlay_highlights(img: &DynamicImage, zoom: f32, highlights: &[HighlightRect]) -> DynamicImage {
    let mut rgb = img.to_rgb8();
    let scale = (DEFAULT_DPI / 72.0) * zoom;
    let (w, h) = (rgb.width(), rgb.height());

    for hl in highlights {
        let px0 = ((hl.x0 * scale) as u32).min(w);
        let py0 = ((hl.y0 * scale) as u32).min(h);
        let px1 = ((hl.x1 * scale) as u32).min(w);
        let py1 = ((hl.y1 * scale) as u32).min(h);

        let a = hl.alpha;
        let inv_a = 1.0 - a;

        for y in py0..py1 {
            for x in px0..px1 {
                let pixel = rgb.get_pixel(x, y);
                let blended = Rgb([
                    (pixel[0] as f32 * inv_a + hl.color[0] as f32 * a) as u8,
                    (pixel[1] as f32 * inv_a + hl.color[1] as f32 * a) as u8,
                    (pixel[2] as f32 * inv_a + hl.color[2] as f32 * a) as u8,
                ]);
                rgb.put_pixel(x, y, blended);
            }
        }
    }

    DynamicImage::ImageRgb8(rgb)
}

/// Cache of stripe PNG bytes, keyed by (page_index, zoom_key).
/// Evicts oldest entries (by insertion order) when total bytes exceed the limit.
pub struct StripeCache {
    /// page_index, zoom_key -> list of PNG-encoded stripes
    entries: HashMap<(usize, u32), Vec<Vec<u8>>>,
    /// Insertion order for LRU-style eviction
    order: Vec<(usize, u32)>,
    /// Total bytes across all cached PNGs
    total_bytes: usize,
}

impl StripeCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            total_bytes: 0,
        }
    }

    pub fn get(&self, page_index: usize, key: u32) -> Option<&Vec<Vec<u8>>> {
        self.entries.get(&(page_index, key))
    }

    pub fn insert(&mut self, page_index: usize, key: u32, stripe_pngs: Vec<Vec<u8>>) {
        let entry_bytes: usize = stripe_pngs.iter().map(|p| p.len()).sum();

        // Evict oldest entries until we have room
        while self.total_bytes + entry_bytes > MAX_CACHE_BYTES && !self.order.is_empty() {
            let oldest_key = self.order.remove(0);
            if let Some(removed) = self.entries.remove(&oldest_key) {
                let removed_bytes: usize = removed.iter().map(|p| p.len()).sum();
                self.total_bytes -= removed_bytes;
            }
        }

        self.entries.insert((page_index, key), stripe_pngs);
        self.order.push((page_index, key));
        self.total_bytes += entry_bytes;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.total_bytes = 0;
    }
}

impl Default for StripeCache {
    fn default() -> Self {
        Self::new()
    }
}
