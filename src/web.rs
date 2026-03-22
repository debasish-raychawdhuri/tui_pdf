use std::time::Duration;

use headless_chrome::{Browser, LaunchOptions};
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use image::DynamicImage;

use crate::error::{Result, TuiPdfError};

pub struct WebContent {
    pub url: String,
    pub image: DynamicImage,
}

/// Maximum screenshot height to avoid excessive memory usage.
const MAX_CAPTURE_HEIGHT: u32 = 20000;

pub fn capture_url(url: &str) -> Result<WebContent> {
    let launch_options = LaunchOptions {
        headless: true,
        window_size: Some((1920, 1080)),
        ..LaunchOptions::default()
    };

    let browser = Browser::new(launch_options).map_err(|e| {
        TuiPdfError::Other(format!("Failed to launch Chrome: {}. Is Chrome/Chromium installed?", e))
    })?;

    let tab = browser.new_tab().map_err(|e| {
        TuiPdfError::Other(format!("Failed to create browser tab: {}", e))
    })?;

    tab.set_default_timeout(Duration::from_secs(30));

    tab.navigate_to(url).map_err(|e| {
        TuiPdfError::Other(format!("Failed to navigate to {}: {}", url, e))
    })?;

    tab.wait_until_navigated().map_err(|e| {
        TuiPdfError::Other(format!("Page load timeout for {}: {}", url, e))
    })?;

    // Small delay for dynamic content to settle
    std::thread::sleep(Duration::from_millis(500));

    let png_data = tab
        .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
        .map_err(|e| {
            TuiPdfError::Other(format!("Screenshot failed: {}", e))
        })?;

    let image = image::load_from_memory_with_format(&png_data, image::ImageFormat::Png)
        .map_err(|e| TuiPdfError::Other(format!("Failed to decode screenshot: {}", e)))?;

    // Cap height to avoid excessive memory
    let image = if image.height() > MAX_CAPTURE_HEIGHT {
        image.crop_imm(0, 0, image.width(), MAX_CAPTURE_HEIGHT)
    } else {
        image
    };

    Ok(WebContent {
        url: url.to_string(),
        image,
    })
}
