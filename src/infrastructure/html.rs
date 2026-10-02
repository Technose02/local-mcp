//! HTML helpers built on the `tl` parser: DuckDuckGo result parsing, readable
//! text extraction, link discovery and small text utilities.

use std::collections::HashSet;

use url::Url;

use crate::domain::error::ProviderError;
use crate::domain::fetch::Link;
use crate::domain::search::SearchHit;

/// Block-level tags whose text is kept by the readable extractor.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "pre",
    "blockquote",
    "td",
    "dd",
    "dt",
    "figcaption",
];

/// Tags whose entire subtree is skipped by the readable extractor.
const SKIP_TAGS: &[&str] = &[
    "nav", "header", "footer", "aside", "script", "style", "noscript", "form", "svg",
];

/// Collapse all runs of whitespace into single spaces.
pub fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove HTML tags and resolve entities in a short text fragment.
pub fn strip_tags(value: &str) -> String {
    let mut without_tags = String::with_capacity(value.len());
    let mut inside_tag = false;
    for character in value.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => without_tags.push(character),
            _ => {}
        }
    }
    normalize_whitespace(&unescape_html_entities(&without_tags))
}

/// Decode the handful of HTML entities that appear in search results.
pub fn unescape_html_entities(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '&' {
            let mut end = index + 1;
            while end < characters.len() && end - index <= 12 && characters[end] != ';' {
                end += 1;
            }
            if end < characters.len() && characters[end] == ';' {
                let entity: String = characters[index + 1..end].iter().collect();
                if let Some(decoded) = decode_entity(&entity) {
                    output.push_str(&decoded);
                    index = end + 1;
                    continue;
                }
            }
        }
        output.push(characters[index]);
        index += 1;
    }
    output
}

fn decode_entity(entity: &str) -> Option<String> {
    let named = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        _ => {
            let code = entity.strip_prefix('#')?;
            let value = if let Some(hex) = code.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                code.parse::<u32>().ok()?
            };
            return char::from_u32(value).map(String::from);
        }
    };
    Some(named.to_string())
}

/// Percent-decode a URI component without pulling in a dedicated crate.
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            output.push(high * 16 + low);
            index += 3;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Whether a DuckDuckGo result URL is a sponsored/advert result.
pub fn is_ddg_ad(url: &str) -> bool {
    url.contains("duckduckgo.com/y.js")
        || url.contains("ad_provider=")
        || url.contains("ad_domain=")
}

/// Read an attribute as an owned string, if present and valid UTF-8.
fn attr<'a>(tag: &tl::HTMLTag<'a>, name: &'a str) -> Option<String> {
    tag.attributes()
        .get(name)
        .flatten()
        .and_then(|bytes| bytes.try_as_utf8_str())
        .map(str::to_string)
}

/// Resolve a DuckDuckGo result href (which is usually a `uddg=` redirect).
fn decode_ddg_href(href: &str) -> String {
    let href = unescape_html_entities(href);
    let target = match href.find("uddg=") {
        Some(index) => {
            let rest = &href[index + "uddg=".len()..];
            let encoded = rest.split('&').next().unwrap_or(rest);
            percent_decode(encoded)
        }
        None => href.clone(),
    };
    let target = target.trim();
    if let Some(stripped) = target.strip_prefix("//") {
        format!("https://{stripped}")
    } else if target.starts_with('/') {
        format!("https://duckduckgo.com{target}")
    } else {
        target.to_string()
    }
}

