//! Render a Markdown file (with GitHub-flavoured extensions and LaTeX math) to a
//! PDF in memory, so it can be viewed through the regular mupdf pipeline.
//!
//! The pipeline is fully native Rust, no external tools:
//!   * `comrak` parses Markdown (incl. `$..$` / `$$..$$` math) into an AST.
//!   * a small layout engine walks the AST and emits `printpdf` operations.
//!   * `ratex` (KaTeX-compatible, pure Rust) renders each math span to a PNG,
//!     embedded inline and baseline-aligned using the display-list metrics.
//!
//! Known v1 limitations (acceptable, documented here): code blocks are not
//! syntax-highlighted; a single block (image / math) taller than the page is
//! scaled to fit rather than split; strikethrough text is rendered without a
//! strike line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{parse_document, Arena, Options};
use printpdf::{
    Color, FontId, Line, LinePoint, Op, ParsedFont, PdfDocument, PdfFontHandle, PdfPage,
    PdfSaveOptions, Point, Polygon, PolygonRing, Pt, Mm, PaintMode, RawImage, Rgb, TextItem,
    WindingOrder, XObjectId, XObjectTransform,
};

use crate::error::{Result, TuiPdfError};

// ---- page geometry, in PDF points (1/72 inch) -----------------------------
const PT_PER_MM: f32 = 72.0 / 25.4;
const PAGE_W: f32 = 215.9 * PT_PER_MM; // US Letter width  (612 pt)
const MARGIN: f32 = 54.0; // 0.75"
const CONTENT_W: f32 = PAGE_W - 2.0 * MARGIN;
const RIGHT: f32 = PAGE_W - MARGIN;

// ---- typography ------------------------------------------------------------
const BODY_SIZE: f32 = 11.0;
const CODE_SIZE: f32 = 9.5;
const LIST_INDENT: f32 = 22.0;
const QUOTE_INDENT: f32 = 16.0;
const CODE_PAD: f32 = 5.0;

// ---- embedded fonts --------------------------------------------------------
const F_REGULAR: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");
const F_BOLD: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");
const F_ITALIC: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Oblique.ttf");
const F_MONO: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

// math render resolution: em pixels = MATH_FONT_PX * MATH_DPR
const MATH_FONT_PX: f32 = 40.0;
const MATH_DPR: f32 = 3.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
    Regular,
    Bold,
    Italic,
    Mono,
}

fn rgb(r: f32, g: f32, b: f32) -> Rgb {
    Rgb {
        r,
        g,
        b,
        icc_profile: None,
    }
}

const COL_TEXT: (f32, f32, f32) = (0.0, 0.0, 0.0);
const COL_LINK: (f32, f32, f32) = (0.10, 0.33, 0.80);
const COL_CODE: (f32, f32, f32) = (0.15, 0.15, 0.15);
const COL_QUOTE: (f32, f32, f32) = (0.35, 0.35, 0.35);
const COL_ERR: (f32, f32, f32) = (0.80, 0.10, 0.10);
const COL_CODE_BG: (f32, f32, f32) = (0.95, 0.95, 0.96);
const COL_RULE: (f32, f32, f32) = (0.70, 0.70, 0.72);

// ---------------------------------------------------------------------------
// Font metrics (for measuring) via ttf-parser.
// ---------------------------------------------------------------------------
struct FaceMetrics {
    face: ttf_parser::Face<'static>,
    upem: f32,
}

impl FaceMetrics {
    fn new(bytes: &'static [u8]) -> Self {
        let face = ttf_parser::Face::parse(bytes, 0).expect("bundled font parses");
        let upem = face.units_per_em() as f32;
        Self { face, upem }
    }
    fn advance(&self, ch: char, size: f32) -> f32 {
        let a = self
            .face
            .glyph_index(ch)
            .and_then(|g| self.face.glyph_hor_advance(g))
            .unwrap_or(0) as f32;
        a / self.upem * size
    }
    fn ascent(&self, size: f32) -> f32 {
        self.face.ascender() as f32 / self.upem * size
    }
    fn descent(&self, size: f32) -> f32 {
        // returned as a positive distance below the baseline
        (-(self.face.descender() as f32)) / self.upem * size
    }
}

struct Faces {
    regular: FaceMetrics,
    bold: FaceMetrics,
    italic: FaceMetrics,
    mono: FaceMetrics,
}

