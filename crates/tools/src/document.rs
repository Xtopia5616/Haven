//! Bounded local extraction for document attachments.
//!
//! This is deliberately a tool-side representation step, not a provider
//! upload protocol. The extractor reads a managed host file, returns a
//! bounded derived text representation, and leaves the caller to add the
//! normal untrusted-content fence. Unsupported/encrypted documents are
//! reported explicitly instead of being treated as empty text.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use flate2::read::ZlibDecoder;
use quick_xml::Reader;
use quick_xml::events::Event;
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

/// Hard upper bound for one document read, independent of the model context
/// budget. This protects the local parser and ZIP decompression path from
/// untrusted attachment sizes and compression ratios.
pub const MAX_DOCUMENT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ZIP_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ZIP_TOTAL_BYTES: u64 = 24 * 1024 * 1024;
const MAX_PDF_STREAM_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PDF_TOTAL_DECODED_BYTES: u64 = 24 * 1024 * 1024;

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
}

/// Extract a bounded textual representation from a supported document.
#[cfg(test)]
pub fn extract_document(
    path: &Path,
    max_chars: usize,
    max_document_bytes: u64,
) -> anyhow::Result<DocumentExtraction> {
    extract_document_inner(path, max_chars, max_document_bytes, None)
}

/// Cancellable variant used by the files tool while parsing untrusted
/// documents. The ordinary helper remains synchronous for existing callers.
pub fn extract_document_with_cancel(
    path: &Path,
    max_chars: usize,
    max_document_bytes: u64,
    cancel: &CancellationToken,
) -> anyhow::Result<DocumentExtraction> {
    extract_document_inner(path, max_chars, max_document_bytes, Some(cancel))
}

fn extract_document_inner(
    path: &Path,
    max_chars: usize,
    max_document_bytes: u64,
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<DocumentExtraction> {
    check_cancel(cancel)?;
    let format = format_for_path(path)
        .ok_or_else(|| anyhow::anyhow!("document extraction is unsupported for this file type"))?;
    let metadata = std::fs::metadata(path)?;
    let size_bytes = metadata.len();
    let byte_limit = max_document_bytes.min(MAX_DOCUMENT_BYTES);
    if size_bytes > byte_limit {
        anyhow::bail!("document exceeds the local extraction size limit");
    }

    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(size_bytes.min(byte_limit) as usize);
    CancellableReader {
        inner: file,
        cancel,
    }
    .take(byte_limit.saturating_add(1))
    .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > byte_limit {
        anyhow::bail!("document exceeds the local extraction size limit");
    }

    let (text, sections) = match format {
        DocumentFormat::Pdf => extract_pdf(&bytes, cancel)?,
        DocumentFormat::Docx | DocumentFormat::Xlsx | DocumentFormat::Pptx => {
            extract_open_xml(path, format, cancel)?
        }
    };
    let (text, _) = haven_common::encoding::truncate_output(&normalize_text(&text), max_chars);
    if text.trim().is_empty() {
        anyhow::bail!("document contains no extractable text");
    }
    Ok(DocumentExtraction {
        format,
        representation: format.representation(),
        text,
        sections: sections.max(1),
        size_bytes,
    })
}

fn check_cancel(cancel: Option<&CancellationToken>) -> anyhow::Result<()> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        anyhow::bail!("document extraction cancelled");
    }
    Ok(())
}

struct CancellableReader<'a, R> {
    inner: R,
    cancel: Option<&'a CancellationToken>,
}

impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "document extraction cancelled",
            ));
        }
        self.inner.read(buffer)
    }
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

