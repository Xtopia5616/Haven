//! Bounded text extraction for HTTP response bodies.

use std::path::Path;
use tokio_util::sync::CancellationToken;

pub(super) fn extract_http_content(
    bytes: &[u8],
    content_type: &str,
    as_html: bool,
    extract_known_content: bool,
    max_chars: usize,
    byte_cap_truncated: bool,
    cancel: &CancellationToken,
) -> anyhow::Result<(String, bool, &'static str, Option<String>)> {
    if cancel.is_cancelled() {
        anyhow::bail!("HTTP response extraction cancelled");
    }
    let response_body = haven_common::encoding::decode_lossy(bytes);
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let is_html = content_type.contains("html") || looks_like_html(&response_body);
    if is_html {
        let title = html_title(&response_body);
        if as_html {
            let output = haven_common::encoding::truncate_output(&response_body, max_chars);
            return Ok((output.text, output.truncated, "html", title));
        }
        let output =
            haven_common::encoding::truncate_output(&html_to_text(&response_body), max_chars);
        return Ok((output.text, output.truncated, "text", title));
    }
    if !extract_known_content {
        let output = haven_common::encoding::truncate_output(&response_body, max_chars);
        return Ok((output.text, output.truncated, "raw", None));
    }

    if content_type.contains("pdf") || bytes.starts_with(b"%PDF") {
        if byte_cap_truncated {
            return Ok((
                "PDF response exceeded the configured HTTP body limit; text extraction was skipped."
                    .into(),
                true,
                "pdf",
                None,
            ));
        }
        let path = Path::new("remote-source.pdf");
        let extracted = crate::document::extract_document_from_bytes_with_cancel(
            path,
            bytes,
            max_chars,
            crate::document::MAX_DOCUMENT_BYTES,
            cancel,
        );
        return match extracted {
            Ok(document) => Ok((document.text, document.truncated, "pdf", None)),
            Err(_) => Ok((
                "PDF text extraction is unavailable for this document; it may be encrypted, scanned, or use an unsupported format.".into(),
                false,
                "pdf",
                None,
            )),
        };
    }

    let trimmed = response_body.trim_start();
    if (content_type.contains("json") || trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes)
    {
        let title = value
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string);
        let formatted = serde_json::to_string_pretty(&value).unwrap_or(response_body);
        let output = haven_common::encoding::truncate_output(&formatted, max_chars);
        return Ok((output.text, output.truncated, "json", title));
    }

    if is_feed_content(content_type.as_str(), trimmed) {
        if let Ok((title, text)) = extract_feed_text(&response_body, cancel) {
            let output = haven_common::encoding::truncate_output(&text, max_chars);
            return Ok((output.text, output.truncated, "feed", title));
        }
        let output = haven_common::encoding::truncate_output(&response_body, max_chars);
        return Ok((output.text, output.truncated, "xml", None));
    }

    let format = if content_type.starts_with("text/") {
        "text"
    } else {
        "raw"
    };
    let output = haven_common::encoding::truncate_output(&response_body, max_chars);
    Ok((output.text, output.truncated, format, None))
}

fn is_feed_content(content_type: &str, body: &str) -> bool {
    content_type.contains("rss+xml")
        || content_type.contains("atom+xml")
        || content_type.contains("feed+xml")
        || {
            let prefix = body
                .chars()
                .take(1_024)
                .collect::<String>()
                .to_ascii_lowercase();
            prefix.contains("<rss") || prefix.contains("<feed")
        }
}

