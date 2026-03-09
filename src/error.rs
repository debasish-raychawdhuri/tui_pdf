use thiserror::Error;

#[derive(Debug, Error)]
pub enum TuiPdfError {
    #[error("MuPDF error: {0}")]
    Mupdf(#[from] mupdf::Error),

    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Page {0} out of range (document has {1} pages)")]
    PageOutOfRange(usize, usize),
}

pub type Result<T> = std::result::Result<T, TuiPdfError>;
