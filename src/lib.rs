pub mod document;
pub mod error;
pub mod renderer;
pub mod toc;
pub mod widget;

pub use document::Document;
pub use error::{Result, TuiPdfError};
pub use renderer::{compute_stripe_count, render_page, render_page_dpi, split_into_stripes, PageCache};
pub use toc::{TocState, TocWidget};
pub use widget::{PdfViewState, PdfWidget, StatusBar};
