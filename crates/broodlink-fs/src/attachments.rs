// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attachment storage: date-partitioned local filesystem with SHA-256 dedup
//! and optional thumbnail generation.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Store attachment bytes to disk under `attachments_dir/{YYYY-MM}/{uuid}.{ext}`.
/// Returns `(relative_path, sha256_hex)` on success.
pub fn store_attachment(
    attachments_dir: &str,
    file_name: &str,
    bytes: &[u8],
    max_size: u64,
) -> Result<(String, String), String> {
    if bytes.is_empty() {
        return Err("empty attachment".to_string());
    }
    if bytes.len() as u64 > max_size {
        return Err(format!(
            "attachment too large ({} bytes, max {max_size})",
            bytes.len()
        ));
    }

    let dir = crate::expand_tilde(attachments_dir);
    let now = chrono::Utc::now();
    let partition = now.format("%Y-%m").to_string();
    let sub_dir = PathBuf::from(&dir).join(&partition);
    std::fs::create_dir_all(&sub_dir)
        .map_err(|e| format!("cannot create attachments dir {}: {e}", sub_dir.display()))?;

    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let uuid = uuid::Uuid::new_v4();
    let file_name_on_disk = format!("{uuid}.{ext}");
    let full_path = sub_dir.join(&file_name_on_disk);

    let hash = hex::encode(Sha256::digest(bytes));

    // Atomic write: temp file then rename
    let tmp_path = full_path.with_extension("tmp");
    std::fs::write(&tmp_path, bytes).map_err(|e| format!("write failed: {e}"))?;
    std::fs::rename(&tmp_path, &full_path).map_err(|e| format!("rename failed: {e}"))?;

    let relative = format!("{partition}/{file_name_on_disk}");
    Ok((relative, hash))
}

/// Read attachment bytes from disk.
pub fn read_attachment(attachments_dir: &str, relative_path: &str) -> Result<Vec<u8>, String> {
    let dir = crate::expand_tilde(attachments_dir);
    let full = PathBuf::from(&dir).join(relative_path);

    if relative_path.contains("..") {
        return Err("path traversal not allowed".to_string());
    }

    std::fs::read(&full).map_err(|e| format!("read attachment failed: {e}"))
}

/// Generate a JPEG thumbnail for an image attachment.
/// Returns the relative path to the thumbnail on success.
pub fn generate_thumbnail(
    attachments_dir: &str,
    source_relative: &str,
    max_width: u32,
) -> Result<String, String> {
    let dir = crate::expand_tilde(attachments_dir);
    let source = PathBuf::from(&dir).join(source_relative);

    let img = image::open(&source).map_err(|e| format!("open image failed: {e}"))?;

    let thumb = img.thumbnail(max_width, max_width);

    // Store thumbnail next to original with _thumb suffix
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("thumb");
    let thumb_name = format!("{stem}_thumb.jpg");
    let thumb_path = source.with_file_name(&thumb_name);
    thumb
        .save_with_format(&thumb_path, image::ImageFormat::Jpeg)
        .map_err(|e| format!("save thumbnail failed: {e}"))?;

    // Build relative path
    let partition = Path::new(source_relative)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if partition.is_empty() {
        Ok(thumb_name)
    } else {
        Ok(format!("{partition}/{thumb_name}"))
    }
}

/// Compute SHA-256 hex digest of bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_attachment_path_format() {
        let dir = tempfile::tempdir().unwrap();
        let (path, hash) = store_attachment(
            dir.path().to_str().unwrap(),
            "photo.jpg",
            b"fake-jpeg-data",
            1_000_000,
        )
        .unwrap();

        // Path format: YYYY-MM/uuid.jpg
        assert!(path.ends_with(".jpg"), "path should end with .jpg: {path}");
        let parts: Vec<&str> = path.split('/').collect();
        assert_eq!(parts.len(), 2, "should have partition/filename");
        assert!(parts[0].len() == 7, "partition should be YYYY-MM");
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64, "SHA-256 hex should be 64 chars");

        // File should exist on disk
        let full = dir.path().join(&path);
        assert!(full.exists());
        assert_eq!(std::fs::read(&full).unwrap(), b"fake-jpeg-data");
    }

    #[test]
    fn test_store_attachment_hash_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"identical-content";
        let (_, hash1) =
            store_attachment(dir.path().to_str().unwrap(), "a.txt", data, 1_000_000).unwrap();
        let (_, hash2) =
            store_attachment(dir.path().to_str().unwrap(), "b.txt", data, 1_000_000).unwrap();
        assert_eq!(hash1, hash2, "identical content should produce same hash");
    }

    #[test]
    fn test_store_attachment_rejects_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let result = store_attachment(dir.path().to_str().unwrap(), "big.bin", &[0u8; 100], 50);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn test_store_attachment_rejects_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = store_attachment(dir.path().to_str().unwrap(), "empty.bin", &[], 1_000_000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_read_attachment_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = store_attachment(
            dir.path().to_str().unwrap(),
            "doc.pdf",
            b"pdf-content",
            1_000_000,
        )
        .unwrap();
        let data = read_attachment(dir.path().to_str().unwrap(), &path).unwrap();
        assert_eq!(data, b"pdf-content");
    }

    #[test]
    fn test_read_attachment_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_attachment(dir.path().to_str().unwrap(), "../etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("traversal"));
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"hello");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_generate_thumbnail_invalid_image() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = store_attachment(
            dir.path().to_str().unwrap(),
            "notimage.jpg",
            b"not-image-data",
            1_000_000,
        )
        .unwrap();
        let result = generate_thumbnail(dir.path().to_str().unwrap(), &path, 200);
        assert!(result.is_err());
    }

    #[test]
    fn test_store_attachment_no_extension() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) =
            store_attachment(dir.path().to_str().unwrap(), "noext", b"data", 1_000_000).unwrap();
        assert!(
            path.ends_with(".bin"),
            "no extension should default to .bin"
        );
    }
}
