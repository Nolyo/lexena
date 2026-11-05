//! Content transformations shared by the local MCP note tools.
use pulldown_cmark::{html, Event, Options, Parser};
use scraper::{Html, Node};
use std::fmt;

pub const MAX_MUTATION_BYTES: usize = 3_145_728;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentError {
    Unsupported,
    NotFound,
    Ambiguous,
    TooLarge,
}

impl ContentError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "UNSUPPORTED_CONTENT",
            Self::NotFound => "NOT_FOUND",
            Self::Ambiguous => "AMBIGUOUS_MATCH",
            Self::TooLarge => "CONTENT_TOO_LARGE",
        }
    }
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ContentError {}

#[derive(Debug, Clone)]
pub struct NoteView {
    pub content: String,
    pub lossy: bool,
    pub warnings: Vec<String>,
}

fn check_size(html: &str) -> Result<(), ContentError> {
    if html.len() > MAX_MUTATION_BYTES {
        Err(ContentError::TooLarge)
    } else {
        Ok(())
    }
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn safe_style(value: &str) -> bool {
    value
        .split(';')
        .filter(|part| !part.trim().is_empty())
        .all(|part| {
            let Some((key, value)) = part.split_once(':') else {
                return false;
            };
            let value = value.trim().to_ascii_lowercase();
            match key.trim().to_ascii_lowercase().as_str() {
                "text-align" => matches!(
                    value.as_str(),
                    "left" | "right" | "center" | "justify" | "start" | "end"
                ),
                "color" | "background-color" => {
                    if value.is_empty() {
                        return false;
                    }
                    if value.bytes().all(|b| b.is_ascii_alphabetic()) {
                        return true;
                    }
                    if let Some(hex) = value.strip_prefix('#') {
                        return matches!(hex.len(), 3 | 4 | 6 | 8)
                            && hex.bytes().all(|b| b.is_ascii_hexdigit());
                    }
                    ["rgb(", "rgba(", "hsl(", "hsla("].iter().any(|prefix| {
                        value
                            .strip_prefix(prefix)
                            .and_then(|s| s.strip_suffix(')'))
                            .is_some_and(|inner| {
                                !inner.is_empty()
                                    && inner
                                        .bytes()
                                        .all(|b| b.is_ascii_digit() || b" ,.%-/".contains(&b))
                            })
                    })
                }
                "width" | "height" | "min-width" => {
                    let number = value
                        .strip_suffix("px")
                        .or_else(|| value.strip_suffix('%'))
                        .unwrap_or(&value);
                    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                }
                _ => false,
            }
        })
}

fn safe_href(value: &str) -> bool {
    if value.chars().any(|c| c.is_control() || c == '\\') {
        return false;
    }
    let value = value.trim();
    if value.starts_with('#') { return true; }
    let Ok(parsed) = url::Url::parse(value) else { return false; };
    if parsed.scheme() == "mailto" { return true; }
    if !matches!(parsed.scheme(), "http" | "https") { return false; }
    match parsed.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain.contains('.') && !["localhost", "local", "internal", "lan", "home", "localdomain"].iter().any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}")))
        }
        Some(url::Host::Ipv4(ip)) => public_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => {
            if let Some(mapped) = ip.to_ipv4_mapped() { return public_ipv4(mapped); }
            !ip.is_loopback() && !ip.is_unspecified() && !ip.is_unique_local() && !ip.is_unicast_link_local() && !ip.is_multicast()
        }
        None => false,
    }
}

fn public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let octets = ip.octets();
    !ip.is_private() && !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast()
        && octets[0] != 0 && octets[0] < 224 && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
        && !(octets[0] == 198 && matches!(octets[1], 18 | 19))
}

fn raster_data_uri(value: &str) -> bool {
    ["png", "jpeg", "jpg", "gif", "webp"].iter().any(|format| {
        value
            .strip_prefix(&format!("data:image/{format};base64,"))
            .is_some_and(|data| {
                !data.is_empty()
                    && data
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
            })
    })
}

