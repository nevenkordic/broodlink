// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sandboxed filesystem operations for Broodlink agents.
//!
//! All path access is gated by configurable allow-lists and blocked-pattern
//! checks.  Intended to be shared by `a2a-gateway` (chat tools) and
//! `beads-bridge` (MCP tools).

use std::path::{Path, PathBuf};

/// File-name substrings that are always blocked regardless of directory.
const BLOCKED_PATTERNS: &[&str] = &[
    ".env",
    "secrets",
    "credentials",
    ".token",
    ".key",
    ".pem",
    "id_rsa",
    "id_ed25519",
    ".ssh",
    "password",
    ".age",
];

// ---------------------------------------------------------------------------
// Path validation
// ---------------------------------------------------------------------------

/// Validate that `path` is inside one of `allowed_dirs` and does not match
/// any blocked pattern.  Returns the canonicalized path on success.
pub fn validate_read_path(
    path: &str,
    allowed_dirs: &[String],
    max_size: u64,
) -> Result<PathBuf, String> {
    let path = expand_tilde(path);
    reject_traversal(&path)?;

    let canonical =
        std::fs::canonicalize(&path).map_err(|e| format!("cannot resolve path: {e}"))?;

    check_allowed(&canonical, allowed_dirs)?;
    check_blocked(&canonical)?;

    let meta = std::fs::metadata(&canonical).map_err(|e| format!("cannot stat file: {e}"))?;
    if !meta.is_file() {
        return Err("path is not a regular file".to_string());
    }
    if meta.len() > max_size {
        return Err(format!(
            "file too large ({} bytes, max {})",
            meta.len(),
            max_size
        ));
    }

    Ok(canonical)
}

/// Validate that `path` is inside one of `allowed_dirs` for writing.
/// The file need not exist yet, but the parent directory must.
pub fn validate_write_path(
    path: &str,
    allowed_dirs: &[String],
    content_size: u64,
    max_size: u64,
) -> Result<PathBuf, String> {
    let path = expand_tilde(path);
    reject_traversal(&path)?;

    if content_size > max_size {
        return Err(format!(
            "content too large ({content_size} bytes, max {max_size})"
        ));
    }

    let p = Path::new(&path);
    let parent = p
        .parent()
        .ok_or_else(|| "invalid path: no parent directory".to_string())?;
    let file_name = p
        .file_name()
        .ok_or_else(|| "invalid path: no file name".to_string())?;

    let canonical_parent =
        std::fs::canonicalize(parent).map_err(|e| format!("parent directory not found: {e}"))?;
    let canonical = canonical_parent.join(file_name);

    check_allowed(&canonical, allowed_dirs)?;
    check_blocked(&canonical)?;

    Ok(canonical)
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/// Read a file as UTF-8 text (falls back to lossy conversion for non-UTF8).
pub fn read_file_safe(path: &Path, max_size: u64) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot stat: {e}"))?;
    if meta.len() > max_size {
        return Err(format!(
            "file too large ({} bytes, max {})",
            meta.len(),
            max_size
        ));
    }

    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(_) => {
            // Non-UTF8 — try lossy
            let bytes = std::fs::read(path).map_err(|e| format!("read error: {e}"))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

/// Atomically write content to a file.  Creates parent directories if needed.
pub fn write_file_safe(path: &Path, content: &[u8], max_size: u64) -> Result<(), String> {
    if content.len() as u64 > max_size {
        return Err(format!(
            "content too large ({} bytes, max {})",
            content.len(),
            max_size
        ));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create directories: {e}"))?;
    }

    // Atomic write: tmp file + rename
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, content).map_err(|e| format!("write error: {e}"))?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        // Clean up tmp on rename failure
        let _ = std::fs::remove_file(&tmp_path);
        format!("rename error: {e}")
    })?;

    Ok(())
}

/// Extract text from a Word (.docx) file.
///
/// DOCX files are ZIP archives containing `word/document.xml`.
/// We extract text from `<w:t>` elements, inserting paragraph breaks at `</w:p>`.
pub fn read_docx_safe(path: &Path, max_chars: usize) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("cannot open file: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a valid DOCX: {e}"))?;

    let mut xml = String::new();
    {
        let mut doc = archive
            .by_name("word/document.xml")
            .map_err(|_| "DOCX missing word/document.xml".to_string())?;
        std::io::Read::read_to_string(&mut doc, &mut xml)
            .map_err(|e| format!("read error: {e}"))?;
    }

    let text = extract_text_from_docx_xml(&xml);

    if text.len() > max_chars {
        let truncated = match text[..max_chars].rfind('\n') {
            Some(idx) => &text[..idx],
            None => &text[..max_chars],
        };
        Ok(format!(
            "{}\n\n[Truncated: showing first ~{} characters of {}]",
            truncated,
            max_chars,
            text.len()
        ))
    } else {
        Ok(text)
    }
}

/// Simple XML text extraction for DOCX document.xml.
fn extract_text_from_docx_xml(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len() / 4);
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut in_wt = false;

    for ch in xml.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
            continue;
        }
        if ch == '>' {
            in_tag = false;
            // Check if this is a <w:t> opening or </w:t> closing
            let tag = tag_buf.trim();
            if tag == "w:t" || tag.starts_with("w:t ") {
                in_wt = true;
            } else if tag == "/w:t" {
                in_wt = false;
            } else if tag == "/w:p" {
                // Paragraph break
                result.push('\n');
            } else if tag == "w:br" || tag == "w:br/" || tag == "w:br /" {
                result.push('\n');
            }
            continue;
        }
        if in_tag {
            tag_buf.push(ch);
        } else if in_wt {
            result.push(ch);
        }
    }

    // Decode common XML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Extract text from a PDF file, truncating to approximately `max_pages` pages.
