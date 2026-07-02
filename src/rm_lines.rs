//! Reading reMarkable "v6" `.rm` annotation files and turning their pen strokes
//! into vector overlays expressed in PDF-point coordinates.
//!
//! The device never modifies the PDF; handwritten strokes live in separate
//! `<page-uuid>.rm` files (one per annotated page) under a `<doc-uuid>/`
//! directory. Each `.rm` file is a sequence of tagged binary blocks (the v6
//! format introduced with firmware 3.0). We parse only what's needed to draw
//! the ink: per-stroke point lists, colour and thickness.
//!
//! Strokes are stored in PDF points (top-left origin, y-down) so the viewer can
//! rasterise them exactly like [`crate::renderer::overlay_highlights`] does for
//! search rectangles: `pixel = point * (DEFAULT_DPI / 72.0) * zoom`.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::annotations_dir;

/// A single pen stroke in PDF-point coordinates (top-left origin, y-down).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stroke {
    /// 0-based original PDF page index this stroke overlays.
    pub page: usize,
    /// Polyline vertices in PDF points.
    pub pts: Vec<(f32, f32)>,
    /// Stroke colour (RGB).
    pub color: [u8; 3],
    /// Stroke width in PDF points.
    pub width_pt: f32,
    /// 0.0 = transparent, 1.0 = opaque (highlighter uses < 1.0).
    pub alpha: f32,
}

/// A stroke stored on disk in reMarkable canvas coordinates, with its target
/// PDF page already resolved. Kept raw (not pre-transformed to PDF points) so
/// the canvas→page transform can be re-derived at view time — if the device
/// width is ever wrong it's a one-number fix with no re-sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPageStroke {
    /// 0-based page (in the rendered document) this stroke overlays.
    pub page: usize,
    /// Polyline vertices in reMarkable canvas px (x centered, y from the top).
    pub pts: Vec<(f32, f32)>,
    pub color_id: u32,
    pub tool_id: u32,
    pub thickness: f32,
    /// True if this page has no PDF backing (notebook / inserted page) → the
    /// strokes are fit to their bounding box rather than registered fit-to-width.
    #[serde(default)]
    pub backing_less: bool,
}

/// All annotations pulled from one device document, keyed on disk by its
/// reMarkable UUID.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocAnnotations {
    /// Device `lastModified` (ms) at pull time — used to skip re-pulling
    /// unchanged annotations on the next sync.
    pub last_modified_ms: i64,
    /// The device's stroke-canvas width in px (framebuffer width). reMarkable
    /// stores strokes in this pixel space and fits PDFs to it; the transform
    /// needs it and it differs by model, so we record what the device reported.
    pub device_width: f32,
    /// For a PDF opened under "Adjust view" (`zoomMode: customFit`), the width in
    /// canvas px that a page spans at that custom zoom (`.content`'s
    /// `customZoomPageWidth`). reMarkable stores strokes against this span, not
    /// the device width, so the transform must normalize by it — otherwise a
    /// zoomed-in note renders too big and shifted. `None` for normal fit-to-width.
    #[serde(default)]
    pub custom_zoom_page_width: Option<f32>,
    /// Strokes in canvas coordinates, page already resolved. Each stroke carries
    /// its own `backing_less` flag (a merged document mixes PDF-backed pages and
    /// blank inserted pages).
    pub raw: Vec<RawPageStroke>,
}