/// Reject unsupported constructs; never silently sanitize a rich document.
pub fn validate_html(html: &str) -> Result<(), ContentError> {
    let document = Html::parse_fragment(html);
    for node in document.tree.root().descendants() {
        let Some(element) = node.value().as_element() else {
            continue;
        };
        let tag = element.name();
        if !matches!(
            tag,
            "html"
                | "p"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "br"
                | "hr"
                | "strong"
                | "b"
                | "em"
                | "i"
                | "s"
                | "strike"
                | "u"
                | "code"
                | "pre"
                | "blockquote"
                | "ul"
                | "ol"
                | "li"
                | "table"
                | "thead"
                | "tbody"
                | "tfoot"
                | "tr"
                | "th"
                | "td"
                | "colgroup"
                | "col"
                | "a"
                | "img"
                | "span"
                | "mark"
                | "div"
                | "label"
                | "input"
        ) {
            return Err(ContentError::Unsupported);
        }
        for (name, value) in element.attrs() {
            let allowed = match name {
                "class" => true,
                "style" => safe_style(value),
                "data-type" => matches!(value, "taskList" | "taskItem"),
                "data-checked" => matches!(value, "true" | "false"),
                "data-note-link" => tag == "a" && value == "true",
                "data-note-id" | "data-note-title" => tag == "a",
                "data-color" => tag == "mark" || tag == "span",
                "href" => tag == "a" && safe_href(value),
                "target" => tag == "a" && matches!(value, "_blank" | "_self"),
                "rel" => {
                    tag == "a"
                        && value
                            .split_whitespace()
                            .all(|token| matches!(token, "noopener" | "noreferrer" | "nofollow"))
                }
                "src" => tag == "img" && raster_data_uri(value),
                "alt" | "title" => matches!(tag, "img" | "a"),
                "colspan" | "rowspan" | "colwidth" | "span" | "width" | "height" | "start" => {
                    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit() || b == b',')
                }
                "type" => tag == "input" && value == "checkbox",
                "checked" | "disabled" => tag == "input" && (value.is_empty() || value == name),
                _ => false,
            };
            if !allowed {
                return Err(ContentError::Unsupported);
            }
        }
        if tag == "input" && element.attr("type") != Some("checkbox") {
            return Err(ContentError::Unsupported);
        }
    }
    Ok(())
}

pub fn render_input(content: &str, format: &str) -> Result<String, ContentError> {
    check_size(content)?;
    let rendered = match format {
        "text" => content
            .split('\n')
            .map(|line| format!("<p>{}</p>", escape_text(line)))
            .collect::<String>(),
        "markdown" => {
            let parser = Parser::new_ext(
                content,
                Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
            );
            let events: Vec<_> = parser.collect();
            if events
                .iter()
                .any(|event| matches!(event, Event::Html(_) | Event::InlineHtml(_)))
            {
                return Err(ContentError::Unsupported);
            }
            let mut output = String::new();
            html::push_html(&mut output, events.into_iter());
            output
        }
        _ => return Err(ContentError::Unsupported),
    };
    check_size(&rendered)?;
    validate_html(&rendered)?;
    Ok(rendered)
}

pub fn append_html(existing: &str, fragment: &str) -> Result<String, ContentError> {
    if existing.len().saturating_add(fragment.len()) > MAX_MUTATION_BYTES {
        return Err(ContentError::TooLarge);
    }
    validate_html(existing)?;
    validate_html(fragment)?;
    Ok(format!("{existing}{fragment}"))
}