impl Faces {
    fn new() -> Self {
        Self {
            regular: FaceMetrics::new(F_REGULAR),
            bold: FaceMetrics::new(F_BOLD),
            italic: FaceMetrics::new(F_ITALIC),
            mono: FaceMetrics::new(F_MONO),
        }
    }
    fn metrics(&self, f: Face) -> &FaceMetrics {
        match f {
            Face::Regular => &self.regular,
            Face::Bold => &self.bold,
            Face::Italic => &self.italic,
            Face::Mono => &self.mono,
        }
    }
    fn measure(&self, text: &str, f: Face, size: f32) -> f32 {
        let m = self.metrics(f);
        text.chars().map(|c| m.advance(c, size)).sum()
    }
}

// Build a printpdf font from the bundled bytes, mapping every codepoint we may
// emit to its glyph id + advance width (the text_layout-free `with_glyph_data`
// path — printpdf does not parse the font itself in this build).
fn build_pdf_font(bytes: &'static [u8], name: &str) -> ParsedFont {
    let face = ttf_parser::Face::parse(bytes, 0).expect("bundled font parses");
    let upem = face.units_per_em();
    let mut cp_to_gid: BTreeMap<u32, u16> = BTreeMap::new();
    let mut widths: BTreeMap<u16, u16> = BTreeMap::new();
    // Map every Unicode codepoint the font actually supports (enumerated from
    // its cmap), so any glyph in the document — punctuation, math symbols,
    // ballot boxes, Greek, etc. — renders rather than showing as tofu.
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap.subtables {
            if !subtable.is_unicode() {
                continue;
            }
            subtable.codepoints(|cp| {
                if let Some(ch) = char::from_u32(cp)
                    && let Some(gid) = face.glyph_index(ch)
                {
                    cp_to_gid.insert(cp, gid.0);
                    widths.insert(gid.0, face.glyph_hor_advance(gid).unwrap_or(0));
                }
            });
        }
    }
    let fm = printpdf::FontMetrics {
        ascent: face.ascender(),
        descent: face.descender(),
    };
    ParsedFont::with_glyph_data(
        bytes.to_vec(),
        0,
        Some(name.to_string()),
        cp_to_gid,
        widths,
        upem,
        fm,
    )
}

struct FontIds {
    regular: FontId,
    bold: FontId,
    italic: FontId,
    mono: FontId,
}

impl FontIds {
    fn handle(&self, f: Face) -> PdfFontHandle {
        let id = match f {
            Face::Regular => &self.regular,
            Face::Bold => &self.bold,
            Face::Italic => &self.italic,
            Face::Mono => &self.mono,
        };
        PdfFontHandle::External(id.clone())
    }
}

// ---------------------------------------------------------------------------
// Inline atoms and wrapped lines.
// ---------------------------------------------------------------------------
#[derive(Clone)]
struct Style {
    face: Face,
    size: f32,
    color: (f32, f32, f32),
}

#[derive(Clone)]
enum Atom {
    /// A run of non-space characters (a "word") of a single style.
    Word {
        text: String,
        width: f32,
        face: Face,
        size: f32,
        color: (f32, f32, f32),
        ascent: f32,
        descent: f32,
    },
    /// Breakable whitespace.
    Space { width: f32 },
    /// An inline image box (math or image), aligned on the baseline.
    Box {
        xobj: XObjectId,
        w: f32,
        ascent: f32,
        descent: f32,
        dpi: f32,
    },
    /// A display-math box: forced onto its own centred line.
    Display {
        xobj: XObjectId,
        w: f32,
        ascent: f32,
        descent: f32,
        dpi: f32,
    },
    HardBreak,
}

struct Placed {
    x: f32,
    kind: PlacedKind,
}

enum PlacedKind {
    Word {
        text: String,
        face: Face,
        size: f32,
        color: (f32, f32, f32),
    },
    Box {
        xobj: XObjectId,
        descent: f32,
        dpi: f32,
    },
}

enum LineBox {
    Text {
        items: Vec<Placed>,
        ascent: f32,
        descent: f32,
        max_size: f32,
    },
    Display {
        xobj: XObjectId,
        w: f32,
        ascent: f32,
        descent: f32,
        dpi: f32,
    },
}

