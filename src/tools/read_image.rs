//! Implementation of the ReadImage tool.
//!
//! This module provides the implementation for the ReadImage tool, which is used
//! to read image files and return their contents as base64-encoded data with
//! the appropriate MIME type.

use base64::{Engine, engine::general_purpose};
use mime_guess::MimeGuess;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, instrument};

use crate::errors::{Result, WinxError};
use crate::state::bash_state::BashState;
use crate::types::ReadImage;
use crate::utils::path::expand_user;

/// Supported MIME types for images
pub const SUPPORTED_MIME_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

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
fn read_image_from_path(file_path: &str, cwd: &Path) -> Result<(String, String)> {
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

    // Check if path exists
    if !path.exists() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: Arc::new("File does not exist".to_string()),
        });
    }

    // Ensure it's a file
    if !path.is_file() {
        return Err(WinxError::FileAccessError {
            path: path.clone(),
            message: Arc::new("Path exists but is not a file".to_string()),
        });
    }

    // Read the file as bytes
    let image_bytes = std::fs::read(&path).map_err(|e| WinxError::FileAccessError {
        path: path.clone(),
        message: Arc::new(format!("Error reading file: {}", e)),
    })?;

    // Encode the bytes to base64
    let image_b64 = general_purpose::STANDARD.encode(&image_bytes);

    // Guess the MIME type from the file extension
    let mime_type = MimeGuess::from_path(&path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string();

    // Verify the MIME type is a supported image type
    if !SUPPORTED_MIME_TYPES.contains(&mime_type.as_str()) {
        debug!(
            "Detected MIME type '{}' is not in the supported list",
            mime_type
        );
        // Fall back to a best effort based on common extensions
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();

        let mime_type = match extension.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "image/jpeg", // Default fallback
        };

        debug!("Using fallback MIME type: {}", mime_type);
        Ok((mime_type.to_string(), image_b64))
    } else {
        Ok((mime_type.to_string(), image_b64))
    }
}

/// Handle the ReadImage tool call
///
/// This function processes the ReadImage tool call, which reads an image file
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
    

    // Lock bash state to extract data
    let bash_state_guard = bash_state_arc.lock().await;

    // Ensure bash state is initialized
    let bash_state = match &*bash_state_guard {
        Some(state) => state,
        None => {
            error!("BashState not initialized");
            return Err(WinxError::BashStateNotInitialized);
        }
    };

    // Extract needed data
    let cwd: PathBuf = bash_state.cwd.clone();

    // Read the image file
    read_image_from_path(&read_image.file_path, &cwd)
}
