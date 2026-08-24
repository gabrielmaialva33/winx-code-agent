//! Bounded native-image delivery for multimodal MCP clients.
//!
//! Small valid images pass through unchanged. Large images are decoded with
//! explicit resource limits, resized, and encoded as a bounded JPEG before they
//! enter JSON/base64. A live session also remembers recent content fingerprints
//! so an accidental repeated call does not resend the same multi-megabyte image.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::{engine::general_purpose, Engine};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageFormat, ImageReader, Limits};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadImage;
use crate::utils::path::{expand_user, validate_path_in_workspace, validate_path_with_roots};

/// Supported MCP image MIME types.
pub const SUPPORTED_MIME_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Maximum encoded source size accepted from disk. Decoded dimensions and
/// allocations have independent limits below.
const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
/// Maximum binary image payload before base64 expansion. This turns the 17 MiB
/// production outlier into a response of at most ~2.7 MiB of base64.
pub const MAX_DELIVERED_BYTES: usize = 2 * 1024 * 1024;
/// Multimodal models downsample large inputs anyway; bounding the long edge
/// preserves useful screenshot detail without shipping wallpaper-sized files.
pub const MAX_DELIVERED_DIMENSION: u32 = 2_560;
const MAX_SOURCE_DIMENSION: u32 = 16_384;
const MAX_SOURCE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_DECODE_ALLOC_BYTES: u64 = 320 * 1024 * 1024;
const MIN_TRANSCODE_DIMENSION: u32 = 512;
const JPEG_QUALITIES: [u8; 5] = [85, 75, 65, 55, 45];

/// Safe metadata returned alongside native image content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadImageMetadata {
    pub source_bytes: usize,
    pub delivered_bytes: usize,
    pub base64_bytes: usize,
    pub source_width: u32,
    pub source_height: u32,
    pub delivered_width: u32,
    pub delivered_height: u32,
    pub source_mime_type: String,
    pub delivered_mime_type: String,
    pub transcoded: bool,
    pub deduplicated: bool,
    /// A short content fingerprint for correlation, never file contents.
    pub content_fingerprint: String,
}

/// Detailed server-facing outcome. Direct library callers retain the historical
/// (`mime_type`, `base64_data`) wrapper below.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadImageDelivery {
    Image { mime_type: String, base64_data: String, metadata: ReadImageMetadata },
    AlreadyDelivered { metadata: ReadImageMetadata },
}

struct ImageSource {
    bytes: Vec<u8>,
    path: PathBuf,
    format: ImageFormat,
    mime_type: String,
    width: u32,
    height: u32,
    fingerprint: String,
    short_fingerprint: String,
}

fn image_error(path: &Path, message: impl Into<String>) -> WinxError {
    WinxError::FileAccessError { path: path.to_path_buf(), message: message.into() }
}

fn supported_mime(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

fn decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    limits
}

fn fingerprint(bytes: &[u8]) -> (String, String) {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut full = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(full, "{byte:02x}");
    }
    let short = full[..16].to_string();
    (full, short)
}

