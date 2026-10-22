//! Bounded local extraction for document attachments.
//!
//! This is deliberately a tool-side representation step, not a provider
//! upload protocol. The extractor reads a managed host file, returns a
//! bounded derived text representation, and leaves the caller to add the
//! normal untrusted-content fence. Unsupported/encrypted documents are
//! reported explicitly instead of being treated as empty text.

#[cfg(test)]
use std::fs::File;
#[cfg(test)]
use std::io::Read;
use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::cancellable_reader::CancellableReader;

mod open_xml;
mod pdf;

/// Hard upper bound for one document read, independent of the model context
/// budget. This protects the local parser and ZIP decompression path from
/// untrusted attachment sizes and compression ratios.
pub const MAX_DOCUMENT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
}

impl DocumentFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
        }
    }

    pub const fn representation(self) -> &'static str {
        match self {
            Self::Xlsx => "table_data",
            Self::Pdf | Self::Docx | Self::Pptx => "document_pages",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentExtraction {
    pub format: DocumentFormat,
    pub representation: &'static str,
    pub text: String,
    pub sections: usize,
    pub size_bytes: u64,
    pub truncated: bool,
    /// Zero-based logical page/section selected for the model view.
    pub page_index: usize,
    /// Total logical pages/sections discovered by the bounded extractor.
    pub total_pages: usize,
    /// Cursor for the next page, if another page is available.
    pub next_page: Option<usize>,
}

/// Whether the bounded extractor has a parser for this path's extension.
/// Callers use this to distinguish a deliberately unsupported format from a
/// supported document that failed validation or parsing.
pub fn supports_document_path(path: &Path) -> bool {
    format_for_path(path).is_some()
}

/// Extract a bounded textual representation from a supported document.
#[cfg(test)]
pub fn extract_document(
    path: &Path,
    max_chars: usize,
    max_document_bytes: u64,
) -> anyhow::Result<DocumentExtraction> {
    extract_document_inner(path, max_chars, max_document_bytes, None, None)
}

/// Extract one logical page/section. The returned `next_page` is an explicit
/// zero-based cursor; callers do not need to split the aggregate text or
/// guess how a PDF/Office parser partitioned the document.
#[cfg(test)]
pub fn extract_document_page_with_cancel(
    path: &Path,
    max_chars: usize,
    max_document_bytes: u64,
    page_index: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<DocumentExtraction> {
    extract_document_inner(
        path,
        max_chars,
        max_document_bytes,
        Some(cancel),
        Some(page_index),
    )
}

/// Extract a document snapshot that was read from an already-validated file
/// handle. The supplied path is used only to select the document format; no
/// file is reopened during parsing.
pub fn extract_document_page_from_bytes_with_cancel(
    path: &Path,
    bytes: &[u8],
    max_chars: usize,
    max_document_bytes: u64,
    page_index: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<DocumentExtraction> {
    extract_document_bytes_inner(
        path,
        bytes,
        max_chars,
        max_document_bytes,
        Some(cancel),
        Some(page_index),
    )
}

/// Extract all logical pages from an already bounded remote document body.
/// The caller has already read the bytes through its network destination and
/// response-size policy; this function applies the same parser and byte cap as
/// managed local documents.
pub fn extract_document_from_bytes_with_cancel(
    path: &Path,
    bytes: &[u8],
    max_chars: usize,
    max_document_bytes: u64,
    cancel: &CancellationToken,
) -> anyhow::Result<DocumentExtraction> {
    extract_document_bytes_inner(
        path,
        bytes,
        max_chars,
        max_document_bytes,
        Some(cancel),
        None,
    )
}

#[cfg(test)]
fn extract_document_inner(
    path: &Path,
    max_chars: usize,
    max_document_bytes: u64,
    cancel: Option<&CancellationToken>,
    page_index: Option<usize>,
) -> anyhow::Result<DocumentExtraction> {
    check_cancel(cancel)?;
    let _format = format_for_path(path)
        .ok_or_else(|| anyhow::anyhow!("document extraction is unsupported for this file type"))?;
    let metadata = std::fs::metadata(path)?;
    let size_bytes = metadata.len();
    let byte_limit = max_document_bytes.min(MAX_DOCUMENT_BYTES);
    if size_bytes > byte_limit {
        anyhow::bail!("document exceeds the local extraction size limit");
    }

    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(size_bytes.min(byte_limit) as usize);
    cancellable_document_reader(file, cancel)
        .take(byte_limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > byte_limit {
        anyhow::bail!("document exceeds the local extraction size limit");
    }

    extract_document_bytes_inner(path, &bytes, max_chars, byte_limit, cancel, page_index)
}

fn extract_document_bytes_inner(
    path: &Path,
    bytes: &[u8],
    max_chars: usize,
    max_document_bytes: u64,
    cancel: Option<&CancellationToken>,
    page_index: Option<usize>,
) -> anyhow::Result<DocumentExtraction> {
    check_cancel(cancel)?;
    let format = format_for_path(path)
        .ok_or_else(|| anyhow::anyhow!("document extraction is unsupported for this file type"))?;
    let byte_limit = max_document_bytes.min(MAX_DOCUMENT_BYTES);
    let size_bytes = bytes.len() as u64;
    if size_bytes > byte_limit {
        anyhow::bail!("document exceeds the local extraction size limit");
    }

    let pages = match format {
        DocumentFormat::Pdf => pdf::extract_pdf(bytes, cancel)?,
        DocumentFormat::Docx | DocumentFormat::Xlsx | DocumentFormat::Pptx => {
            open_xml::extract_open_xml(bytes, format, cancel)?
        }
    };
    let total_pages = pages.len().max(1);
    let selected_page = page_index.unwrap_or(0);
    if selected_page >= total_pages {
        anyhow::bail!(
            "page_index {} is outside the document (total_pages={})",
            selected_page,
            total_pages
        );
    }
    let text = page_index
        .and_then(|index| pages.get(index).cloned())
        .unwrap_or_else(|| pages.join("\n"));
    let output = haven_common::encoding::truncate_output(&normalize_text(&text), max_chars);
    if output.text.trim().is_empty() {
        anyhow::bail!("document contains no extractable text");
    }
    Ok(DocumentExtraction {
        format,
        representation: format.representation(),
        text: output.text,
        sections: total_pages,
        size_bytes,
        truncated: output.truncated,
        page_index: selected_page,
        total_pages,
        next_page: page_index.and_then(|index| (index + 1 < total_pages).then_some(index + 1)),
    })
}

fn check_cancel(cancel: Option<&CancellationToken>) -> anyhow::Result<()> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        anyhow::bail!("document extraction cancelled");
    }
    Ok(())
}

fn cancellable_document_reader<R>(
    reader: R,
    cancel: Option<&CancellationToken>,
) -> CancellableReader<R> {
    CancellableReader::new(reader, cancel).with_cancel_error(
        std::io::ErrorKind::Interrupted,
        "document extraction cancelled",
    )
}

fn format_for_path(path: &Path) -> Option<DocumentFormat> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => Some(DocumentFormat::Pdf),
        "docx" => Some(DocumentFormat::Docx),
        "xlsx" => Some(DocumentFormat::Xlsx),
        "pptx" => Some(DocumentFormat::Pptx),
        _ => None,
    }
}

