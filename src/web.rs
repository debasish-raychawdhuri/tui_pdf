use std::time::Duration;

use base64::Engine;
use headless_chrome::protocol::cdp::Page;
use headless_chrome::{Browser, LaunchOptions};
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
        TuiPdfError::Other(format!(
            "Failed to launch Chrome: {}. Is Chrome/Chromium installed?",
            e
        ))
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

    // Get the full page dimensions via JS
    let remote_obj = tab
        .evaluate(
            "JSON.stringify({width: Math.max(document.body.scrollWidth, document.documentElement.scrollWidth), height: Math.max(document.body.scrollHeight, document.documentElement.scrollHeight)})",
            false,
        )
        .map_err(|e| TuiPdfError::Other(format!("Failed to get page dimensions: {}", e)))?;

    let (page_width, page_height) = if let Some(val) = remote_obj.value {
        let s = val.as_str().unwrap_or("{}");
        let parsed: serde_json::Value =
            serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
        let w = parsed["width"].as_f64().unwrap_or(1920.0);
        let h = parsed["height"].as_f64().unwrap_or(1080.0);
        (w, h.min(MAX_CAPTURE_HEIGHT as f64))
    } else {
        (1920.0, 1080.0)
    };

    // Use call_method directly to set capture_beyond_viewport
    let response = tab
        .call_method(Page::CaptureScreenshot {
            format: Some(Page::CaptureScreenshotFormatOption::Png),
            clip: Some(Page::Viewport {
                x: 0.0,
                y: 0.0,
                width: page_width,
                height: page_height,
                scale: 1.0,
            }),
            quality: None,
            from_surface: Some(true),
            capture_beyond_viewport: Some(true),
            optimize_for_speed: None,
        })
        .map_err(|e| TuiPdfError::Other(format!("Screenshot failed: {}", e)))?;

    let png_data = base64::prelude::BASE64_STANDARD
        .decode(&response.data)
        .map_err(|e| TuiPdfError::Other(format!("Failed to decode base64 screenshot: {}", e)))?;

    let image = image::load_from_memory_with_format(&png_data, image::ImageFormat::Png)
        .map_err(|e| TuiPdfError::Other(format!("Failed to decode screenshot: {}", e)))?;

    Ok(WebContent {
        url: url.to_string(),
        image,
    })
}