fn read_image_source_impl(
    file_path: &str,
    cwd: &Path,
    workspace_root: &Path,
    extra_roots: Option<&[PathBuf]>,
) -> Result<ImageSource> {
    debug!(file_path, "reading image source");
    let expanded = expand_user(file_path);
    let requested = if Path::new(&expanded).is_absolute() {
        PathBuf::from(&expanded)
    } else {
        cwd.join(&expanded)
    };
    let validated = match extra_roots {
        Some(extra_roots) => validate_path_with_roots(&requested, workspace_root, extra_roots),
        None => validate_path_in_workspace(&requested, workspace_root),
    };
    let path = validated.map_err(|error| WinxError::PathSecurityError {
        path: requested,
        message: error.to_string(),
    })?;

    if !path.is_file() {
        return Err(image_error(&path, "file does not exist or is not a regular file"));
    }
    let size = std::fs::metadata(&path).map_err(|error| {
        image_error(&path, format!("could not inspect image metadata: {error}"))
    })?;
    if size.len() > MAX_SOURCE_BYTES {
        return Err(WinxError::FileTooLarge { path, size: size.len(), max_size: MAX_SOURCE_BYTES });
    }

    let bytes = std::fs::read(&path)
        .map_err(|error| image_error(&path, format!("could not read image: {error}")))?;
    let actual_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_size > MAX_SOURCE_BYTES {
        return Err(WinxError::FileTooLarge {
            path,
            size: actual_size,
            max_size: MAX_SOURCE_BYTES,
        });
    }

    let format = image::guess_format(&bytes)
        .map_err(|error| image_error(&path, format!("unsupported or malformed image: {error}")))?;
    let mime_type = supported_mime(format).ok_or_else(|| {
        image_error(
            &path,
            format!("image format {format:?} is not supported; use JPEG, PNG, GIF, or WebP"),
        )
    })?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
    reader.limits(decode_limits());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| image_error(&path, format!("could not read image dimensions: {error}")))?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > MAX_SOURCE_PIXELS {
        return Err(image_error(
            &path,
            format!(
                "decoded image would contain {pixels} pixels; the safety limit is \
                 {MAX_SOURCE_PIXELS} pixels"
            ),
        ));
    }

    let (fingerprint, short_fingerprint) = fingerprint(&bytes);
    Ok(ImageSource {
        bytes,
        path,
        format,
        mime_type: mime_type.to_string(),
        width,
        height,
        fingerprint,
        short_fingerprint,
    })
}

fn decode_source(source: &ImageSource) -> Result<DynamicImage> {
    let mut reader = ImageReader::with_format(Cursor::new(source.bytes.as_slice()), source.format);
    reader.limits(decode_limits());
    reader
        .decode()
        .map_err(|error| image_error(&source.path, format!("could not decode image: {error}")))
}

fn flatten_onto_white(image: &DynamicImage) -> image::RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }

    let rgba = image.to_rgba8();
    let capacity = (rgba.width() as usize).saturating_mul(rgba.height() as usize).saturating_mul(3);
    let mut rgb = Vec::with_capacity(capacity);
    for pixel in rgba.pixels() {
        let alpha = u16::from(pixel[3]);
        let inverse = 255_u16.saturating_sub(alpha);
        for channel in &pixel.0[..3] {
            let blended = (u16::from(*channel) * alpha + 255 * inverse + 127) / 255;
            rgb.push(u8::try_from(blended).unwrap_or(255));
        }
    }
    image::RgbImage::from_raw(rgba.width(), rgba.height(), rgb)
        .unwrap_or_else(|| image::RgbImage::new(rgba.width(), rgba.height()))
}

fn encode_jpeg(image: &image::RgbImage, quality: u8, path: &Path) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, quality)
        .encode(image.as_raw(), image.width(), image.height(), ExtendedColorType::Rgb8)
        .map_err(|error| image_error(path, format!("could not encode bounded JPEG: {error}")))?;
    Ok(encoded)
}

fn metadata_for_duplicate(source: &ImageSource) -> ReadImageMetadata {
    ReadImageMetadata {
        source_bytes: source.bytes.len(),
        delivered_bytes: 0,
        base64_bytes: 0,
        source_width: source.width,
        source_height: source.height,
        delivered_width: 0,
        delivered_height: 0,
        source_mime_type: source.mime_type.clone(),
        delivered_mime_type: source.mime_type.clone(),
        transcoded: false,
        deduplicated: true,
        content_fingerprint: source.short_fingerprint.clone(),
    }
}