fn extract_pdf(
    bytes: &[u8],
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<(String, usize)> {
    if !bytes.starts_with(b"%PDF") {
        anyhow::bail!("file is not a PDF document");
    }
    if bytes
        .windows(b"/Encrypt".len())
        .any(|window| window == b"/Encrypt")
    {
        anyhow::bail!("encrypted PDF extraction is unavailable");
    }

    let mut cursor = 0;
    let mut output = String::new();
    let mut streams = 0;
    let mut total_decoded_bytes = 0u64;
    while let Some(stream_offset) = find_bytes(&bytes[cursor..], b"stream") {
        check_cancel(cancel)?;
        let stream_offset = cursor + stream_offset;
        let data_start = skip_stream_eol(bytes, stream_offset + b"stream".len());
        let Some(end_offset) = find_bytes(&bytes[data_start..], b"endstream") else {
            break;
        };
        let end_offset = data_start + end_offset;
        let dictionary_start = bytes[..stream_offset]
            .windows(2)
            .rposition(|window| window == b"<<")
            .unwrap_or(cursor);
        let dictionary = &bytes[dictionary_start..stream_offset];
        let stream = if dictionary
            .windows(b"/FlateDecode".len())
            .any(|window| window == b"/FlateDecode")
        {
            let input = CancellableReader {
                inner: &bytes[data_start..end_offset],
                cancel,
            };
            let mut decoder = ZlibDecoder::new(input);
            let mut decoded = Vec::new();
            decoder
                .by_ref()
                .take(MAX_PDF_STREAM_BYTES.saturating_add(1))
                .read_to_end(&mut decoded)?;
            if decoded.len() as u64 > MAX_PDF_STREAM_BYTES {
                anyhow::bail!("PDF decompressed stream exceeds the extraction limit");
            }
            decoded
        } else if dictionary.windows(7).any(|window| window == b"/Filter") {
            anyhow::bail!("PDF uses an unsupported stream filter");
        } else {
            let raw = &bytes[data_start..end_offset];
            if raw.len() as u64 > MAX_PDF_STREAM_BYTES {
                anyhow::bail!("PDF stream exceeds the extraction limit");
            }
            raw.to_vec()
        };
        total_decoded_bytes = total_decoded_bytes.saturating_add(stream.len() as u64);
        if total_decoded_bytes > MAX_PDF_TOTAL_DECODED_BYTES {
            anyhow::bail!("PDF decompressed content exceeds the extraction limit");
        }
        let text = extract_pdf_stream_text(&stream);
        if !text.trim().is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&text);
        }
        streams += 1;
        cursor = end_offset + b"endstream".len();
    }
    if streams == 0 {
        anyhow::bail!("PDF contains no readable content streams");
    }
    Ok((output, streams))
}

fn skip_stream_eol(bytes: &[u8], mut offset: usize) -> usize {
    if bytes.get(offset) == Some(&b'\r') {
        offset += 1;
        if bytes.get(offset) == Some(&b'\n') {
            offset += 1;
        }
    } else if bytes.get(offset) == Some(&b'\n') {
        offset += 1;
    }
    offset
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[derive(Debug)]
enum PdfToken {
    String(String),
    Array(Vec<String>),
    Word(String),
}

fn extract_pdf_stream_text(stream: &[u8]) -> String {
    let mut index = 0;
    let mut in_text = false;
    let mut pending = Vec::new();
    let mut output = String::new();
    while let Some(token) = next_pdf_token(stream, &mut index) {
        match token {
            PdfToken::Word(word) if word == "BT" => in_text = true,
            PdfToken::Word(word) if word == "ET" => {
                flush_pdf_pending(&mut output, &mut pending);
                in_text = false;
            }
            PdfToken::String(value) if in_text => pending.push(value),
            PdfToken::Array(values) if in_text => pending.extend(values),
            PdfToken::Word(word) if in_text && word == "Tj" => {
                flush_pdf_pending(&mut output, &mut pending);
            }
            PdfToken::Word(word) if in_text && word == "TJ" => {
                flush_pdf_pending(&mut output, &mut pending);
            }
            PdfToken::Word(word)
                if in_text && matches!(word.as_str(), "'" | "\"" | "T*" | "Td" | "TD") =>
            {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                flush_pdf_pending(&mut output, &mut pending);
            }
            _ => {}
        }
    }
    flush_pdf_pending(&mut output, &mut pending);
    output
}

fn flush_pdf_pending(output: &mut String, pending: &mut Vec<String>) {
    for value in pending.drain(..) {
        output.push_str(&value);
    }
}

fn next_pdf_token(bytes: &[u8], index: &mut usize) -> Option<PdfToken> {
    skip_pdf_space_and_comments(bytes, index);
    let first = *bytes.get(*index)?;
    if first == b']' {
        *index += 1;
        return Some(PdfToken::Word("]".into()));
    }
    match first {
        b'(' => Some(PdfToken::String(parse_pdf_literal(bytes, index))),
        b'<' if bytes.get(*index + 1) != Some(&b'<') => {
            Some(PdfToken::String(parse_pdf_hex(bytes, index)))
        }
        b'[' => Some(PdfToken::Array(parse_pdf_array(bytes, index))),
        _ => {
            let start = *index;
            while let Some(byte) = bytes.get(*index) {
                if byte.is_ascii_whitespace() || b"()<>[]{}/%".contains(byte) {
                    break;
                }
                *index += 1;
            }
            if *index == start {
                *index += 1;
                return next_pdf_token(bytes, index);
            }
            Some(PdfToken::Word(
                String::from_utf8_lossy(&bytes[start..*index]).into_owned(),
            ))
        }
    }
}

fn skip_pdf_space_and_comments(bytes: &[u8], index: &mut usize) {
    loop {
        while bytes
            .get(*index)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            *index += 1;
        }
        if bytes.get(*index) != Some(&b'%') {
            break;
        }
        while bytes
            .get(*index)
            .is_some_and(|byte| *byte != b'\r' && *byte != b'\n')
        {
            *index += 1;
        }
    }
}