impl DocAnnotations {
    /// Transform stored strokes into `Stroke`s in PDF points, grouped by page.
    ///
    /// reMarkable fits a PDF page to the device width, so the scale is derived
    /// from each page's own width (`page_size(page) -> (w_pt, h_pt)`), which
    /// makes this correct for any page size and any device (via `device_width`).
    /// `pdf_x = (raw_x + w/2) * (page_w / span)`, `pdf_y = raw_y * (page_w / span)`.
    /// `span` is the device width normally, or `customZoomPageWidth` when the PDF
    /// was written under "Adjust view" (see [`Self::custom_zoom_page_width`]).
    pub fn strokes_by_page<F>(&self, page_size: F) -> std::collections::HashMap<usize, Vec<Stroke>>
    where
        F: Fn(usize) -> Option<(f32, f32)>,
    {
        // The canvas-px width a page spans. Under "Adjust view" reMarkable fits
        // the page to a custom zoom rather than the device width, and records
        // strokes against that span; normalize by it so zoomed notes land at the
        // right size and place.
        let dw = self
            .custom_zoom_page_width
            .filter(|w| *w > 0.0)
            .unwrap_or(if self.device_width > 0.0 { self.device_width } else { 1404.0 });
        // Group raw strokes by page first (bbox-fit needs a page's full extent).
        let mut by_page: std::collections::HashMap<usize, Vec<&RawPageStroke>> =
            std::collections::HashMap::new();
        for rs in &self.raw {
            if rs.pts.len() >= 2 {
                by_page.entry(rs.page).or_default().push(rs);
            }
        }

        let mut map: std::collections::HashMap<usize, Vec<Stroke>> = std::collections::HashMap::new();
        for (page, strokes) in by_page {
            let (page_w, page_h) = match page_size(page) {
                Some((w, h)) if w > 0.0 && h > 0.0 => (w, h),
                _ => continue,
            };

            // Fit-to-width: raw x centered on the device, y from the top. This is
            // how reMarkable renders every framed page — PDFs and un-panned
            // notebook / inserted pages alike — so a note written at the top of
            // the page lands at the top.
            let fit_to_width = {
                let k = page_w / dw;
                (k, k, dw / 2.0 * k, 0.0)
            };
            let backing_less = strokes.first().map(|s| s.backing_less).unwrap_or(false);
            let (kx, ky, ox, oy) = if backing_less {
                // A blank page (notebook / inserted page) is written in the same
                // device frame as a PDF *unless* the user panned/zoomed the
                // canvas. Measure the strokes' extent: `dh` is this page's height
                // in canvas px once fit to the device width. If the strokes sit
                // inside the frame (x within ±dw/2, y within [0, dh]) the page
                // was not panned, so use fit-to-width — otherwise the content was
                // moved out of frame, so fall back to a bounding-box fit that
                // keeps every stroke on the page rather than clipping it.
                let (mut xmin, mut xmax, mut ymin, mut ymax) =
                    (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for s in &strokes {
                    for &(x, y) in &s.pts {
                        xmin = xmin.min(x); xmax = xmax.max(x);
                        ymin = ymin.min(y); ymax = ymax.max(y);
                    }
                }
                let half = dw / 2.0;
                let dh = page_h * dw / page_w;
                let in_device_frame =
                    xmin >= -half && xmax <= half && ymin >= 0.0 && ymax <= dh;
                if in_device_frame {
                    fit_to_width
                } else {
                    let (bw, bh) = ((xmax - xmin).max(1.0), (ymax - ymin).max(1.0));
                    let margin = 0.04;
                    let k = ((page_w * (1.0 - 2.0 * margin)) / bw)
                        .min((page_h * (1.0 - 2.0 * margin)) / bh);
                    (k, k, (page_w - bw * k) / 2.0 - xmin * k, (page_h - bh * k) / 2.0 - ymin * k)
                }
            } else {
                fit_to_width
            };

            let out = map.entry(page).or_default();
            for rs in strokes {
                let highlighter = is_highlighter(rs.tool_id);
                let color = color_to_rgb(rs.color_id);
                let alpha = if highlighter { 0.35 } else { 1.0 };
                let width_rm = if highlighter {
                    10.0 * rs.thickness.max(1.0)
                } else {
                    1.5 * rs.thickness.max(0.8)
                };
                let width_pt = (width_rm * kx).max(0.4);
                let pts = rs.pts.iter().map(|&(x, y)| (x * kx + ox, y * ky + oy)).collect();
                out.push(Stroke { page, pts, color, width_pt, alpha });
            }
        }
        map
    }
}

/// Points per millimetre (printpdf works in mm).
const PT_PER_MM: f32 = 72.0 / 25.4;

/// Write a blank white PDF of `page_count` pages sized `w_pt` x `h_pt` (points).
/// Used as the backing "page" for a pulled reMarkable notebook, which has no PDF
/// of its own — its strokes are then drawn over it via the annotation overlay,
/// so a notebook opens exactly like any other PDF (cf. how `.md` is rendered to
/// a PDF and opened).
pub fn write_blank_pdf(
    path: &std::path::Path,
    page_count: usize,
    w_pt: f32,
    h_pt: f32,
) -> io::Result<()> {
    use printpdf::{Mm, Op, PdfDocument, PdfPage, PdfSaveOptions};
    let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("notebook");
    let mut doc = PdfDocument::new(title);
    let pages: Vec<PdfPage> = (0..page_count.max(1))
        .map(|_| PdfPage::new(Mm(w_pt / PT_PER_MM), Mm(h_pt / PT_PER_MM), Vec::<Op>::new()))
        .collect();
    doc.with_pages(pages);
    let mut warnings = Vec::new();
    let bytes = doc.save(&PdfSaveOptions::default(), &mut warnings);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// Rebuild a PDF to mirror the reMarkable's page sequence: the original PDF
/// pages with blank pages inserted at `inserted_ordinals` (the positions where
/// the tablet has pages with no PDF backing). Returns the merged PDF bytes.
/// Assumes the backing pages keep their original order (the normal case).
pub fn build_merged_pdf(
    orig_pdf_bytes: &[u8],
    inserted_ordinals: &[usize],
    blank_w_pt: f32,
    blank_h_pt: f32,
) -> io::Result<Vec<u8>> {
    use mupdf::pdf::PdfDocument;
    use std::io::Write as _;

    let mut doc = PdfDocument::from_bytes(orig_pdf_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("open pdf: {e}")))?;
    let mut ords: Vec<usize> = inserted_ordinals.to_vec();
    ords.sort_unstable();
    // Insert ascending: when we reach ordinal N, positions 0..N are already
    // filled (original pages + earlier inserts), so N is the correct index.
    for ord in ords {
        doc.new_page_at(ord as i32, (blank_w_pt, blank_h_pt))
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("insert page: {e}")))?;
    }
    let mut out = Vec::new();
    doc.write_to(&mut out)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("write pdf: {e}")))?;
    let _ = out.flush();
    Ok(out)
}

