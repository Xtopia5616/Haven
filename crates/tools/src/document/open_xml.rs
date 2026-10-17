use super::{DocumentFormat, cancellable_document_reader, check_cancel};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::io::{Cursor, Read, Seek};
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

const MAX_ZIP_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ZIP_TOTAL_BYTES: u64 = 24 * 1024 * 1024;

pub(super) fn extract_open_xml(
    bytes: &[u8],
    format: DocumentFormat,
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<Vec<String>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
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

    let mut pages = Vec::new();
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
            pages.push(section);
        }
    }
    Ok(pages)
}

fn read_zip_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    max_bytes: u64,
    cancel: Option<&CancellationToken>,
) -> anyhow::Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    let mut bytes = Vec::new();
    cancellable_document_reader(&mut entry, cancel)
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
    output.push_str(&event.xml10_content()?);
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
