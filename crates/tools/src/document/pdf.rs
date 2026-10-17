use super::{cancellable_document_reader, check_cancel};
use flate2::read::ZlibDecoder;
use std::io::Read;
use tokio_util::sync::CancellationToken;

const MAX_PDF_STREAM_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PDF_TOTAL_DECODED_BYTES: u64 = 24 * 1024 * 1024;

pub(super) fn extract_pdf(
    bytes: &[u8],
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<Vec<String>> {
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
    let mut pages = Vec::new();
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
            let input = cancellable_document_reader(&bytes[data_start..end_offset], cancel);
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
        pages.push(text);
        streams += 1;
        cursor = end_offset + b"endstream".len();
    }
    if streams == 0 {
        anyhow::bail!("PDF contains no readable content streams");
    }
    Ok(pages)
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

#[cfg(test)]
mod tests {
    use super::super::{MAX_DOCUMENT_BYTES, extract_document};
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    use tempfile::NamedTempFile;

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
    fn decodes_pdf_literal_escapes_and_hex_utf16() {
        assert_eq!(parse_pdf_literal(b"(a\\nb)", &mut 0), "a\nb");
        assert_eq!(parse_pdf_hex(b"<FEFF00480069>", &mut 0), "Hi");
    }
}