// Greedily wrap a flat atom list into lines fitting `max_width`.
#[allow(unused_assignments)]
fn wrap(atoms: &[Atom], max_width: f32) -> Vec<LineBox> {
    let mut lines: Vec<LineBox> = Vec::new();
    let mut cur: Vec<Placed> = Vec::new();
    let mut x = 0.0f32;
    let mut ascent = 0.0f32;
    let mut descent = 0.0f32;
    let mut max_size = BODY_SIZE;
    let mut pending_space = 0.0f32;

    macro_rules! flush {
        ($force:expr) => {
            if !cur.is_empty() || $force {
                lines.push(LineBox::Text {
                    items: std::mem::take(&mut cur),
                    ascent: if ascent > 0.0 {
                        ascent
                    } else {
                        BODY_SIZE * 0.8
                    },
                    descent: if descent > 0.0 {
                        descent
                    } else {
                        BODY_SIZE * 0.2
                    },
                    max_size,
                });
                x = 0.0;
                ascent = 0.0;
                descent = 0.0;
                max_size = BODY_SIZE;
                pending_space = 0.0;
            }
        };
    }

    for atom in atoms {
        match atom {
            Atom::Space { width } => pending_space += width,
            Atom::HardBreak => flush!(true),
            Atom::Display {
                xobj,
                w,
                ascent: a,
                descent: d,
                dpi,
            } => {
                flush!(false);
                lines.push(LineBox::Display {
                    xobj: xobj.clone(),
                    w: *w,
                    ascent: *a,
                    descent: *d,
                    dpi: *dpi,
                });
            }
            Atom::Word {
                text,
                width,
                face,
                size,
                color,
                ascent: a,
                descent: d,
            } => {
                if !cur.is_empty() && x + pending_space + width > max_width {
                    flush!(false);
                }
                if !cur.is_empty() {
                    x += pending_space;
                }
                pending_space = 0.0;
                cur.push(Placed {
                    x,
                    kind: PlacedKind::Word {
                        text: text.clone(),
                        face: *face,
                        size: *size,
                        color: *color,
                    },
                });
                x += width;
                ascent = ascent.max(*a);
                descent = descent.max(*d);
                max_size = max_size.max(*size);
            }
            Atom::Box {
                xobj,
                w,
                ascent: a,
                descent: d,
                dpi,
            } => {
                if !cur.is_empty() && x + pending_space + w > max_width {
                    flush!(false);
                }
                if !cur.is_empty() {
                    x += pending_space;
                }
                pending_space = 0.0;
                cur.push(Placed {
                    x,
                    kind: PlacedKind::Box {
                        xobj: xobj.clone(),
                        descent: *d,
                        dpi: *dpi,
                    },
                });
                x += w;
                ascent = ascent.max(*a);
                descent = descent.max(*d);
            }
        }
    }
    flush!(false);
    lines
}

// ---------------------------------------------------------------------------
// The builder: walks the AST and accumulates printpdf pages.
// ---------------------------------------------------------------------------
/// Maps a Markdown source line to its rendered position, for SyncTeX-style
/// forward/reverse search. `page` is 0-based; `y` is points from the page top.
#[derive(Clone, Copy, Debug)]
pub struct SourceLoc {
    pub line: usize,
    pub page: usize,
    pub y: f32,
}

/// A drawing command in top-down page coordinates (`y` = distance from the top
/// of the single continuous page). Markdown renders as one tall page, so the
/// final PDF y (origin bottom-left) is computed once the total height is known.
enum Draw {
    Text {
        x: f32,
        baseline_top: f32,
        text: String,
        face: Face,
        size: f32,
        color: (f32, f32, f32),
    },
    Image {
        x: f32,
        baseline_top: f32,
        descent: f32,
        xobj: XObjectId,
        dpi: f32,
    },
    Rect {
        x: f32,
        top: f32,
        w: f32,
        h: f32,
        color: (f32, f32, f32),
    },
    Line {
        x0: f32,
        t0: f32,
        x1: f32,
        t1: f32,
        color: (f32, f32, f32),
        thick: f32,
    },
}

struct Builder<'a> {
    doc: &'a mut PdfDocument,
    fonts: &'a FontIds,
    faces: &'a Faces,
    base_dir: PathBuf,
    draws: Vec<Draw>,
    y: f32, // distance from page top, in pt
    sourcemap: Vec<SourceLoc>,
}

fn linepoint(x: f32, y: f32) -> LinePoint {
    LinePoint {
        p: Point {
            x: Pt(x),
            y: Pt(y),
        },
        bezier: false,
    }
}

impl<'a> Builder<'a> {
    fn new(
        doc: &'a mut PdfDocument,
        fonts: &'a FontIds,
        faces: &'a Faces,
        base_dir: PathBuf,
    ) -> Self {
        Self {
            doc,
            fonts,
            faces,
            base_dir,
            draws: Vec::new(),
            y: MARGIN,
            sourcemap: Vec::new(),
        }
    }

    // Markdown renders as one continuous page; there are no page breaks, so
    // this is a no-op kept at the former break points for clarity.
    fn ensure(&mut self, _h: f32) {}