/// Path to the stored annotations for a device document UUID.
pub fn annotations_path(uuid: &str) -> PathBuf {
    annotations_dir().join(format!("{uuid}.json"))
}

/// Load stored annotations for a document UUID, if any.
pub fn load(uuid: &str) -> Option<DocAnnotations> {
    let text = std::fs::read_to_string(annotations_path(uuid)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist annotations for a document UUID.
pub fn save(uuid: &str, ann: &DocAnnotations) -> io::Result<()> {
    let dir = annotations_dir();
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string(ann)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    std::fs::write(annotations_path(uuid), text)
}

// ---------------------------------------------------------------------------
// Colour / pen mapping (used by the view-time transform in strokes_by_page)
// ---------------------------------------------------------------------------
//
// CALIBRATION NOTE: the geometry lives in `DocAnnotations::strokes_by_page` and
// is derived from data (page width + device framebuffer width) — nothing is
// hardcoded per device model. If strokes are misaligned against a real device,
// the things to check are: (a) that the device reported the right framebuffer
// width, (b) that raw x is centered (x + width/2) vs top-left, (c) fit-to-width
// vs fit-to-height (reMarkable's `.content` zoomMode). The colour/pen tables
// below are the only tweakable constants here.

/// Map a reMarkable pen colour id to RGB. Unknown ids fall back to black.
fn color_to_rgb(color_id: u32) -> [u8; 3] {
    match color_id {
        0 => [0, 0, 0],        // black
        1 => [144, 144, 144],  // gray
        2 => [255, 255, 255],  // white
        3 => [240, 220, 0],    // yellow
        4 => [0, 160, 0],      // green
        5 => [255, 40, 150],   // pink
        6 => [0, 90, 220],     // blue
        7 => [220, 0, 0],      // red
        8 => [144, 144, 144],  // gray (overlap)
        9 => [0, 180, 0],      // green
        10 => [0, 190, 210],   // cyan
        11 => [200, 0, 200],   // magenta
        12 => [240, 220, 0],   // yellow
        _ => [0, 0, 0],
    }
}

/// Highlighter pen ids (translucent, wide). Others draw opaque.
fn is_highlighter(tool_id: u32) -> bool {
    matches!(tool_id, 5 | 18)
}

// ---------------------------------------------------------------------------
// v6 .rm parsing
// ---------------------------------------------------------------------------

/// A stroke as it comes out of a `.rm` file, before the coordinate transform:
/// points are still in reMarkable canvas coordinates.
#[derive(Debug, Clone)]
pub struct RawStroke {
    pub pts: Vec<(f32, f32)>,
    pub color_id: u32,
    pub tool_id: u32,
    /// Base thickness scale from the line item.
    pub thickness: f32,
}

// Tag types packed into the low nibble of a tag varuint (index = value >> 4).
const TAG_ID: u8 = 0xF;
const TAG_LENGTH4: u8 = 0xC;
const TAG_BYTE8: u8 = 0x8;
const TAG_BYTE4: u8 = 0x4;
#[allow(dead_code)] // documents the format; used once per-point width is drawn
const TAG_BYTE1: u8 = 0x1;

const BLOCK_SCENE_LINE_ITEM: u8 = 0x05;
const ITEM_TYPE_LINE: u8 = 0x03;

/// Fixed v6 header. Data begins immediately after it.
const HEADER_PREFIX: &[u8] = b"reMarkable .lines file, version=";
const HEADER_LEN: usize = 43;

/// A little cursor over a byte slice with the v6 primitive readers.
struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cur { b, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(eof());
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    #[allow(dead_code)] // used when per-point width/speed are decoded
    fn u16(&mut self) -> io::Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn f32(&mut self) -> io::Result<f32> {
        let s = self.take(4)?;
        Ok(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn f64(&mut self) -> io::Result<f64> {
        let s = self.take(8)?;
        Ok(f64::from_le_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }
    /// LEB128-style unsigned varint (7 bits/byte, high bit = continue).
    fn varuint(&mut self) -> io::Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            result |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift >= 64 {
                return Err(bad("varuint too long"));
            }
        }
        Ok(result)
    }

    /// Peek the next tag without consuming it: `(field_index, tag_type)`.
    fn peek_tag(&self) -> Option<(u8, u8)> {
        let mut c = Cur { b: self.b, pos: self.pos };
        let v = c.varuint().ok()?;
        Some(((v >> 4) as u8, (v & 0xF) as u8))
    }

    /// Consume a tag, requiring the given field index and type.
    fn expect_tag(&mut self, index: u8, tag_type: u8) -> io::Result<()> {
        let v = self.varuint()?;
        let got_index = (v >> 4) as u8;
        let got_type = (v & 0xF) as u8;
        if got_index != index || got_type != tag_type {
            return Err(bad(&format!(
                "tag mismatch: want ({index},{tag_type}) got ({got_index},{got_type})"
            )));
        }
        Ok(())
    }

    fn read_id(&mut self, index: u8) -> io::Result<()> {
        self.expect_tag(index, TAG_ID)?;
        let _part1 = self.u8()?;
        let _part2 = self.varuint()?;
        Ok(())
    }
    fn read_int(&mut self, index: u8) -> io::Result<u32> {
        self.expect_tag(index, TAG_BYTE4)?;
        self.u32()
    }
    fn read_float(&mut self, index: u8) -> io::Result<f32> {
        self.expect_tag(index, TAG_BYTE4)?;
        self.f32()
    }
    fn read_double(&mut self, index: u8) -> io::Result<f64> {
        self.expect_tag(index, TAG_BYTE8)?;
        self.f64()
    }
    /// Open a length-prefixed subblock, returning a cursor over its contents.
    fn read_subblock(&mut self, index: u8) -> io::Result<Cur<'a>> {
        self.expect_tag(index, TAG_LENGTH4)?;
        let len = self.u32()? as usize;
        let s = self.take(len)?;
        Ok(Cur::new(s))
    }
    fn has_field(&self, index: u8) -> bool {
        matches!(self.peek_tag(), Some((i, _)) if i == index)
    }
}

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of .rm data")
}
fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Parse a v6 `.rm` file into raw strokes (canvas coordinates). Returns an empty
/// vector for a well-formed file with no line items. Malformed individual blocks
/// are skipped (blocks are length-prefixed) rather than aborting the whole file.
pub fn parse_rm_v6(bytes: &[u8]) -> io::Result<Vec<RawStroke>> {
    if bytes.len() < HEADER_LEN || !bytes.starts_with(HEADER_PREFIX) {
        return Err(bad("not a reMarkable .lines file"));
    }
    if bytes[HEADER_PREFIX.len()] != b'6' {
        return Err(bad("unsupported .rm version (need v6)"));
    }

    let mut cur = Cur { b: bytes, pos: HEADER_LEN };
    let mut strokes = Vec::new();

    while cur.remaining() >= 8 {
        let block_len = cur.u32()? as usize;
        let _reserved = cur.u8()?;
        let _min_ver = cur.u8()?;
        let cur_ver = cur.u8()?;
        let block_type = cur.u8()?;
        let content = match cur.take(block_len) {
            Ok(c) => c,
            Err(_) => break, // truncated final block
        };
        if block_type == BLOCK_SCENE_LINE_ITEM {
            if let Ok(Some(s)) = parse_line_block(content, cur_ver) {
                strokes.push(s);
            }
        }
    }
    Ok(strokes)
}

/// Parse one `SceneLineItemBlock`. Returns `Ok(None)` if the item is present but
/// not a line (e.g. deleted/tombstone), `Err` on malformed data.
fn parse_line_block(content: &[u8], version: u8) -> io::Result<Option<RawStroke>> {
    let mut c = Cur::new(content);
    // Scene-item wrapper: parent/item/left/right CRDT ids, then deleted_length.
    c.read_id(1)?;
    c.read_id(2)?;
    c.read_id(3)?;
    c.read_id(4)?;
    let _deleted_length = c.read_int(5)?;
    // The item value lives in optional subblock 6.
    if !c.has_field(6) {
        return Ok(None);
    }
    let mut v = c.read_subblock(6)?;
    let item_type = v.u8()?;
    if item_type != ITEM_TYPE_LINE {
        return Ok(None);
    }

    let tool_id = v.read_int(1)?;
    let color_id = v.read_int(2)?;
    let thickness = v.read_double(3)? as f32;
    let _starting_length = v.read_float(4)?;
    let pts_block = v.read_subblock(5)?;

    // Point size depends on this block's version: v1 = 24 bytes (6× f32),
    // v2 = 14 bytes. We only need x,y (the first two f32 either way).
    let point_size = if version == 1 { 24 } else { 14 };
    let mut pts = Vec::new();
    if point_size > 0 && pts_block.b.len() % point_size == 0 {
        let n = pts_block.b.len() / point_size;
        let mut pc = pts_block;
        for _ in 0..n {
            let x = pc.f32()?;
            let y = pc.f32()?;
            // Skip the remaining per-point fields (speed/width/direction/pressure).
            pc.take(point_size - 8)?;
            pts.push((x, y));
        }
    }

    Ok(Some(RawStroke {
        pts,
        color_id,
        tool_id,
        thickness,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(index: u8, ty: u8) -> u8 {
        (index << 4) | ty
    }

    /// Build a minimal valid v6 file with a single 2-point line item and check
    /// the parser recovers the tool/colour and exact x,y coordinates. This is a
    /// round-trip guard against byte-offset regressions (real-device fixtures
    /// still validate the coordinate transform separately).
    #[test]
    fn parses_single_line_item() {
        // Inner subblock(6) content: item_type + Line fields.
        let mut inner: Vec<u8> = Vec::new();
        inner.push(ITEM_TYPE_LINE);
        inner.push(tag(1, TAG_BYTE4));
        inner.extend_from_slice(&7u32.to_le_bytes()); // tool_id = 7
        inner.push(tag(2, TAG_BYTE4));
        inner.extend_from_slice(&6u32.to_le_bytes()); // color_id = 6 (blue)
        inner.push(tag(3, TAG_BYTE8));
        inner.extend_from_slice(&2.0f64.to_le_bytes()); // thickness
        inner.push(tag(4, TAG_BYTE4));
        inner.extend_from_slice(&0.0f32.to_le_bytes()); // starting_length
        // points subblock(5): two v2 points (14 bytes each).
        let mut pts: Vec<u8> = Vec::new();
        for (x, y) in [(10.0f32, 20.0f32), (30.0f32, 40.0f32)] {
            pts.extend_from_slice(&x.to_le_bytes());
            pts.extend_from_slice(&y.to_le_bytes());
            pts.extend_from_slice(&[0u8; 6]); // speed,width,dir,pressure
        }
        inner.push(tag(5, TAG_LENGTH4));
        inner.extend_from_slice(&(pts.len() as u32).to_le_bytes());
        inner.extend_from_slice(&pts);

        // Scene-item wrapper: 4 CRDT ids + deleted_length, then subblock(6).
        let mut content: Vec<u8> = Vec::new();
        for idx in 1..=4u8 {
            content.push(tag(idx, TAG_ID));
            content.push(0); // part1
            content.push(0); // part2 varuint
        }
        content.push(tag(5, TAG_BYTE4));
        content.extend_from_slice(&0u32.to_le_bytes()); // deleted_length
        content.push(tag(6, TAG_LENGTH4));
        content.extend_from_slice(&(inner.len() as u32).to_le_bytes());
        content.extend_from_slice(&inner);

        // File: header + one block envelope (cur_ver=2 → v2 points).
        let mut file: Vec<u8> = Vec::new();
        let mut header = HEADER_PREFIX.to_vec();
        header.push(b'6');
        while header.len() < HEADER_LEN {
            header.push(b' ');
        }
        file.extend_from_slice(&header);
        file.extend_from_slice(&(content.len() as u32).to_le_bytes());
        file.push(0); // reserved
        file.push(0); // min_ver
        file.push(2); // cur_ver
        file.push(BLOCK_SCENE_LINE_ITEM);
        file.extend_from_slice(&content);

        let strokes = parse_rm_v6(&file).expect("parse");
        assert_eq!(strokes.len(), 1);
        let s = &strokes[0];
        assert_eq!(s.tool_id, 7);
        assert_eq!(s.color_id, 6);
        assert_eq!(s.pts, vec![(10.0, 20.0), (30.0, 40.0)]);
    }

    #[test]
    fn rejects_non_v6() {
        assert!(parse_rm_v6(b"not a remarkable file").is_err());
    }

    /// A notebook note written near the top of the device frame must render near
    /// the top of the page (device-frame fit-to-width), not vertically centered.
    /// Regression guard for the bbox-fit bug that centered sparse top content.
    #[test]
    fn backing_less_top_content_stays_at_top() {
        // Paper Pro: 1620px canvas → 516x688pt device-aspect backing page.
        let (page_w, page_h) = (516.0f32, 688.0f32);
        let doc = DocAnnotations {
            last_modified_ms: 0,
            device_width: 1620.0,
            custom_zoom_page_width: None,
            raw: vec![RawPageStroke {
                page: 0,
                // x centered, y from the top — a short note high on the page.
                pts: vec![(-100.0, 86.0), (100.0, 299.0)],
                color_id: 0,
                tool_id: 2,
                thickness: 2.0,
                backing_less: true,
            }],
        };
        let map = doc.strokes_by_page(|_| Some((page_w, page_h)));
        let strokes = &map[&0];
        let max_y = strokes[0].pts.iter().fold(0.0f32, |m, &(_, y)| m.max(y));
        // With fit-to-width (k = 516/1620 ≈ 0.319), y=299 → ~95pt, well within the
        // top third. The old bbox-fit centered it to ~mid-page (~380pt).
        assert!(max_y < page_h / 3.0, "content should be near the top, got y={max_y}");
        // x=0 (device centre) must map to the page centre.
        let mid_x = strokes[0].pts[0].0.min(strokes[0].pts[1].0)
            + (strokes[0].pts[1].0 - strokes[0].pts[0].0).abs() / 2.0;
        assert!((mid_x - page_w / 2.0).abs() < 1.0, "x should be centred, got {mid_x}");
    }

    /// A panned/zoomed page (content outside the device frame — here negative y)
    /// still uses the bounding-box fit so nothing is clipped off the page.
    #[test]
    fn backing_less_panned_content_uses_bbox_fit() {
        let (page_w, page_h) = (516.0f32, 688.0f32);
        let doc = DocAnnotations {
            last_modified_ms: 0,
            device_width: 1620.0,
            custom_zoom_page_width: None,
            raw: vec![RawPageStroke {
                page: 0,
                // y goes negative → panned out of frame → bbox-fit centres it.
                pts: vec![(-100.0, -500.0), (100.0, -300.0)],
                color_id: 0,
                tool_id: 2,
                thickness: 2.0,
                backing_less: true,
            }],
        };
        let map = doc.strokes_by_page(|_| Some((page_w, page_h)));
        let strokes = &map[&0];
        // bbox-fit centres the content vertically regardless of raw y.
        let min_y = strokes[0].pts.iter().fold(f32::MAX, |m, &(_, y)| m.min(y));
        let max_y = strokes[0].pts.iter().fold(f32::MIN, |m, &(_, y)| m.max(y));
        let mid_y = (min_y + max_y) / 2.0;
        assert!((mid_y - page_h / 2.0).abs() < 2.0, "panned content should be centred, got y={mid_y}");
    }

    /// A PDF annotation written under "Adjust view" (customFit) must normalize by
    /// the custom zoom page span, not the device width — otherwise it renders too
    /// big and shifted down. Numbers are from a real Paper Pro capture: a 612x792pt
    /// page, device 1620px, customZoomPageWidth 1930, stroke at x:[322..584]
    /// y:[1093..1199].
    #[test]
    fn custom_fit_normalizes_by_zoom_span() {
        let (page_w, page_h) = (612.0f32, 792.0f32);
        let stroke = RawPageStroke {
            page: 16,
            pts: vec![(322.0, 1093.0), (584.0, 1199.0)],
            color_id: 0,
            tool_id: 2,
            thickness: 2.0,
            backing_less: false,
        };
        let with_zoom = DocAnnotations {
            last_modified_ms: 0,
            device_width: 1620.0,
            custom_zoom_page_width: Some(1930.0),
            raw: vec![stroke.clone()],
        };
        let no_zoom = DocAnnotations {
            last_modified_ms: 0,
            device_width: 1620.0,
            custom_zoom_page_width: None,
            raw: vec![stroke],
        };
        let zy = |d: &DocAnnotations| {
            let m = d.strokes_by_page(|_| Some((page_w, page_h)));
            let p = &m[&16][0].pts;
            p.iter().fold(f32::MIN, |a, &(_, y)| a.max(y))
        };
        let y_zoom = zy(&with_zoom); // k = 612/1930 → max y ≈ 380
        let y_flat = zy(&no_zoom); //  k = 612/1620 → max y ≈ 453
        // Custom-fit places it higher (smaller y from the top) than the naive fit.
        assert!(y_zoom < y_flat, "zoom fit should sit higher: {y_zoom} vs {y_flat}");
        assert!((y_zoom - 380.0).abs() < 2.0, "expected ~380pt, got {y_zoom}");
        assert!((y_flat - 453.0).abs() < 2.0, "expected ~453pt, got {y_flat}");
    }

    #[test]
    fn blank_notebook_pdf_opens() {
        let dir = std::env::temp_dir().join("tui-pdf-test-nb");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("nb.pdf");
        write_blank_pdf(&path, 3, 447.0, 596.0).expect("write blank pdf");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // It must open as a real PDF with the requested page count.
        let doc = crate::document::Document::open(&path).expect("open blank pdf");
        assert_eq!(doc.page_count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