pub fn replace_unique_text(html: &str, old: &str, new: &str) -> Result<String, ContentError> {
    if old.is_empty() {
        return Err(ContentError::Unsupported);
    }
    validate_html(html)?;
    let mut document = Html::parse_fragment(html);
    let mut matches = Vec::new();
    let mut all_text = String::new();
    for node in document.tree.root().descendants() {
        if let Node::Text(text) = node.value() {
            all_text.push_str(&text.text);
            if text.text.contains(old)
                && node.ancestors().any(|ancestor| {
                    ancestor
                        .value()
                        .as_element()
                        .is_some_and(|element| element.attr("data-note-link").is_some())
                })
            {
                return Err(ContentError::Unsupported);
            }
            if let Some(offset) = text.text.find(old) {
                // Search again one Unicode boundary after the first start so
                // overlapping occurrences remain ambiguous, without O(n*m) scans.
                let next = offset + text.text[offset..].chars().next().unwrap().len_utf8();
                if !matches.is_empty() || text.text[next..].contains(old) { return Err(ContentError::Ambiguous); }
                matches.push((node.id(), offset));
            }
        }
    }
    if matches.len() > 1 {
        return Err(ContentError::Ambiguous);
    }
    let Some((id, offset)) = matches.first().copied() else {
        return Err(if all_text.contains(old) {
            ContentError::Unsupported
        } else {
            ContentError::NotFound
        });
    };
    let mut node = document.tree.get_mut(id).ok_or(ContentError::NotFound)?;
    if let Node::Text(text) = node.value() {
        let mut replaced = text.text.to_string();
        replaced.replace_range(offset..offset + old.len(), new);
        text.text = replaced.into();
    }
    let result = document.root_element().inner_html();
    check_size(&result)?;
    Ok(result)
}

pub fn plain_text(html: &str) -> String {
    let document = Html::parse_fragment(html);
    let mut output = String::new();
    for node in document.tree.root().descendants() {
        if node.ancestors().any(|ancestor| {
            ancestor
                .value()
                .as_element()
                .is_some_and(|e| matches!(e.name(), "script" | "style" | "iframe" | "object"))
        }) {
            continue;
        }
        match node.value() {
            Node::Text(text) => output.push_str(&text.text),
            Node::Element(element) if element.name() == "img" => output.push_str("[image]"),
            Node::Element(element)
                if matches!(
                    element.name(),
                    "p" | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "br"
                        | "li"
                        | "tr"
                        | "blockquote"
                        | "pre"
                ) =>
            {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
            }
            _ => {}
        }
    }
    output.trim().to_string()
}

pub fn read_view(html: &str, format: &str) -> Result<NoteView, ContentError> {
    match format {
        "html" => Ok(NoteView {
            content: html.to_string(),
            lossy: false,
            warnings: vec![],
        }),
        "text" => Ok(NoteView {
            content: plain_text(html),
            lossy: true,
            warnings: vec!["Text view omits rich formatting and image payloads.".into()],
        }),
        "markdown" => {
            let mut document = Html::parse_fragment(html);
            let images: Vec<_> = document
                .tree
                .root()
                .descendants()
                .filter(|node| node.value().as_element().is_some_and(|e| e.name() == "img"))
                .map(|node| node.id())
                .collect();
            for id in images {
                if let Some(mut node) = document.tree.get_mut(id) {
                    *node.value() = Node::Text(scraper::node::Text {
                        text: "[image]".into(),
                    });
                }
            }
            let content = htmd::convert(&document.root_element().inner_html())
                .map_err(|_| ContentError::Unsupported)?;
            Ok(NoteView { content, lossy: true, warnings: vec!["Markdown omits some TipTap styles, internal-link metadata and image payloads. Use HTML for complete replacement.".into()] })
        }
        _ => Err(ContentError::Unsupported),
    }
}