pub fn read_pdf_safe(path: &Path, max_pages: u32) -> Result<String, String> {
    let text =
        pdf_extract::extract_text(path).map_err(|e| format!("PDF extraction failed: {e}"))?;

    let max_chars = max_pages as usize * 3000;
    if text.len() > max_chars {
        // Find a clean break point (newline) near the limit
        let truncated = match text[..max_chars].rfind('\n') {
            Some(idx) => &text[..idx],
            None => &text[..max_chars],
        };
        Ok(format!(
            "{}\n\n[Truncated: showing approximately first {} pages]",
            truncated, max_pages
        ))
    } else {
        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{}", home.to_string_lossy(), rest);
        }
    }
    path.to_string()
}

fn reject_traversal(path: &str) -> Result<(), String> {
    if path.contains("..") {
        return Err("path traversal (..) is not allowed".to_string());
    }
    Ok(())
}

fn check_allowed(canonical: &Path, allowed_dirs: &[String]) -> Result<(), String> {
    let canonical_str = canonical.to_string_lossy();

    for dir in allowed_dirs {
        let expanded = expand_tilde(dir);
        if let Ok(dir_canonical) = std::fs::canonicalize(&expanded) {
            let dir_str = dir_canonical.to_string_lossy();
            if canonical_str.starts_with(dir_str.as_ref()) {
                return Ok(());
            }
        }
    }

    Err("path is outside allowed directories".to_string())
}

fn check_blocked(canonical: &Path) -> Result<(), String> {
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    for pattern in BLOCKED_PATTERNS {
        if name.contains(pattern) {
            return Err("access to sensitive files is blocked".to_string());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reject_traversal() {
        assert!(reject_traversal("../etc/passwd").is_err());
        assert!(reject_traversal("/foo/../../bar").is_err());
        assert!(reject_traversal("/foo/bar").is_ok());
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/test/file.txt");
        assert!(!expanded.starts_with("~/"), "tilde should be expanded");
        assert!(expanded.ends_with("/test/file.txt"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn test_check_blocked_env() {
        let path = PathBuf::from("/some/dir/.env");
        assert!(check_blocked(&path).is_err());
    }

    #[test]
    fn test_check_blocked_token() {
        let path = PathBuf::from("/some/dir/jwt.token");
        assert!(check_blocked(&path).is_err());
    }

    #[test]
    fn test_check_blocked_ssh_key() {
        let path = PathBuf::from("/home/user/id_rsa");
        assert!(check_blocked(&path).is_err());
    }

    #[test]
    fn test_check_blocked_clean_file() {
        let path = PathBuf::from("/some/dir/readme.md");
        assert!(check_blocked(&path).is_ok());
    }

    #[test]
    fn test_read_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize the tempdir path to handle macOS /var -> /private/var symlink
        let dir_canonical = std::fs::canonicalize(dir.path()).unwrap();
        let file = dir_canonical.join("test.txt");
        let content = b"hello broodlink";
        let allowed = vec![dir_canonical.to_string_lossy().to_string()];

        write_file_safe(&file, content, 1024).unwrap();
        let read = read_file_safe(&file, 1024).unwrap();
        assert_eq!(read, "hello broodlink");

        // Validate paths
        let canonical = validate_read_path(&file.to_string_lossy(), &allowed, 1024).unwrap();
        assert_eq!(canonical, file);
    }

    #[test]
    fn test_write_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.txt");
        let content = vec![b'x'; 2000];
        let result = write_file_safe(&file, &content, 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn test_read_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.txt");
        std::fs::write(&file, vec![b'x'; 2000]).unwrap();
        let result = read_file_safe(&file, 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn test_validate_read_outside_allowed() {
        let result = validate_read_path("/etc/hosts", &["/tmp".to_string()], 1_000_000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed"));
    }

    #[test]
    fn test_validate_write_outside_allowed() {
        let result = validate_write_path("/etc/test.txt", &["/tmp".to_string()], 10, 1024);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed"));
    }

    #[test]
    fn test_validate_write_content_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = vec![dir.path().to_string_lossy().to_string()];
        let file = dir.path().join("test.txt");
        let result = validate_write_path(&file.to_string_lossy(), &allowed, 2000, 1000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn test_pdf_extraction_nonexistent() {
        let result = read_pdf_safe(Path::new("/nonexistent/file.pdf"), 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_docx_extraction_nonexistent() {
        let result = read_docx_safe(Path::new("/nonexistent/file.docx"), 100_000);
        assert!(result.is_err());
    }

    #[test]
    fn test_docx_extraction_not_zip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fake.docx");
        std::fs::write(&file, "this is not a zip file").unwrap();
        let result = read_docx_safe(&file, 100_000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a valid DOCX"));
    }

    #[test]
    fn test_extract_text_from_docx_xml() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> World</w:t></w:r></w:p><w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p></w:body></w:document>"#;
        let text = extract_text_from_docx_xml(xml);
        assert_eq!(text, "Hello World\nSecond paragraph\n");
    }

    #[test]
    fn test_extract_text_xml_entities() {
        let xml = r#"<w:p><w:r><w:t>A &amp; B &lt; C</w:t></w:r></w:p>"#;
        let text = extract_text_from_docx_xml(xml);
        assert_eq!(text, "A & B < C\n");
    }

    #[test]
    fn test_validate_read_blocked_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".env");
        std::fs::write(&file, "SECRET=foo").unwrap();
        let allowed = vec![dir.path().to_string_lossy().to_string()];
        let result = validate_read_path(&file.to_string_lossy(), &allowed, 1_000_000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("sensitive"));
    }
}