    /// Emit a single page whose height equals the laid-out content (plus a
    /// bottom margin), converting the top-down draw commands to bottom-origin
    /// PDF ops now that the total height is known.
    fn finish(self) -> Vec<PdfPage> {
        let total_h = self.y + MARGIN;
        let mut ops: Vec<Op> = Vec::with_capacity(self.draws.len() * 4);
        for d in &self.draws {
            match d {
                Draw::Rect { x, top, w, h, color } => {
                    let y0 = total_h - (top + h);
                    let y1 = total_h - top;
                    ops.push(Op::SetFillColor {
                        col: Color::Rgb(rgb(color.0, color.1, color.2)),
                    });
                    ops.push(Op::DrawPolygon {
                        polygon: Polygon {
                            rings: vec![PolygonRing {
                                points: vec![
                                    linepoint(*x, y0),
                                    linepoint(*x + *w, y0),
                                    linepoint(*x + *w, y1),
                                    linepoint(*x, y1),
                                ],
                            }],
                            mode: PaintMode::Fill,
                            winding_order: WindingOrder::NonZero,
                        },
                    });
                }
                Draw::Line { x0, t0, x1, t1, color, thick } => {
                    ops.push(Op::SetOutlineColor {
                        col: Color::Rgb(rgb(color.0, color.1, color.2)),
                    });
                    ops.push(Op::SetOutlineThickness { pt: Pt(*thick) });
                    ops.push(Op::DrawLine {
                        line: Line {
                            points: vec![
                                linepoint(*x0, total_h - *t0),
                                linepoint(*x1, total_h - *t1),
                            ],
                            is_closed: false,
                        },
                    });
                }
                Draw::Text { x, baseline_top, text, face, size, color } => {
                    ops.push(Op::StartTextSection);
                    ops.push(Op::SetFont {
                        font: self.fonts.handle(*face),
                        size: Pt(*size),
                    });
                    ops.push(Op::SetFillColor {
                        col: Color::Rgb(rgb(color.0, color.1, color.2)),
                    });
                    ops.push(Op::SetTextCursor {
                        pos: Point {
                            x: Pt(*x),
                            y: Pt(total_h - *baseline_top),
                        },
                    });
                    ops.push(Op::ShowText {
                        items: vec![TextItem::Text(text.clone())],
                    });
                    ops.push(Op::EndTextSection);
                }
                Draw::Image { x, baseline_top, descent, xobj, dpi } => {
                    let ll_y = total_h - (baseline_top + descent);
                    ops.push(Op::UseXobject {
                        id: xobj.clone(),
                        transform: XObjectTransform {
                            translate_x: Some(Pt(*x)),
                            translate_y: Some(Pt(ll_y)),
                            rotate: None,
                            scale_x: None,
                            scale_y: None,
                            dpi: Some(*dpi),
                        },
                    });
                }
            }
        }
        vec![PdfPage::new(Mm(PAGE_W / PT_PER_MM), Mm(total_h / PT_PER_MM), ops)]
    }

    // -- primitive drawing (records top-down commands) ---------------------
    fn fill_rect(&mut self, x: f32, y_top: f32, w: f32, h: f32, c: (f32, f32, f32)) {
        self.draws.push(Draw::Rect { x, top: y_top, w, h, color: c });
    }

    fn stroke_line(&mut self, x0: f32, yt0: f32, x1: f32, yt1: f32, c: (f32, f32, f32), t: f32) {
        self.draws.push(Draw::Line { x0, t0: yt0, x1, t1: yt1, color: c, thick: t });
    }

    fn draw_word(&mut self, x: f32, baseline_top: f32, text: &str, face: Face, size: f32, c: (f32, f32, f32)) {
        self.draws.push(Draw::Text {
            x,
            baseline_top,
            text: text.to_string(),
            face,
            size,
            color: c,
        });
    }

    fn draw_box(&mut self, x: f32, baseline_top: f32, xobj: &XObjectId, descent: f32, dpi: f32) {
        self.draws.push(Draw::Image {
            x,
            baseline_top,
            descent,
            xobj: xobj.clone(),
            dpi,
        });
    }

