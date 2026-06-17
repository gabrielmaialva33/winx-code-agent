//! Implementation of the `ReadImage` tool.
//!
//! This module provides the implementation for the `ReadImage` tool, which is used
//! to read image files and return their contents as base64-encoded data with
//! the appropriate MIME type.

use base64::{engine::general_purpose, Engine};
use mime_guess::MimeGuess;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadImage;
use crate::utils::path::{expand_user, validate_path_in_workspace};

/// Supported MIME types for images
pub const SUPPORTED_MIME_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Refuse to read images larger than this. base64 encoding inflates the
/// footprint ~1.37×, so a few-hundred-MB file would spike to gigabytes; under
/// `panic = "abort"` an allocation failure aborts the whole server. Real images
/// (screenshots, mockups, error PNGs) are orders of magnitude smaller.
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

/// Read an image from the file system
///
/// This function reads an image file, base64 encodes its contents, and
/// determines the MIME type based on the file extension.
///
/// # Arguments
///
/// * `file_path` - Path to the image file
/// * `cwd` - Current working directory for resolving relative paths
///
/// # Returns
///
/// A tuple containing:
/// - The MIME type of the image
/// - The base64-encoded image data
///
/// # Errors
///
/// Returns an error if the file cannot be accessed or read
#[instrument(level = "debug", skip(file_path))]
fn read_image_from_path(
    file_path: &str,
    cwd: &Path,
    workspace_root: &Path,
) -> Result<(String, String)> {
    debug!("Reading image: {}", file_path);

    // Expand the path
    let file_path = expand_user(file_path);

    // Ensure path is absolute
    let path = if Path::new(&file_path).is_absolute() {
        PathBuf::from(&file_path)
    } else {
        // Use current working directory if path is relative
        cwd.join(&file_path)
    };

    // Confine to the workspace and resolve symlinks BEFORE touching the file —
    // the same guarantee ReadFiles/ContextSave give. Without this, ReadImage is
    // an arbitrary-file read primitive (`/etc/shadow`, `~/.ssh/*`), and over the
    // network-reachable HTTP transport that means remote exfiltration.
    let path = validate_path_in_workspace(&path, workspace_root)
        .map_err(|e| WinxError::PathSecurityError { path: path.clone(), message: e.to_string() })?;

    // Ensure it's a regular file (also rejects a path that doesn't exist).
    if !path.is_file() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: "File does not exist or is not a regular file".to_string(),
        });
    }

    // Cap the size before reading it into memory + base64-encoding it.
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    if size > MAX_IMAGE_BYTES {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: format!("Image too large: {size} bytes (max {MAX_IMAGE_BYTES} bytes)"),
        });
    }

    // Read the file as bytes
    let image_bytes = std::fs::read(&path).map_err(|e| WinxError::FileAccessError {
        path: path.clone(),
        message: format!("Error reading file: {e}"),
    })?;

    // Encode the bytes to base64
    let image_b64 = general_purpose::STANDARD.encode(&image_bytes);

    // Guess the MIME type from the file extension
    let mime_type =
        MimeGuess::from_path(&path).first_raw().unwrap_or("application/octet-stream").to_string();

    // Verify the MIME type is a supported image type
    if SUPPORTED_MIME_TYPES.contains(&mime_type.as_str()) {
        Ok((mime_type, image_b64))
    } else {
        debug!("Detected MIME type '{}' is not in the supported list", mime_type);
        // Fall back to a best effort based on common extensions
        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_lowercase();

        let mime_type = match extension.as_str() {
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "image/jpeg", // Default fallback
        };

        debug!("Using fallback MIME type: {}", mime_type);
        Ok((mime_type.to_string(), image_b64))
    }
}

/// Handle the `ReadImage` tool call
///
/// This function processes the `ReadImage` tool call, which reads an image file
/// and returns its contents as base64-encoded data with the appropriate MIME type.
///
/// # Arguments
///
/// * `bash_state_arc` - Shared reference to the bash state
/// * `read_image` - The read image parameters
///
/// # Returns
///
/// A Result containing a tuple with the MIME type and base64-encoded image data
///
/// # Errors
///
/// Returns an error if the image file cannot be accessed or read
#[instrument(level = "info", skip(bash_state_arc, read_image))]
pub async fn handle_tool_call(
    bash_state_arc: &Arc<Mutex<Option<BashState>>>,
    read_image: ReadImage,
) -> Result<(String, String)> {
    info!("ReadImage tool called with: {:?}", read_image);

    // We need to extract data from the bash state before awaiting
    // to avoid holding the MutexGuard across await points
    let cwd: PathBuf;
    let workspace_root: PathBuf;

    // Lock bash state to extract data
    {
        let bash_state_guard = bash_state_arc.lock().await;

        // Ensure bash state is initialized
        let Some(bash_state) = &*bash_state_guard else {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        };

        // Extract needed data
        cwd = bash_state.cwd.clone();
        workspace_root = bash_state.workspace_root.clone();
    }

    // Read the image file
    read_image_from_path(&read_image.file_path, &cwd, &workspace_root)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn reads_image_inside_workspace() {
        let ws = TempDir::new().unwrap();
        let img = ws.path().join("shot.png");
        fs::write(&img, b"\x89PNG\r\n\x1a\nfake").unwrap();
        let (mime, b64) =
            read_image_from_path(img.to_str().unwrap(), ws.path(), ws.path()).unwrap();
        assert_eq!(mime, "image/png");
        assert!(!b64.is_empty());
    }

    #[test]
    fn rejects_image_outside_workspace() {
        // The exfil case: an absolute path outside the workspace must be refused,
        // not read (this is what made ReadImage an arbitrary-file-read primitive).
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.png");
        fs::write(&secret, b"\x89PNG secret").unwrap();
        let err = read_image_from_path(secret.to_str().unwrap(), ws.path(), ws.path());
        assert!(matches!(err, Err(WinxError::PathSecurityError { .. })));
    }
}