fn parse_pdf_literal(bytes: &[u8], index: &mut usize) -> String {
    *index += 1;
    let mut output = Vec::new();
    let mut depth = 1;
    while let Some(byte) = bytes.get(*index).copied() {
        *index += 1;
        match byte {
            b'(' => {
                depth += 1;
                output.push(byte);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                output.push(byte);
            }
            b'\\' => match bytes.get(*index).copied() {
                Some(b'n') => {
                    *index += 1;
                    output.push(b'\n');
                }
                Some(b'r') => {
                    *index += 1;
                    output.push(b'\r');
                }
                Some(b't') => {
                    *index += 1;
                    output.push(b'\t');
                }
                Some(b'b') | Some(b'f') => {
                    *index += 1;
                }
                Some(b'\r') => {
                    *index += 1;
                    if bytes.get(*index) == Some(&b'\n') {
                        *index += 1;
                    }
                }
                Some(b'\n') => *index += 1,
                Some(next @ b'0'..=b'7') => {
                    let mut value = next - b'0';
                    *index += 1;
                    for _ in 0..2 {
                        let Some(digit @ b'0'..=b'7') = bytes.get(*index).copied() else {
                            break;
                        };
                        value = value.saturating_mul(8).saturating_add(digit - b'0');
                        *index += 1;
                    }
                    output.push(value);
                }
                Some(next) => {
                    *index += 1;
                    output.push(next);
                }
                None => break,
            },
            other => output.push(other),
        }
    }
    decode_pdf_bytes(&output)
}

fn parse_pdf_hex(bytes: &[u8], index: &mut usize) -> String {
    *index += 1;
    let mut hex = Vec::new();
    while let Some(byte) = bytes.get(*index).copied() {
        *index += 1;
        if byte == b'>' {
            break;
        }
        if byte.is_ascii_hexdigit() {
            hex.push(byte);
        }
    }
    if hex.len() % 2 != 0 {
        hex.push(b'0');
    }
    let decoded = hex
        .chunks(2)
        .filter_map(|pair| {
            if pair.len() != 2 {
                return None;
            }
            let high = (pair[0] as char).to_digit(16)? as u8;
            let low = (pair[1] as char).to_digit(16)? as u8;
            Some((high << 4) | low)
        })
        .collect::<Vec<_>>();
    decode_pdf_bytes(&decoded)
}

fn parse_pdf_array(bytes: &[u8], index: &mut usize) -> Vec<String> {
    *index += 1;
    let mut values = Vec::new();
    while let Some(token) = next_pdf_token(bytes, index) {
        match token {
            PdfToken::String(value) => values.push(value),
            PdfToken::Word(word) if word == "]" => break,
            _ => {}
        }
    }
    values
}