    // -- flowing wrapped lines ---------------------------------------------
    fn emit_flow(&mut self, lines: Vec<LineBox>, left: f32, bar_x: Option<f32>) {
        for line in lines {
            match line {
                LineBox::Text {
                    items,
                    ascent,
                    descent,
                    max_size,
                } => {
                    let leading = max_size * 0.32;
                    let h = ascent + descent + leading;
                    self.ensure(h);
                    let baseline = self.y + ascent;
                    if let Some(bx) = bar_x {
                        self.fill_rect(bx, self.y, 3.0, ascent + descent, COL_QUOTE);
                    }
                    for placed in items {
                        let px = left + placed.x;
                        match placed.kind {
                            PlacedKind::Word {
                                text,
                                face,
                                size,
                                color,
                            } => self.draw_word(px, baseline, &text, face, size, color),
                            PlacedKind::Box {
                                xobj,
                                descent: d,
                                dpi,
                                ..
                            } => self.draw_box(px, baseline, &xobj, d, dpi),
                        }
                    }
                    self.y += h;
                }
                LineBox::Display {
                    xobj,
                    w,
                    ascent,
                    descent,
                    dpi,
                } => {
                    let leading = BODY_SIZE * 0.6;
                    let h = ascent + descent + leading;
                    self.ensure(h);
                    let baseline = self.y + ascent + leading * 0.5;
                    let x = MARGIN + (CONTENT_W - w).max(0.0) / 2.0;
                    self.draw_box(x, baseline, &xobj, descent, dpi);
                    self.y += h;
                }
            }
        }
    }

    // -- block walking ------------------------------------------------------
    fn blocks(&mut self, node: &'a AstNode<'a>, indent: f32) {
        for child in node.children() {
            self.block(child, indent);
        }
    }