fn normalize_text(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut previous_blank = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            if !previous_blank && !normalized.is_empty() {
                normalized.push('\n');
            }
            previous_blank = true;
            continue;
        }
        if !normalized.is_empty() && !normalized.ends_with('\n') {
            normalized.push('\n');
        }
        normalized.push_str(line);
        normalized.push('\n');
        previous_blank = false;
    }
    normalized.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    #[test]
    fn extracts_uncompressed_pdf_text_without_paths() {
        let mut file = NamedTempFile::with_suffix(".pdf").unwrap();
        let body = b"BT\n/F1 12 Tf\n72 720 Td\n(Hello PDF) Tj\nET\n";
        writeln!(file, "%PDF-1.4").unwrap();
        writeln!(file, "1 0 obj").unwrap();
        writeln!(file, "<< /Length {} >>", body.len()).unwrap();
        writeln!(file, "stream").unwrap();
        file.write_all(body).unwrap();
        writeln!(file, "endstream").unwrap();
        writeln!(file, "endobj").unwrap();
        file.flush().unwrap();

        let extracted = extract_document(file.path(), 1_000, MAX_DOCUMENT_BYTES).unwrap();
        assert_eq!(extracted.format, DocumentFormat::Pdf);
        assert_eq!(extracted.text, "Hello PDF");
    }

    #[test]
    fn extracts_from_the_verified_byte_snapshot_without_reopening_the_path() {
        let path = Path::new("snapshot.pdf");
        let body = b"BT\n(Page from verified handle) Tj\nET\n";
        let mut bytes =
            format!("%PDF-1.4\n1 0 obj\n<< /Length {} >>\nstream\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"endstream\nendobj\n");

        let extracted = extract_document_page_from_bytes_with_cancel(
            path,
            &bytes,
            1_000,
            MAX_DOCUMENT_BYTES,
            0,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(extracted.text, "Page from verified handle");
    }

    #[test]
    fn page_cursor_returns_one_logical_pdf_stream_at_a_time() {
        let mut file = NamedTempFile::with_suffix(".pdf").unwrap();
        let pages = [
            b"BT\n(Page one) Tj\nET\n".as_slice(),
            b"BT\n(Page two) Tj\nET\n".as_slice(),
        ];
        writeln!(file, "%PDF-1.4").unwrap();
        for (index, body) in pages.iter().enumerate() {
            writeln!(file, "{} 0 obj", index + 1).unwrap();
            writeln!(file, "<< /Length {} >>", body.len()).unwrap();
            writeln!(file, "stream").unwrap();
            file.write_all(body).unwrap();
            writeln!(file, "endstream").unwrap();
            writeln!(file, "endobj").unwrap();
        }
        file.flush().unwrap();

        let cancel = CancellationToken::new();
        let first =
            extract_document_page_with_cancel(file.path(), 1_000, MAX_DOCUMENT_BYTES, 0, &cancel)
                .unwrap();
        assert_eq!(first.text, "Page one");
        assert_eq!(first.page_index, 0);
        assert_eq!(first.total_pages, 2);
        assert_eq!(first.next_page, Some(1));

        let second =
            extract_document_page_with_cancel(file.path(), 1_000, MAX_DOCUMENT_BYTES, 1, &cancel)
                .unwrap();
        assert_eq!(second.text, "Page two");
        assert_eq!(second.page_index, 1);
        assert_eq!(second.total_pages, 2);
        assert_eq!(second.next_page, None);

        let error =
            extract_document_page_with_cancel(file.path(), 1_000, MAX_DOCUMENT_BYTES, 2, &cancel)
                .unwrap_err()
                .to_string();
        assert!(error.contains("page_index 2"));
    }

    #[test]
    fn rejects_encrypted_pdf_explicitly() {
        let mut file = NamedTempFile::with_suffix(".pdf").unwrap();
        file.write_all(b"%PDF-1.7\n/Encrypt\n").unwrap();
        file.flush().unwrap();
        let error = extract_document(file.path(), 1_000, MAX_DOCUMENT_BYTES)
            .unwrap_err()
            .to_string();
        assert!(error.contains("encrypted PDF"));
    }

    #[test]
    fn extracts_flate_compressed_pdf_text() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"BT\n(Compressed PDF) Tj\nET\n").unwrap();
        let compressed = encoder.finish().unwrap();
        let mut file = NamedTempFile::with_suffix(".pdf").unwrap();
        writeln!(file, "%PDF-1.4").unwrap();
        writeln!(file, "1 0 obj").unwrap();
        writeln!(
            file,
            "<< /Length {} /Filter /FlateDecode >>",
            compressed.len()
        )
        .unwrap();
        writeln!(file, "stream").unwrap();
        file.write_all(&compressed).unwrap();
        writeln!(file, "\nendstream").unwrap();
        file.flush().unwrap();

        let extracted = extract_document(file.path(), 1_000, MAX_DOCUMENT_BYTES).unwrap();
        assert_eq!(extracted.text, "Compressed PDF");
    }

    #[test]
    fn extracts_docx_text_from_bounded_xml_parts() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("report.docx");
        let handle = File::create(&path).unwrap();
        let mut archive = ZipWriter::new(handle);
        archive
            .start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(
                br#"<w:document xmlns:w="urn"><w:body><w:p><w:r><w:t>Hello &amp; world</w:t></w:r></w:p></w:body></w:document>"#,
            )
            .unwrap();
        archive.finish().unwrap();

        let extracted = extract_document(&path, 1_000, MAX_DOCUMENT_BYTES).unwrap();
        assert_eq!(extracted.format, DocumentFormat::Docx);
        assert_eq!(extracted.text, "Hello & world");
    }

    #[test]
    fn extracts_xlsx_shared_strings_and_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("table.xlsx");
        let handle = File::create(&path).unwrap();
        let mut archive = ZipWriter::new(handle);
        archive
            .start_file("xl/sharedStrings.xml", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(br#"<sst><si><t>Name</t></si><si><t>Ada</t></si></sst>"#)
            .unwrap();
        archive
            .start_file("xl/worksheets/sheet1.xml", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(
                br#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c t="s"><v>1</v></c><c><v>42</v></c></row></sheetData></worksheet>"#,
            )
            .unwrap();
        archive.finish().unwrap();

        let extracted = extract_document(&path, 1_000, MAX_DOCUMENT_BYTES).unwrap();
        assert_eq!(extracted.format, DocumentFormat::Xlsx);
        assert!(extracted.text.contains("Name\tAda\t42"));
    }

    #[test]
    fn rejects_malformed_office_xml_instead_of_returning_partial_text() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("broken.docx");
        let handle = File::create(&path).unwrap();
        let mut archive = ZipWriter::new(handle);
        archive
            .start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(br#"<w:document><w:p>broken"#).unwrap();
        archive.finish().unwrap();

        let error = extract_document(&path, 1_000, MAX_DOCUMENT_BYTES).unwrap_err();
        assert!(error.to_string().contains("invalid Office XML"));
    }

    #[test]
    fn cancellation_is_checked_before_document_work() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = extract_document_page_with_cancel(
            Path::new("missing.pdf"),
            1_000,
            MAX_DOCUMENT_BYTES,
            0,
            &cancel,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
}