fn decode_pdf_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units = bytes[2..]
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
        return String::from_utf16_lossy(&units.collect::<Vec<_>>());
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn extract_open_xml(
    path: &Path,
    format: DocumentFormat,
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<(String, usize)> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        check_cancel(cancel)?;
        let name = archive.by_index(index)?.name().to_string();
        let keep = match format {
            DocumentFormat::Docx => {
                name == "word/document.xml"
                    || name.starts_with("word/header")
                    || name.starts_with("word/footer")
            }
            DocumentFormat::Pptx => name.starts_with("ppt/slides/slide") && name.ends_with(".xml"),
            DocumentFormat::Xlsx => {
                (name == "xl/sharedStrings.xml" || name.starts_with("xl/worksheets/sheet"))
                    && name.ends_with(".xml")
            }
            DocumentFormat::Pdf => false,
        };
        if keep {
            names.push(name);
        }
    }
    names.sort();
    if names.is_empty() {
        anyhow::bail!("Office document has no supported content parts");
    }

    let mut total_bytes = 0u64;
    let shared_strings = if format == DocumentFormat::Xlsx {
        names
            .iter()
            .find(|name| name.as_str() == "xl/sharedStrings.xml")
            .map(|name| read_zip_entry(&mut archive, name, MAX_ZIP_ENTRY_BYTES, cancel))
            .transpose()?
            .map(|bytes| {
                total_bytes = total_bytes.saturating_add(bytes.len() as u64);
                if total_bytes > MAX_ZIP_TOTAL_BYTES {
                    anyhow::bail!("Office document content exceeds the extraction limit");
                }
                extract_xml_units(&bytes, b"si", cancel)
            })
            .transpose()?
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut output = String::new();
    let mut sections = 0;
    for name in names {
        check_cancel(cancel)?;
        if format == DocumentFormat::Xlsx && name == "xl/sharedStrings.xml" {
            continue;
        }
        let bytes = read_zip_entry(&mut archive, &name, MAX_ZIP_ENTRY_BYTES, cancel)?;
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        if total_bytes > MAX_ZIP_TOTAL_BYTES {
            anyhow::bail!("Office document content exceeds the extraction limit");
        }
        let section = if format == DocumentFormat::Xlsx {
            extract_xlsx_sheet(&bytes, &shared_strings, cancel)?
        } else {
            extract_xml_text(&bytes, cancel)?
        };
        if !section.trim().is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&section);
            sections += 1;
        }
    }
    Ok((output, sections))
}

fn read_zip_entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    max_bytes: u64,
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    let mut bytes = Vec::new();
    CancellableReader {
        inner: &mut entry,
        cancel,
    }
    .take(max_bytes.saturating_add(1))
    .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!("Office XML part exceeds the extraction limit");
    }
    Ok(bytes)
}

fn extract_xml_units(
    bytes: &[u8],
    boundary: &[u8],
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<Vec<String>> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut xml_depth = 0usize;
    let mut current = String::new();
    let mut units = Vec::new();
    loop {
        check_cancel(cancel)?;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                xml_depth += 1;
                if local_name(event.name().as_ref()) == boundary {
                    depth += 1;
                    current.clear();
                }
            }
            Ok(Event::End(event)) => {
                if xml_depth == 0 {
                    anyhow::bail!("invalid Office XML: unmatched closing tag");
                }
                xml_depth -= 1;
                if local_name(event.name().as_ref()) == boundary {
                    depth = depth.saturating_sub(1);
                    if depth == 0 && !current.trim().is_empty() {
                        units.push(current.trim().to_string());
                    }
                }
            }
            Ok(Event::Text(event)) if depth > 0 => {
                append_xml_text(&mut current, &event)?;
            }
            Ok(Event::CData(event)) if depth > 0 => {
                current.push_str(&String::from_utf8_lossy(event.as_ref()));
            }
            Ok(Event::GeneralRef(event)) if depth > 0 => {
                append_xml_ref(&mut current, &event);
            }
            Ok(Event::Eof) => break,
            Err(error) => anyhow::bail!("invalid Office XML: {error}"),
            _ => {}
        }
        buf.clear();
    }
    if xml_depth != 0 {
        anyhow::bail!("invalid Office XML: unclosed tag");
    }
    Ok(units)
}

fn extract_xml_text(bytes: &[u8], cancel: Option<&CancellationToken>) -> anyhow::Result<String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut output = String::new();
    let mut xml_depth = 0usize;
    loop {
        check_cancel(cancel)?;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(_)) => xml_depth += 1,
            Ok(Event::Text(event)) => append_xml_text(&mut output, &event)?,
            Ok(Event::CData(event)) => output.push_str(&String::from_utf8_lossy(event.as_ref())),
            Ok(Event::GeneralRef(event)) => append_xml_ref(&mut output, &event),
            Ok(Event::Empty(event)) => match local_name(event.name().as_ref()) {
                b"br" | b"tab" => output.push('\n'),
                _ => {}
            },
            Ok(Event::End(event)) => {
                if xml_depth == 0 {
                    anyhow::bail!("invalid Office XML: unmatched closing tag");
                }
                xml_depth -= 1;
                match local_name(event.name().as_ref()) {
                    b"p" | b"tr" | b"slide" => output.push('\n'),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => anyhow::bail!("invalid Office XML: {error}"),
            _ => {}
        }
        buf.clear();
    }
    if xml_depth != 0 {
        anyhow::bail!("invalid Office XML: unclosed tag");
    }
    Ok(output)
}