    fn block(&mut self, node: &'a AstNode<'a>, indent: f32) {
        // Record where this block's source line lands in the rendered PDF, for
        // forward/reverse search. `pages.len()` is the page currently being
        // built; `y` is the cursor measured from the page top.
        let start_line = node.data.borrow().sourcepos.start.line;
        if start_line > 0 {
            self.sourcemap.push(SourceLoc {
                line: start_line,
                page: 0, // single continuous page
                y: self.y,
            });
        }
        let value = node.data.borrow().value.clone();
        match value {
            NodeValue::Heading(h) => {
                let size = match h.level {
                    1 => 21.0,
                    2 => 17.0,
                    3 => 14.5,
                    4 => 12.5,
                    5 => 11.5,
                    _ => 11.0,
                };
                self.y += size * 0.5;
                let style = Style {
                    face: Face::Bold,
                    size,
                    color: COL_TEXT,
                };
                let mut atoms = Vec::new();
                self.inlines(node, &style, &mut atoms);
                let lines = wrap(&atoms, CONTENT_W - indent);
                self.emit_flow(lines, MARGIN + indent, None);
                self.y += size * 0.25;
            }
            NodeValue::Paragraph => {
                let style = Style {
                    face: Face::Regular,
                    size: BODY_SIZE,
                    color: COL_TEXT,
                };
                let mut atoms = Vec::new();
                self.inlines(node, &style, &mut atoms);
                let lines = wrap(&atoms, CONTENT_W - indent);
                self.emit_flow(lines, MARGIN + indent, None);
                self.y += BODY_SIZE * 0.5;
            }
            NodeValue::List(_) => {
                self.list(node, indent);
                self.y += BODY_SIZE * 0.3;
            }
            NodeValue::CodeBlock(cb) => self.code_block(&cb.literal, indent),
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                self.y += BODY_SIZE * 0.2;
                self.quote(node, indent);
                self.y += BODY_SIZE * 0.3;
            }
            NodeValue::ThematicBreak => {
                self.ensure(BODY_SIZE);
                self.y += BODY_SIZE * 0.4;
                self.stroke_line(MARGIN + indent, self.y, RIGHT, self.y, COL_RULE, 0.8);
                self.y += BODY_SIZE * 0.6;
            }
            NodeValue::Table(_) => self.table(node, indent),
            // Any other block container: recurse into its children.
            _ => self.blocks(node, indent),
        }
    }

    fn list(&mut self, node: &'a AstNode<'a>, indent: f32) {
        let mut ordinal = match &node.data.borrow().value {
            NodeValue::List(l) if l.list_type == ListType::Ordered => l.start,
            _ => 0,
        };
        let ordered = ordinal != 0
            || matches!(&node.data.borrow().value, NodeValue::List(l) if l.list_type == ListType::Ordered);
        for item in node.children() {
            let item_value = item.data.borrow().value.clone();
            // marker
            let marker = match &item_value {
                NodeValue::TaskItem(t) => {
                    if t.symbol.is_some() {
                        "\u{2611}".to_string() // ☑
                    } else {
                        "\u{2610}".to_string() // ☐
                    }
                }
                _ if ordered => {
                    let m = format!("{}.", ordinal);
                    ordinal += 1;
                    m
                }
                _ => "\u{2022}".to_string(), // •
            };
            let marker_top = self.y;
            // Render the item body first at the indented column, then drop the
            // marker on the first body line's baseline.
            let body_indent = indent + LIST_INDENT;
            let y_before = self.y;
            self.blocks(item, body_indent);
            // marker baseline ~ first line baseline of body
            let baseline = marker_top + self.faces.metrics(Face::Regular).ascent(BODY_SIZE);
            if self.y > y_before {
                self.draw_word(
                    MARGIN + indent + 2.0,
                    baseline,
                    &marker,
                    Face::Regular,
                    BODY_SIZE,
                    COL_TEXT,
                );
            }
        }
    }

    fn quote(&mut self, node: &'a AstNode<'a>, indent: f32) {
        // Children rendered with extra indent; emit_flow draws the left bar per
        // line via bar_x. To get the bar we route paragraphs through a quote
        // style; nested blocks recurse normally (bar only on direct text).
        let inner = indent + QUOTE_INDENT;
        for child in node.children() {
            let v = child.data.borrow().value.clone();
            if let NodeValue::Paragraph = v {
                let style = Style {
                    face: Face::Italic,
                    size: BODY_SIZE,
                    color: COL_QUOTE,
                };
                let mut atoms = Vec::new();
                self.inlines(child, &style, &mut atoms);
                let lines = wrap(&atoms, CONTENT_W - inner);
                self.emit_flow(lines, MARGIN + inner, Some(MARGIN + indent + 4.0));
                self.y += BODY_SIZE * 0.4;
            } else {
                self.block(child, inner);
            }
        }
    }

    fn code_block(&mut self, literal: &str, indent: f32) {
        let left = MARGIN + indent;
        let avail = CONTENT_W - indent - 2.0 * CODE_PAD;
        let line_h = CODE_SIZE * 1.4;
        self.y += BODY_SIZE * 0.3;
        for raw in literal.trim_end_matches('\n').split('\n') {
            // character-wrap long lines
            for chunk in wrap_mono(raw, avail, self.faces) {
                self.ensure(line_h);
                self.fill_rect(left, self.y, CONTENT_W - indent, line_h, COL_CODE_BG);
                let baseline = self.y + self.faces.metrics(Face::Mono).ascent(CODE_SIZE) + 2.0;
                if !chunk.is_empty() {
                    self.draw_word(left + CODE_PAD, baseline, &chunk, Face::Mono, CODE_SIZE, COL_CODE);
                }
                self.y += line_h;
            }
        }
        self.y += BODY_SIZE * 0.4;
    }

    fn table(&mut self, node: &'a AstNode<'a>, indent: f32) {
        // Collect rows -> each row is a Vec of cell nodes.
        let rows: Vec<&'a AstNode<'a>> = node.children().collect();
        if rows.is_empty() {
            return;
        }
        let ncols = rows
            .iter()
            .map(|r| r.children().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let col_w = (CONTENT_W - indent) / ncols as f32;
        let cell_pad = 4.0;
        self.y += BODY_SIZE * 0.3;
        for row in rows {
            let is_header = matches!(&row.data.borrow().value, NodeValue::TableRow(true));
            let face = if is_header { Face::Bold } else { Face::Regular };
            // Wrap every cell, find row height.
            let mut cell_lines: Vec<Vec<LineBox>> = Vec::new();
            for cell in row.children() {
                let style = Style {
                    face,
                    size: BODY_SIZE,
                    color: COL_TEXT,
                };
                let mut atoms = Vec::new();
                self.inlines(cell, &style, &mut atoms);
                cell_lines.push(wrap(&atoms, col_w - 2.0 * cell_pad));
            }
            let line_h = BODY_SIZE * 1.35;
            let max_lines = cell_lines.iter().map(|l| l.len().max(1)).max().unwrap_or(1);
            let row_h = max_lines as f32 * line_h + 2.0 * cell_pad;
            self.ensure(row_h);
            let top = self.y;
            let left = MARGIN + indent;
            if is_header {
                self.fill_rect(left, top, CONTENT_W - indent, row_h, (0.93, 0.93, 0.95));
            }
            // cell text
            for (ci, lines) in cell_lines.into_iter().enumerate() {
                let cx = left + ci as f32 * col_w + cell_pad;
                let mut yy = top + cell_pad;
                for line in lines {
                    if let LineBox::Text { items, ascent, .. } = line {
                        let baseline = yy + ascent;
                        for placed in items {
                            match placed.kind {
                                PlacedKind::Word {
                                    text,
                                    face: f,
                                    size,
                                    color,
                                } => self.draw_word(cx + placed.x, baseline, &text, f, size, color),
                                PlacedKind::Box { xobj, descent, dpi } => {
                                    self.draw_box(cx + placed.x, baseline, &xobj, descent, dpi)
                                }
                            }
                        }
                        yy += line_h;
                    }
                }
            }
            // grid: horizontal top + vertical separators
            self.stroke_line(left, top, RIGHT, top, COL_RULE, 0.6);
            for ci in 0..=ncols {
                let x = left + ci as f32 * col_w;
                self.stroke_line(x, top, x, top + row_h, COL_RULE, 0.6);
            }
            self.stroke_line(left, top + row_h, RIGHT, top + row_h, COL_RULE, 0.6);
            self.y += row_h;
        }
        self.y += BODY_SIZE * 0.4;
    }

    // -- inline walking -----------------------------------------------------
    fn inlines(&mut self, node: &'a AstNode<'a>, style: &Style, out: &mut Vec<Atom>) {
        for child in node.children() {
            self.inline(child, style, out);
        }
    }

    fn inline(&mut self, node: &'a AstNode<'a>, style: &Style, out: &mut Vec<Atom>) {
        let value = node.data.borrow().value.clone();
        match value {
            NodeValue::Text(t) => self.push_text(&t, style, out),
            NodeValue::Code(c) => {
                let code_style = Style {
                    face: Face::Mono,
                    size: style.size * 0.94,
                    color: COL_CODE,
                };
                self.push_text(&c.literal, &code_style, out);
            }
            NodeValue::Emph => {
                let s = Style {
                    face: if style.face == Face::Bold {
                        Face::Bold
                    } else {
                        Face::Italic
                    },
                    ..style.clone()
                };
                self.inlines(node, &s, out);
            }
            NodeValue::Strong => {
                let s = Style {
                    face: Face::Bold,
                    ..style.clone()
                };
                self.inlines(node, &s, out);
            }
            NodeValue::Strikethrough => self.inlines(node, style, out),
            NodeValue::Link(_) => {
                let s = Style {
                    color: COL_LINK,
                    ..style.clone()
                };
                self.inlines(node, &s, out);
            }
            NodeValue::Image(link) => {
                if let Some(atom) = self.image_atom(&link.url) {
                    out.push(atom);
                }
            }
            NodeValue::SoftBreak => out.push(Atom::Space {
                width: self.faces.metrics(style.face).advance(' ', style.size),
            }),
            NodeValue::LineBreak => out.push(Atom::HardBreak),
            NodeValue::Math(m) => self.math_atom(&m.literal, m.display_math, style, out),
            // unknown inline container: recurse
            _ => self.inlines(node, style, out),
        }
    }

    fn push_text(&self, text: &str, style: &Style, out: &mut Vec<Atom>) {
        let m = self.faces.metrics(style.face);
        let mut word = String::new();
        let flush_word = |word: &mut String, out: &mut Vec<Atom>| {
            if !word.is_empty() {
                let width = self.faces.measure(word, style.face, style.size);
                out.push(Atom::Word {
                    text: std::mem::take(word),
                    width,
                    face: style.face,
                    size: style.size,
                    color: style.color,
                    ascent: m.ascent(style.size),
                    descent: m.descent(style.size),
                });
            }
        };
        for ch in text.chars() {
            if ch.is_whitespace() {
                flush_word(&mut word, out);
                out.push(Atom::Space {
                    width: m.advance(' ', style.size),
                });
            } else {
                word.push(ch);
            }
        }
        flush_word(&mut word, out);
    }

    fn image_atom(&mut self, url: &str) -> Option<Atom> {
        // Only local images are supported (no network fetch).
        if url.starts_with("http://") || url.starts_with("https://") {
            return None;
        }
        let path = self.base_dir.join(url);
        let bytes = std::fs::read(&path).ok()?;
        let mut warnings = Vec::new();
        let raw = RawImage::decode_from_bytes(&bytes, &mut warnings).ok()?;
        let (pw, ph) = (raw.width as f32, raw.height as f32);
        if pw <= 0.0 || ph <= 0.0 {
            return None;
        }
        let xobj = self.doc.add_image(&raw);
        // natural size assuming 96 dpi, clamped to content width
        let mut w = pw * 72.0 / 96.0;
        let mut h = ph * 72.0 / 96.0;
        if w > CONTENT_W {
            let s = CONTENT_W / w;
            w *= s;
            h *= s;
        }
        let dpi = pw * 72.0 / w;
        Some(Atom::Box {
            xobj,
            w,
            ascent: h,
            descent: 0.0,
            dpi,
        })
    }

    fn math_atom(&mut self, latex: &str, display: bool, style: &Style, out: &mut Vec<Atom>) {
        match self.render_math(latex, display, style.size) {
            Some((xobj, w, ascent, descent, dpi)) => {
                if display {
                    out.push(Atom::Display {
                        xobj,
                        w,
                        ascent,
                        descent,
                        dpi,
                    });
                } else {
                    out.push(Atom::Box {
                        xobj,
                        w,
                        ascent,
                        descent,
                        dpi,
                    });
                }
            }
            None => {
                // Fallback: show the raw LaTeX in red monospace so the document
                // still renders and the user sees what failed.
                let err_style = Style {
                    face: Face::Mono,
                    size: style.size * 0.94,
                    color: COL_ERR,
                };
                let delim = if display { "$$" } else { "$" };
                self.push_text(&format!("{delim}{latex}{delim}"), &err_style, out);
            }
        }
    }

    fn render_math(
        &mut self,
        latex: &str,
        display: bool,
        em_pt: f32,
    ) -> Option<(XObjectId, f32, f32, f32, f32)> {
        use ratex_layout::{layout, to_display_list, LayoutOptions};
        use ratex_parser::parser::parse;
        use ratex_types::color::Color as RxColor;
        use ratex_types::math_style::MathStyle;

        let ast = parse(latex).ok()?;
        let style = if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        };
        let lopts = LayoutOptions::default()
            .with_style(style)
            .with_color(RxColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            });
        let lbox = layout(&ast, &lopts);
        let dl = to_display_list(&lbox);

        let ropts = ratex_render::RenderOptions {
            font_size: MATH_FONT_PX,
            padding: 0.0,
            background_color: RxColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            font_dir: String::new(),
            device_pixel_ratio: MATH_DPR,
        };
        let png = ratex_render::render_to_png(&dl, &ropts).ok()?;
        let mut warnings = Vec::new();
        let raw = RawImage::decode_from_bytes(&png, &mut warnings).ok()?;
        let px_w = raw.width as f32;
        if px_w <= 0.0 {
            return None;
        }
        let xobj = self.doc.add_image(&raw);

        let mut w = dl.width as f32 * em_pt;
        let mut ascent = dl.height as f32 * em_pt;
        let mut descent = dl.depth as f32 * em_pt;
        // scale down if wider than the content area
        let limit = CONTENT_W;
        if w > limit && w > 0.0 {
            let s = limit / w;
            w *= s;
            ascent *= s;
            descent *= s;
        }
        if w <= 0.0 {
            w = 1.0;
        }
        let dpi = px_w * 72.0 / w;
        Some((xobj, w, ascent, descent, dpi))
    }
}