fn encode_delivery(source: ImageSource) -> Result<(String, String, ReadImageMetadata)> {
    let needs_transcode = source.bytes.len() > MAX_DELIVERED_BYTES
        || source.width > MAX_DELIVERED_DIMENSION
        || source.height > MAX_DELIVERED_DIMENSION;

    let (mime_type, delivered, delivered_width, delivered_height, transcoded) = if needs_transcode {
        let decoded = decode_source(&source)?;
        let mut edge = MAX_DELIVERED_DIMENSION;
        let bounded = loop {
            let candidate = if decoded.width() > edge || decoded.height() > edge {
                decoded.resize(edge, edge, FilterType::Triangle)
            } else {
                decoded.clone()
            };
            let rgb = flatten_onto_white(&candidate);
            let mut accepted = None;
            for quality in JPEG_QUALITIES {
                let encoded = encode_jpeg(&rgb, quality, &source.path)?;
                if encoded.len() <= MAX_DELIVERED_BYTES {
                    accepted = Some((encoded, rgb.width(), rgb.height()));
                    break;
                }
            }
            if let Some(accepted) = accepted {
                break accepted;
            }
            if edge <= MIN_TRANSCODE_DIMENSION {
                return Err(image_error(
                    &source.path,
                    format!(
                        "could not fit image within the {MAX_DELIVERED_BYTES}-byte delivery budget"
                    ),
                ));
            }
            edge = (edge * 3 / 4).max(MIN_TRANSCODE_DIMENSION);
        };
        ("image/jpeg".to_string(), bounded.0, bounded.1, bounded.2, true)
    } else {
        // `guess_format` and `into_dimensions` validate only the container
        // header. Decode once before passing original bytes through so a
        // truncated/corrupt payload is not mislabeled as a usable MCP image.
        drop(decode_source(&source)?);
        (source.mime_type.clone(), source.bytes.clone(), source.width, source.height, false)
    };

    let base64_data = general_purpose::STANDARD.encode(&delivered);
    let metadata = ReadImageMetadata {
        source_bytes: source.bytes.len(),
        delivered_bytes: delivered.len(),
        base64_bytes: base64_data.len(),
        source_width: source.width,
        source_height: source.height,
        delivered_width,
        delivered_height,
        source_mime_type: source.mime_type,
        delivered_mime_type: mime_type.clone(),
        transcoded,
        deduplicated: false,
        content_fingerprint: source.short_fingerprint,
    };
    Ok((mime_type, base64_data, metadata))
}

#[cfg(test)]
fn read_image_from_path_with_roots(
    file_path: &str,
    cwd: &Path,
    workspace_root: &Path,
    extra_roots: &[PathBuf],
) -> Result<(String, String, ReadImageMetadata)> {
    let source = read_image_source_impl(file_path, cwd, workspace_root, Some(extra_roots))?;
    encode_delivery(source)
}

/// Server-facing image delivery with content-based, per-session deduplication.
#[instrument(level = "info", skip(bash_state_arc, read_image))]
pub async fn handle_tool_call_detailed(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    read_image: ReadImage,
) -> Result<ReadImageDelivery> {
    info!(file_path = %read_image.file_path, force = read_image.force, "ReadImage tool called");
    let (cwd, workspace_root) = {
        let guard = bash_state_arc.lock().await;
        let Some(state) = guard.as_ref() else {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        };
        (state.cwd.clone(), state.workspace_root.clone())
    };

    let file_path = read_image.file_path;
    let source = tokio::task::spawn_blocking(move || {
        read_image_source_impl(&file_path, &cwd, &workspace_root, None)
    })
    .await
    .map_err(|error| {
        WinxError::CommandExecutionError(format!("ReadImage source worker failed: {error}"))
    })??;

    if !read_image.force {
        let already_delivered = {
            let mut guard = bash_state_arc.lock().await;
            let state = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
            state.image_was_delivered(&source.fingerprint)
        };
        if already_delivered {
            return Ok(ReadImageDelivery::AlreadyDelivered {
                metadata: metadata_for_duplicate(&source),
            });
        }
    }

    let delivered_fingerprint = source.fingerprint.clone();
    let (mime_type, base64_data, metadata) =
        tokio::task::spawn_blocking(move || encode_delivery(source)).await.map_err(|error| {
            WinxError::CommandExecutionError(format!("ReadImage encode worker failed: {error}"))
        })??;
    {
        let mut guard = bash_state_arc.lock().await;
        let state = guard.as_mut().ok_or(WinxError::BashStateNotInitialized)?;
        state.record_image_delivery(delivered_fingerprint);
    }
    Ok(ReadImageDelivery::Image { mime_type, base64_data, metadata })
}