fn extract_xlsx_sheet(
    bytes: &[u8],
    shared_strings: &[String],
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut output = String::new();
    let mut cell_type = None;
    let mut cell_value = String::new();
    let mut in_value = false;
    let mut xml_depth = 0usize;
    loop {
        check_cancel(cancel)?;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(event)) => {
                xml_depth += 1;
                match local_name(event.name().as_ref()) {
                    b"c" => {
                        cell_type = event
                            .attributes()
                            .flatten()
                            .find(|attribute| local_name(attribute.key.as_ref()) == b"t")
                            .map(|attribute| {
                                String::from_utf8_lossy(&attribute.value).into_owned()
                            });
                        cell_value.clear();
                    }
                    b"v" => in_value = true,
                    _ => {}
                }
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == b"v" => {
                in_value = false;
                if xml_depth == 0 {
                    anyhow::bail!("invalid Office XML: unmatched closing tag");
                }
                xml_depth -= 1;
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == b"c" => {
                let value = if cell_type.as_deref() == Some("s") {
                    cell_value
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| shared_strings.get(index).cloned())
                        .unwrap_or_else(|| cell_value.clone())
                } else {
                    cell_value.clone()
                };
                if !value.is_empty() {
                    if !output.is_empty() && !output.ends_with('\t') && !output.ends_with('\n') {
                        output.push('\t');
                    }
                    output.push_str(&value);
                }
                cell_type = None;
                cell_value.clear();
                if xml_depth == 0 {
                    anyhow::bail!("invalid Office XML: unmatched closing tag");
                }
                xml_depth -= 1;
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == b"row" => {
                output.push('\n');
                if xml_depth == 0 {
                    anyhow::bail!("invalid Office XML: unmatched closing tag");
                }
                xml_depth -= 1;
            }
            Ok(Event::End(_)) => {
                if xml_depth == 0 {
                    anyhow::bail!("invalid Office XML: unmatched closing tag");
                }
                xml_depth -= 1;
            }
            Ok(Event::Text(event)) if in_value => append_xml_text(&mut cell_value, &event)?,
            Ok(Event::GeneralRef(event)) if in_value => append_xml_ref(&mut cell_value, &event),
            Ok(Event::Eof) => break,
            Err(error) => anyhow::bail!("invalid Office XML: {error}"),
            _ => {}
        }
        buf.clear();
    }
    if xml_depth != 0 {
        anyhow::bail!("invalid Office XML: unclosed tag");
    }
    Ok(output)
}

fn append_xml_text(
    output: &mut String,
    event: &quick_xml::events::BytesText<'_>,
) -> anyhow::Result<()> {
    output.push_str(&event.xml_content()?);
    Ok(())
}

fn append_xml_ref(output: &mut String, event: &quick_xml::events::BytesRef<'_>) {
    let reference = String::from_utf8_lossy(event.as_ref());
    match reference.as_ref() {
        "amp" => output.push('&'),
        "lt" => output.push('<'),
        "gt" => output.push('>'),
        "apos" => output.push('\''),
        "quot" => output.push('"'),
        numeric if numeric.starts_with("#x") => {
            if let Ok(value) = u32::from_str_radix(&numeric[2..], 16)
                && let Some(character) = char::from_u32(value)
            {
                output.push(character);
            }
        }
        numeric if numeric.starts_with('#') => {
            if let Ok(value) = numeric[1..].parse::<u32>()
                && let Some(character) = char::from_u32(value)
            {
                output.push(character);
            }
        }
        _ => {}
    }
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
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
    fn rejects_pdf_flate_streams_that_exceed_the_decompressed_limit() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&vec![b'x'; (MAX_PDF_STREAM_BYTES + 1) as usize])
            .unwrap();
        let compressed = encoder.finish().unwrap();
        let mut file = NamedTempFile::with_suffix(".pdf").unwrap();
        writeln!(file, "%PDF-1.4").unwrap();
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

        let error = extract_document(file.path(), 1_000, MAX_DOCUMENT_BYTES)
            .unwrap_err()
            .to_string();
        assert!(error.contains("decompressed stream"));
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
        let error = extract_document_with_cancel(
            Path::new("missing.pdf"),
            1_000,
            MAX_DOCUMENT_BYTES,
            &cancel,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn decodes_pdf_literal_escapes_and_hex_utf16() {
        assert_eq!(parse_pdf_literal(b"(a\\nb)", &mut 0), "a\nb");
        assert_eq!(parse_pdf_hex(b"<FEFF00480069>", &mut 0), "Hi");
    }
}