pub fn derive_note_title(html: &str, locale: &str) -> String {
    let text = plain_text(html);
    let Some(first) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return if locale.starts_with("fr") {
            "Note sans titre"
        } else {
            "Untitled note"
        }
        .into();
    };
    if first.encode_utf16().count() <= 50 {
        return first.into();
    }
    let mut title = String::new();
    let mut units = 0;
    for ch in first.chars() {
        if units + ch.len_utf16() > 50 {
            break;
        }
        units += ch.len_utf16();
        title.push(ch);
    }
    title.push_str("...");
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_input_escapes_markup_and_preserves_paragraphs() {
        assert_eq!(
            render_input("<script> &\nsecond", "text").unwrap(),
            "<p>&lt;script&gt; &amp;</p><p>second</p>"
        );
    }

    #[test]
    fn append_keeps_existing_rich_html_byte_for_byte() {
        let html = r#"<h1>Projet</h1><table><tbody><tr><td colspan="1" rowspan="1"><p>A</p></td></tr></tbody></table><ul data-type="taskList"><li data-type="taskItem" data-checked="true"><label><input type="checkbox" checked="checked"><span></span></label><div><p>Fait</p></div></li></ul><p><a data-note-link="true" data-note-id="11111111-1111-4111-8111-111111111111" data-note-title="Liée">Liée</a><img src="data:image/png;base64,iVBORw0KGgo=" alt="image"></p>"#;
        assert_eq!(
            append_html(html, "<p>Suite</p>").unwrap(),
            format!("{html}<p>Suite</p>")
        );
    }

    #[test]
    fn unique_replacement_changes_text_but_not_attributes_or_other_nodes() {
        let html =
            r#"<p><a href="https://example.com/ancien">ancien</a><strong>intact</strong></p>"#;
        let replaced = replace_unique_text(html, "ancien", "<nouveau> & sûr").unwrap();
        assert!(replaced.contains("href=\"https://example.com/ancien\""));
        assert!(replaced.contains("&lt;nouveau&gt; &amp; sûr"));
        assert!(replaced.contains("<strong>intact</strong>"));
        assert!(!replaced.contains("<nouveau>"));
    }

    #[test]
    fn replacement_rejects_missing_ambiguous_or_cross_node_matches() {
        assert_eq!(
            replace_unique_text("<p>un un</p>", "un", "deux"),
            Err(ContentError::Ambiguous)
        );
        assert_eq!(
            replace_unique_text("<p>un</p>", "absent", "deux"),
            Err(ContentError::NotFound)
        );
        assert_eq!(
            replace_unique_text("<p>bon<strong>jour</strong></p>", "bonjour", "salut"),
            Err(ContentError::Unsupported)
        );
    }

    #[test]
    fn overlapping_matches_are_ambiguous_and_note_link_chips_are_not_edited_as_text() {
        assert_eq!(
            replace_unique_text("<p>aaaa</p>", "aaa", "x"),
            Err(ContentError::Ambiguous)
        );
        assert_eq!(
            replace_unique_text(
                r#"<p><a data-note-link="true" data-note-id="11111111-1111-4111-8111-111111111111" data-note-title="Projet">Projet</a></p>"#,
                "Projet",
                "Autre"
            ),
            Err(ContentError::Unsupported)
        );
    }

    #[test]
    fn active_html_and_local_urls_are_rejected_instead_of_silently_stripped() {
        for html in [
            "<script>alert(1)</script>",
            "<p onclick=\"run()\">x</p>",
            "<a href=\"java&#x73;cript:run()\">x</a>",
            "<img src=\"file:///C:/secret\">",
            "<svg><script>x</script></svg>",
            "<iframe src=\"https://example.com\"></iframe>",
            "<span style=\"color:red;background:url(https://example.com)\">x</span>",
        ] {
            assert_eq!(
                validate_html(html),
                Err(ContentError::Unsupported),
                "{html}"
            );
        }
        assert_eq!(
            render_input("<script>x</script>", "markdown"),
            Err(ContentError::Unsupported)
        );
    }

    #[test]
    fn local_network_hosts_and_alternate_loopback_forms_are_not_safe_links() {
        for destination in ["http://localhost:1420", "http://LOCALHOST.", "http://127.0.0.1", "http://2130706433", "http://0x7f000001", "http://[::1]", "http://[::ffff:127.0.0.1]", "http://10.2.3.4", "https://192.168.1.20", "http://172.16.0.1", "http://169.254.169.254", "http://[fd00::1]", "http://printer", "http://router.lan"] {
            assert_eq!(validate_html(&format!("<p><a href=\"{destination}\">link</a></p>")), Err(ContentError::Unsupported), "{destination}");
        }
        assert_eq!(render_input("[local](http://localhost:1420)", "markdown"), Err(ContentError::Unsupported));
        assert!(validate_html("<a href=\"https://example.com/path\">Web</a><a href=\"#section\">Section</a>").is_ok());
    }

    #[test]
    fn long_repeated_prefix_matching_is_bounded() {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let html = format!("<p>{}</p>", "a".repeat(3_000_000));
            let absent = format!("{}b", "a".repeat(1_000_000));
            sender.send(replace_unique_text(&html, &absent, "x")).unwrap();
        });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(2)).expect("near-cap content must not trigger quadratic matching");
        assert_eq!(result, Err(ContentError::NotFound));
    }

    #[test]
    fn markdown_produces_supported_headings_and_tables() {
        let rendered =
            render_input("# Titre\n\n| A | B |\n| - | - |\n| 1 | 2 |", "markdown").unwrap();
        assert!(rendered.contains("<h1>Titre</h1>"));
        assert!(rendered.contains("<table>"));
        assert_eq!(validate_html(&rendered), Ok(()));
    }

    #[test]
    fn read_views_report_loss_and_do_not_return_inline_image_payloads_as_text() {
        let html =
            "<h1>Bonjour &amp; merci</h1><p>Texte 😀</p><img src=\"data:image/png;base64,AAAA\">";
        let plain = read_view(html, "text").unwrap();
        assert!(plain.content.contains("Bonjour & merci"));
        assert!(plain.content.contains("[image]"));
        assert!(!plain.content.contains("base64"));
        let markdown = read_view(html, "markdown").unwrap();
        assert!(markdown.lossy);
        assert!(!markdown.warnings.is_empty());
        assert!(!markdown.content.contains("base64"));
        assert_eq!(read_view(html, "html").unwrap().content, html);
    }

    #[test]
    fn mutation_cap_is_utf8_bytes_and_does_not_prevent_large_note_reads() {
        let at_cap = format!("<p>{}</p>", "a".repeat(MAX_MUTATION_BYTES - 7));
        assert!(append_html("", &at_cap).is_ok());
        assert_eq!(append_html(&at_cap, "😀"), Err(ContentError::TooLarge));
        assert_eq!(
            read_view(&format!("{at_cap}😀"), "html")
                .unwrap()
                .content
                .len(),
            MAX_MUTATION_BYTES + 4
        );
    }

    #[test]
    fn title_uses_first_visible_line_and_a_unicode_safe_fifty_unit_limit() {
        assert_eq!(
            derive_note_title("<h1>Bonjour <strong>monde</strong></h1><p>Suite</p>", "fr"),
            "Bonjour monde"
        );
        assert_eq!(derive_note_title("<p></p>", "fr"), "Note sans titre");
        assert_eq!(derive_note_title("", "en"), "Untitled note");
        assert_eq!(
            derive_note_title(&format!("<h1>{}😀x</h1>", "a".repeat(49)), "fr"),
            format!("{}...", "a".repeat(49))
        );
    }

    #[test]
    fn title_matches_shared_frontend_fixtures() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/mcp-notes/titles.json"))
                .unwrap();
        for fixture in fixtures.as_array().unwrap() {
            assert_eq!(
                derive_note_title(
                    fixture["html"].as_str().unwrap(),
                    fixture["locale"].as_str().unwrap()
                ),
                fixture["title"].as_str().unwrap()
            );
        }
    }
}