/// Parse a DuckDuckGo HTML results page.
pub fn parse_ddg_results(html: &str) -> Result<Vec<SearchHit>, ProviderError> {
    let dom = tl::parse(html, tl::ParserOptions::default()).map_err(|error| {
        ProviderError::decode("duckduckgo", format!("HTML parse error: {error}"))
    })?;
    let parser = dom.parser();

    let titles: Vec<(String, String)> = dom
        .query_selector("a.result__a")
        .map(|iterator| {
            iterator
                .filter_map(|handle| handle.get(parser))
                .filter_map(|node| node.as_tag())
                .filter_map(|tag| {
                    let href = attr(tag, "href")?;
                    let title = normalize_whitespace(&tag.inner_text(parser));
                    Some((title, decode_ddg_href(&href)))
                })
                .collect()
        })
        .unwrap_or_default();

    let snippets: Vec<String> = dom
        .query_selector("a.result__snippet")
        .map(|iterator| {
            iterator
                .filter_map(|handle| handle.get(parser))
                .filter_map(|node| node.as_tag())
                .map(|tag| normalize_whitespace(&tag.inner_text(parser)))
                .collect()
        })
        .unwrap_or_default();

    let mut hits = Vec::with_capacity(titles.len());
    for (index, (title, url)) in titles.into_iter().enumerate() {
        if title.is_empty() || url.is_empty() || is_ddg_ad(&url) {
            continue;
        }
        let snippet = snippets.get(index).cloned().unwrap_or_default();
        hits.push(SearchHit {
            title,
            url,
            snippet,
            published_date: None,
        });
    }

    if hits.is_empty() {
        let challenged = html.contains("anomaly") || html.contains("challenge");
        return Err(if challenged {
            ProviderError::blocked("duckduckgo", "bot-detection challenge returned")
        } else {
            ProviderError::unavailable("duckduckgo", "no results parsed from HTML")
        });
    }

    Ok(hits)
}

/// Extract a readable title + plain-text body from an HTML document.
pub fn extract_readable(html: &str) -> (Option<String>, String) {
    let Ok(dom) = tl::parse(html, tl::ParserOptions::default()) else {
        return (None, String::new());
    };
    let parser = dom.parser();

    let title = dom
        .query_selector("title")
        .and_then(|mut iterator| iterator.next())
        .and_then(|handle| handle.get(parser))
        .map(|node| normalize_whitespace(&node.inner_text(parser)))
        .filter(|title| !title.is_empty());

    // Byte ranges of subtrees (nav/footer/scripts/...) we want to ignore.
    let mut skip_ranges: Vec<(usize, usize)> = Vec::new();
    for name in SKIP_TAGS {
        if let Some(iterator) = dom.query_selector(name) {
            for handle in iterator {
                if let Some(tag) = handle.get(parser).and_then(|node| node.as_tag()) {
                    skip_ranges.push(tag.boundaries(parser));
                }
            }
        }
    }

    let mut blocks: Vec<String> = Vec::new();
    for node in dom.nodes() {
        let Some(tag) = node.as_tag() else { continue };
        let name = tag.name().as_utf8_str().to_ascii_lowercase();
        if !BLOCK_TAGS.contains(&name.as_str()) {
            continue;
        }
        let (start, end) = tag.boundaries(parser);
        if skip_ranges
            .iter()
            .any(|(skip_start, skip_end)| start >= *skip_start && end <= *skip_end)
        {
            continue;
        }
        let text = normalize_whitespace(&tag.inner_text(parser));
        if text.is_empty() {
            continue;
        }
        if blocks.last().map(|last| last == &text).unwrap_or(false) {
            continue;
        }
        blocks.push(text);
    }

    (title, blocks.join("\n\n"))
}

/// Extract absolute links from an HTML document.
pub fn extract_links(html: &str, base_url: &str, include_external: bool) -> Vec<Link> {
    let Ok(dom) = tl::parse(html, tl::ParserOptions::default()) else {
        return Vec::new();
    };
    let parser = dom.parser();
    let base = Url::parse(base_url).ok();
    let base_host = base
        .as_ref()
        .and_then(|url| url.host_str().map(str::to_string));

    let mut seen = HashSet::new();
    let mut links = Vec::new();
    let Some(iterator) = dom.query_selector("a[href]") else {
        return links;
    };

    for handle in iterator {
        let Some(tag) = handle.get(parser).and_then(|node| node.as_tag()) else {
            continue;
        };
        let Some(href) = attr(tag, "href") else {
            continue;
        };
        let href = unescape_html_entities(&href);
        let href = href.trim();
        if href.is_empty()
            || href.starts_with('#')
            || href.starts_with("mailto:")
            || href.starts_with("javascript:")
            || href.starts_with("tel:")
            || href.starts_with("data:")
        {
            continue;
        }

        let Some(base) = &base else { continue };
        let Ok(resolved) = base.join(href) else {
            continue;
        };
        if !include_external
            && let Some(expected) = &base_host
            && resolved.host_str() != Some(expected.as_str())
        {
            continue;
        }
        let resolved = resolved.to_string();
        let text = normalize_whitespace(&tag.inner_text(parser));
        if seen.insert(resolved.clone()) {
            links.push(Link {
                text: if text.is_empty() {
                    resolved.clone()
                } else {
                    text
                },
                url: resolved,
            });
        }
    }

    links
}