/// Backwards-compatible text-free library API. Direct callers explicitly get
/// an image every time; MCP callers use `handle_tool_call_detailed` so repeat
/// deliveries can be compacted safely.
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    mut read_image: ReadImage,
) -> Result<(String, String)> {
    read_image.force = true;
    match handle_tool_call_detailed(bash_state_arc, read_image).await? {
        ReadImageDelivery::Image { mime_type, base64_data, .. } => Ok((mime_type, base64_data)),
        ReadImageDelivery::AlreadyDelivered { .. } => Err(WinxError::CommandExecutionError(
            "forced ReadImage delivery was unexpectedly deduplicated".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use image::{Rgba, RgbaImage};
    use tempfile::TempDir;

    use super::*;

    fn write_png(path: &Path, width: u32, height: u32) {
        let image = RgbaImage::from_pixel(width, height, Rgba([210, 40, 80, 255]));
        DynamicImage::ImageRgba8(image).save_with_format(path, ImageFormat::Png).unwrap();
    }

    #[test]
    fn reads_valid_image_inside_workspace() {
        let workspace = TempDir::new().unwrap();
        let image = workspace.path().join("shot.png");
        write_png(&image, 2, 2);
        let (mime, base64, metadata) = read_image_from_path_with_roots(
            image.to_str().unwrap(),
            workspace.path(),
            workspace.path(),
            &[],
        )
        .unwrap();
        assert_eq!(mime, "image/png");
        assert!(!base64.is_empty());
        assert!(!metadata.transcoded);
    }

    #[test]
    fn rejects_image_outside_workspace() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.png");
        write_png(&secret, 2, 2);
        let error = read_image_from_path_with_roots(
            secret.to_str().unwrap(),
            workspace.path(),
            workspace.path(),
            &[],
        );
        assert!(matches!(error, Err(WinxError::PathSecurityError { .. })));
    }

    #[test]
    fn large_dimensions_are_transcoded_into_the_delivery_budget() {
        let workspace = TempDir::new().unwrap();
        let image = workspace.path().join("wide.png");
        write_png(&image, MAX_DELIVERED_DIMENSION + 100, 32);
        let (mime, base64, metadata) = read_image_from_path_with_roots(
            image.to_str().unwrap(),
            workspace.path(),
            workspace.path(),
            &[],
        )
        .unwrap();
        assert_eq!(mime, "image/jpeg");
        assert!(metadata.transcoded);
        assert!(metadata.delivered_bytes <= MAX_DELIVERED_BYTES);
        assert!(metadata.delivered_width <= MAX_DELIVERED_DIMENSION);
        assert_eq!(metadata.base64_bytes, base64.len());
    }

    #[test]
    fn rejects_non_image_payload_instead_of_claiming_it_is_jpeg() {
        let workspace = TempDir::new().unwrap();
        let text = workspace.path().join("payload.txt");
        std::fs::write(&text, "not an image").unwrap();
        let error = read_image_from_path_with_roots(
            text.to_str().unwrap(),
            workspace.path(),
            workspace.path(),
            &[],
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsupported or malformed image"), "{error}");
    }

    #[test]
    fn rejects_truncated_image_after_a_valid_header() {
        let workspace = TempDir::new().unwrap();
        let image = workspace.path().join("truncated.png");
        write_png(&image, 32, 32);
        let mut bytes = std::fs::read(&image).unwrap();
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&image, bytes).unwrap();

        let error = read_image_from_path_with_roots(
            image.to_str().unwrap(),
            workspace.path(),
            workspace.path(),
            &[],
        )
        .unwrap_err();
        assert!(error.to_string().contains("could not decode image"), "{error}");
    }

    #[tokio::test]
    async fn repeated_delivery_is_compacted_unless_forced() {
        let workspace = TempDir::new().unwrap();
        let image = workspace.path().join("repeat.png");
        write_png(&image, 2, 2);
        let mut state = BashState::new();
        state.cwd = workspace.path().canonicalize().unwrap();
        state.workspace_root = state.cwd.clone();
        state.current_thread_id = "image-cache".to_string();
        state.initialized = true;
        let state = Arc::new(Mutex::new(Some(state)));
        let request = ReadImage {
            file_path: image.to_string_lossy().into_owned(),
            thread_id: "image-cache".to_string(),
            force: false,
        };

        assert!(matches!(
            handle_tool_call_detailed(&state, request.clone()).await.unwrap(),
            ReadImageDelivery::Image { .. }
        ));
        assert!(matches!(
            handle_tool_call_detailed(&state, request.clone()).await.unwrap(),
            ReadImageDelivery::AlreadyDelivered { .. }
        ));
        assert!(matches!(
            handle_tool_call_detailed(&state, ReadImage { force: true, ..request }).await.unwrap(),
            ReadImageDelivery::Image { .. }
        ));
    }
}
