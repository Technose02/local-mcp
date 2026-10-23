//! Image pipeline for multimodal delivery (`fetch_url`'s `max_images`).
//!
//! Downloads a candidate image under the same SSRF/size rules as page
//! fetches, then decodes, downscales (longest side <= `MAX_SIDE`) and
//! re-encodes it as JPEG so a full-size screenshot never costs more than a
//! bounded amount of model tokens. Tiny images (icons that slipped through
//! the URL filter) are rejected by dimension.

use std::io::Cursor;
use std::time::Duration;

use crate::domain::error::{ProviderError, ProviderFuture};
use crate::domain::fetch::{ImageSource, PageImage, url_host_blocked};
use crate::infrastructure::http::HttpClient;

/// Longest side of a delivered image (a common multimodal sweet spot).
const MAX_SIDE: u32 = 1568;
/// Refuse to even download larger sources.
const MAX_DOWNLOAD_BYTES: u64 = 5 * 1024 * 1024;
/// Images smaller than this on both sides are icons, not content.
const MIN_SIDE: u32 = 64;

#[derive(Clone)]
pub struct ImageFetcher {
    http: HttpClient,
    allow_private_network: bool,
}

impl ImageFetcher {
    pub fn new(http: HttpClient, allow_private_network: bool) -> Self {
        Self {
            http,
            allow_private_network,
        }
    }
}

impl ImageSource for ImageFetcher {
    fn image<'a>(
        &'a self,
        url: &'a str,
    ) -> ProviderFuture<'a, Result<PageImage, ProviderError>> {
        Box::pin(async move {
            if url_host_blocked(url, self.allow_private_network) {
                return Err(ProviderError::unavailable(
                    "images",
                    "blocked by fetch URL policy",
                ));
            }
            let response = self
                .http
                .client()
                .get(url)
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|error| {
                    ProviderError::http("images", format!("request failed: {error}"))
                })?;
            let status = response.status();
            if !status.is_success() {
                return Err(crate::infrastructure::http::status_error(
                    "images",
                    status,
                    None,
                ));
            }
            let too_large = response
                .content_length()
                .is_some_and(|len| len > MAX_DOWNLOAD_BYTES);
            if too_large {
                return Err(ProviderError::unavailable("images", "source too large"));
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|error| ProviderError::http("images", format!("read failed: {error}")))?;
            if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
                return Err(ProviderError::unavailable("images", "source too large"));
            }

            let image = image::load_from_memory(&bytes).map_err(|error| {
                ProviderError::decode("images", format!("unsupported or corrupt image: {error}"))
            })?;
            let (width, height) = (image.width(), image.height());
            if width.max(height) < MIN_SIDE {
                return Err(ProviderError::unavailable("images", "icon-sized image"));
            }

            let resized = if width.max(height) > MAX_SIDE {
                image.thumbnail(MAX_SIDE, MAX_SIDE)
            } else {
                image
            };
            // JPEG has no alpha: flattening onto white keeps screenshots sane.
            let rgb = resized.to_rgb8();
            let mut encoded = Vec::new();
            {
                let mut encoder =
                    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut encoded), 80);
                encoder.encode_image(&rgb).map_err(|error| {
                    ProviderError::decode("images", format!("jpeg encode failed: {error}"))
                })?;
            }

            Ok(PageImage {
                url: url.to_string(),
                media_type: "image/jpeg".to_string(),
                bytes: encoded,
            })
        })
    }
}