/// Extract `<loc>` URLs from a sitemap document.
pub fn extract_sitemap_locations(body: &str) -> Vec<String> {
    let mut locations = Vec::new();
    let mut remainder = body;
    while let Some(start) = remainder.find("<loc>") {
        let after = &remainder[start + "<loc>".len()..];
        let Some(end) = after.find("</loc>") else {
            break;
        };
        let value = unescape_html_entities(after[..end].trim());
        if !value.is_empty() {
            locations.push(value);
        }
        remainder = &after[end + "</loc>".len()..];
    }
    locations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_utf8() {
        assert_eq!(percent_decode("rust%20programming"), "rust programming");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("no-encoding"), "no-encoding");
    }

    #[test]
    fn unescape_entities_handles_named_and_numeric() {
        assert_eq!(unescape_html_entities("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(unescape_html_entities("it&#39;s"), "it's");
        assert_eq!(unescape_html_entities("plain"), "plain");
    }

    #[test]
    fn strip_tags_removes_markup() {
        assert_eq!(
            strip_tags("<span class=\"searchmatch\">Rust</span> lang"),
            "Rust lang"
        );
    }

    #[test]
    fn parse_ddg_results_skips_ads_and_decodes_urls() {
        let html = r#"
            <div class="result">
              <a class="result__a" href="https://duckduckgo.com/y.js?ad_domain=udemy.com&amp;ad_provider=bing">Ad Course</a>
              <a class="result__snippet">Buy now</a>
            </div>
            <div class="result">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=abc">Rust Programming Language</a>
              <a class="result__snippet">A language empowering everyone.</a>
            </div>
        "#;
        let hits = parse_ddg_results(html).expect("parses");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].snippet, "A language empowering everyone.");
    }

    #[test]
    fn parse_ddg_results_detects_challenge() {
        let error = parse_ddg_results("<html>anomaly challenge</html>").unwrap_err();
        assert!(matches!(error, ProviderError::Blocked { .. }));
    }

    #[test]
    fn extract_links_resolves_relative_and_filters_external() {
        let html = r##"
            <a href="/docs">Docs</a>
            <a href="https://other.test/external">External</a>
            <a href="#section">Anchor</a>
        "##;
        let links = extract_links(html, "https://example.com/", false);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/docs");
        let all = extract_links(html, "https://example.com/", true);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn extract_readable_skips_navigation() {
        let html = r#"
            <html><head><title>Hello</title></head>
            <body>
              <nav><p>Home About Contact</p></nav>
              <article>
                <h1>Heading</h1>
                <p>First paragraph.</p>
                <p>Second paragraph.</p>
              </article>
              <footer><p>Copyright</p></footer>
            </body></html>
        "#;
        let (title, text) = extract_readable(html);
        assert_eq!(title.as_deref(), Some("Hello"));
        assert!(text.contains("First paragraph."));
        assert!(text.contains("Second paragraph."));
        assert!(!text.contains("Copyright"));
    }

    #[test]
    fn extract_sitemap_locations_reads_locs() {
        let xml = "<urlset><url><loc>https://a.test/1</loc></url><url><loc>https://a.test/2</loc></url></urlset>";
        assert_eq!(
            extract_sitemap_locations(xml),
            vec!["https://a.test/1", "https://a.test/2"]
        );
    }
}