// character-wrap a code line to a pixel width, returning chunks
fn wrap_mono(line: &str, max_width: f32, faces: &Faces) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let m = faces.metrics(Face::Mono);
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut w = 0.0f32;
    for ch in line.chars() {
        let cw = m.advance(ch, CODE_SIZE);
        if w + cw > max_width && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
            w = 0.0;
        }
        cur.push(ch);
        w += cw;
    }
    chunks.push(cur);
    chunks
}

/// Render a Markdown file to PDF bytes (discarding the source map).
pub fn render_to_pdf_bytes(path: &Path) -> Result<Vec<u8>> {
    Ok(render(path)?.0)
}

/// Render a Markdown file to PDF bytes plus a source-line → PDF-position map
/// (sorted by page then y) for SyncTeX-style forward/reverse search.
pub fn render(path: &Path) -> Result<(Vec<u8>, Vec<SourceLoc>)> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| TuiPdfError::Other(format!("Failed to read markdown {}: {}", path.display(), e)))?;

    let mut options = Options::default();
    options.extension.math_dollars = true;
    options.extension.math_code = true;
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    options.extension.strikethrough = true;
    options.extension.footnotes = true;

    let arena = Arena::new();
    let root = parse_document(&arena, &text, &options);

    let mut doc = PdfDocument::new(
        path.file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "markdown".to_string())
            .as_str(),
    );
    let fonts = FontIds {
        regular: doc.add_font(&build_pdf_font(F_REGULAR, "DejaVuSans")),
        bold: doc.add_font(&build_pdf_font(F_BOLD, "DejaVuSans-Bold")),
        italic: doc.add_font(&build_pdf_font(F_ITALIC, "DejaVuSans-Oblique")),
        mono: doc.add_font(&build_pdf_font(F_MONO, "DejaVuSansMono")),
    };
    let faces = Faces::new();
    let base_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();

    let (pages, mut sourcemap) = {
        let mut builder = Builder::new(&mut doc, &fonts, &faces, base_dir);
        builder.blocks(root, 0.0);
        let map = std::mem::take(&mut builder.sourcemap);
        (builder.finish(), map)
    };
    sourcemap.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
    });

    doc.with_pages(pages);
    let mut warnings = Vec::new();
    let bytes = doc.save(&PdfSaveOptions::default(), &mut warnings);
    Ok((bytes, sourcemap))
}