fn extract_feed_text(
    xml: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<(Option<String>, String)> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    #[derive(Default)]
    struct FeedEntry {
        title: String,
        link: String,
        summary: String,
        published: String,
    }

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut feed_title = None;
    let mut entries = Vec::<FeedEntry>::new();
    let mut current = None::<FeedEntry>;
    let mut active_field = None::<String>;
    let mut active_text = String::new();

    loop {
        if cancel.is_cancelled() {
            anyhow::bail!("feed extraction cancelled");
        }
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = local_xml_name(event.name().as_ref());
                if matches!(name.as_str(), "item" | "entry") {
                    current = Some(FeedEntry::default());
                } else if name == "link"
                    && let Some(entry) = current.as_mut()
                    && let Some(href) = event
                        .attributes()
                        .flatten()
                        .find(|attribute| local_xml_name(attribute.key.as_ref()) == "href")
                {
                    entry.link = String::from_utf8_lossy(href.value.as_ref()).into_owned();
                }
                if matches!(
                    name.as_str(),
                    "title"
                        | "link"
                        | "description"
                        | "summary"
                        | "content"
                        | "encoded"
                        | "pubDate"
                        | "published"
                        | "updated"
                ) {
                    active_field = Some(name);
                    active_text.clear();
                }
            }
            Ok(Event::Empty(event)) => {
                if local_xml_name(event.name().as_ref()) == "link"
                    && let Some(entry) = current.as_mut()
                    && let Some(href) = event
                        .attributes()
                        .flatten()
                        .find(|attribute| local_xml_name(attribute.key.as_ref()) == "href")
                {
                    entry.link = String::from_utf8_lossy(href.value.as_ref()).into_owned();
                }
            }
            Ok(Event::Text(event)) => {
                if active_field.is_some() {
                    active_text.push_str(&event.xml10_content()?);
                }
            }
            Ok(Event::CData(event)) => {
                if active_field.is_some() {
                    active_text.push_str(&String::from_utf8_lossy(event.as_ref()));
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if active_field.is_some() {
                    append_xml_reference(&mut active_text, event.as_ref());
                }
            }
            Ok(Event::End(event)) => {
                let name = local_xml_name(event.name().as_ref());
                if active_field.as_deref() == Some(name.as_str()) {
                    let value = active_text.trim().to_string();
                    if let Some(entry) = current.as_mut() {
                        match name.as_str() {
                            "title" => entry.title = value,
                            "link" if entry.link.is_empty() => entry.link = value,
                            "description" | "summary" | "content" | "encoded"
                                if entry.summary.is_empty() =>
                            {
                                entry.summary = value
                            }
                            "pubDate" | "published" | "updated" if entry.published.is_empty() => {
                                entry.published = value
                            }
                            _ => {}
                        }
                    } else if name == "title" && feed_title.is_none() {
                        feed_title = Some(value);
                    }
                    active_field = None;
                    active_text.clear();
                }
                if matches!(name.as_str(), "item" | "entry")
                    && let Some(entry) = current.take()
                {
                    entries.push(entry);
                    if entries.len() >= 20 {
                        break;
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => anyhow::bail!("invalid feed XML: {error}"),
            _ => {}
        }
    }

    let mut text = String::new();
    if let Some(title) = feed_title.as_deref().filter(|title| !title.is_empty()) {
        text.push_str("Feed: ");
        text.push_str(title);
        text.push('\n');
    }
    for entry in entries {
        if entry.title.is_empty() && entry.summary.is_empty() {
            continue;
        }
        text.push_str("\n- ");
        text.push_str(if entry.title.is_empty() {
            "(untitled)"
        } else {
            entry.title.as_str()
        });
        if !entry.published.is_empty() {
            text.push_str(" [");
            text.push_str(&entry.published);
            text.push(']');
        }
        if !entry.link.is_empty() {
            text.push_str("\n  ");
            text.push_str(&entry.link);
        }
        if !entry.summary.is_empty() {
            text.push('\n');
            text.push_str(&html_to_text(&entry.summary));
        }
        text.push('\n');
    }
    if text.trim().is_empty() {
        anyhow::bail!("feed contains no readable entries");
    }
    Ok((feed_title, text))
}

fn local_xml_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name.rsplit(|byte| *byte == b':').next().unwrap_or(name)).into_owned()
}

fn append_xml_reference(output: &mut String, reference: &[u8]) {
    match String::from_utf8_lossy(reference).as_ref() {
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

fn html_title(html: &str) -> Option<String> {
    use scraper::{Html, Selector};
    let document = Html::parse_document(html);
    let selector = Selector::parse("title").ok()?;
    document
        .select(&selector)
        .next()
        .map(|element| {
            element
                .text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|title| !title.is_empty())
}

pub(super) fn looks_like_html(body: &str) -> bool {
    let head = &body[..body.floor_char_boundary(body.len().min(512))];
    let head = head.trim_start().to_ascii_lowercase();
    head.starts_with("<!doctype html")
        || head.starts_with("<html")
        || head.starts_with("<head")
        || head.starts_with("<body")
}

pub(super) fn html_to_text(html: &str) -> String {
    use scraper::node::Node;
    use scraper::{Html, Selector};

    const BLOCK_TAGS: &[&str] = &[
        "article",
        "aside",
        "blockquote",
        "body",
        "br",
        "div",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "li",
        "main",
        "nav",
        "ol",
        "p",
        "pre",
        "section",
        "table",
        "td",
        "th",
        "tr",
        "ul",
    ];
    const HIDDEN_TAGS: &[&str] = &[
        "head", "script", "style", "noscript", "template", "nav", "footer", "form", "svg",
    ];

    let doc = Html::parse_document(html);
    let root = Selector::parse("main")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .or_else(|| {
            Selector::parse("article")
                .ok()
                .and_then(|sel| doc.select(&sel).next())
        })
        .or_else(|| {
            Selector::parse("body")
                .ok()
                .and_then(|sel| doc.select(&sel).next())
        })
        .unwrap_or_else(|| doc.root_element());

    let mut out = String::new();
    let mut stack: Vec<_> = Vec::new();
    for child in root.children().rev() {
        stack.push(child);
    }
    while let Some(node) = stack.pop() {
        match node.value() {
            Node::Element(el) => {
                let name = el.name();
                if HIDDEN_TAGS.contains(&name) {
                    continue;
                }
                if BLOCK_TAGS.contains(&name) {
                    out.push('\n');
                }
                for child in node.children().rev() {
                    stack.push(child);
                }
            }
            Node::Text(text) => out.push_str(&text.text),
            _ => {}
        }
    }

    out.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
