//! Sanitized Markdown and inline HTML for browser display.
/// One paragraph renders inline, so a formatted line sits among literal ones.
pub fn inline_html(markdown: &str) -> String {
    let html = render_markdown_with_marker(markdown, "awmermaid-user");
    let html = html.trim();
    match html
        .strip_prefix("<p>")
        .and_then(|inner| inner.strip_suffix("</p>"))
    {
        Some(inner) if !inner.contains("<p>") => inner.to_owned(),
        _ => html.to_owned(),
    }
}
#[cfg(test)]
fn render_markdown(text: &str) -> String {
    render_markdown_with_marker(text, "awmermaid-test")
}
pub fn render_markdown_with_marker(text: &str, marker: &str) -> String {
    let parser = pulldown_cmark::Parser::new_ext(
        text,
        pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH,
    );
    let mut out = String::new();
    let parser = parser.map(|event| match event {
        pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(
            pulldown_cmark::CodeBlockKind::Fenced(info),
        )) if info.trim().eq_ignore_ascii_case("mermaid") => pulldown_cmark::Event::Start(
            pulldown_cmark::Tag::CodeBlock(pulldown_cmark::CodeBlockKind::Fenced(marker.into())),
        ),
        pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            // A bare filename with a line suffix resembles a URL scheme to the
            // sanitizer. Make its relative-path meaning explicit before cleaning.
            let parts: Vec<_> = dest_url.split(':').collect();
            let local_line = (parts.len() == 2 || parts.len() == 3)
                && parts[0].contains('.')
                && !parts[0].contains('/')
                && parts[1..]
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
            let dest_url = if local_line {
                format!("./{dest_url}").into()
            } else {
                dest_url
            };
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            })
        }
        event => event,
    });
    // Prose outside code and existing links gets GitHub-style bare URL links.
    let (mut code, mut link) = (0usize, 0usize);
    let parser = pulldown_cmark::TextMergeStream::new(parser).flat_map(move |event| {
        use pulldown_cmark::{Event, Tag, TagEnd};
        match &event {
            Event::Start(Tag::CodeBlock(_)) => code += 1,
            Event::End(TagEnd::CodeBlock) => code = code.saturating_sub(1),
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => link += 1,
            Event::End(TagEnd::Link | TagEnd::Image) => link = link.saturating_sub(1),
            Event::Text(text) if code == 0 && link == 0 => return autolink(text),
            _ => {}
        }
        vec![event]
    });
    pulldown_cmark::html::push_html(&mut out, parser);
    ammonia::Builder::default()
        .url_relative(ammonia::UrlRelative::PassThrough)
        // Agents link host files as file:// URLs; the browser turns local ones
        // into in-app previews instead of navigating.
        .add_url_schemes(&["file", "agentworks"])
        .add_tag_attributes("code", &["class"])
        .attribute_filter(|element, attribute, value| {
            if element != "code" || attribute != "class" {
                return Some(value.into());
            }
            // Keep only fenced-code language hints for display preferences.
            let languages = value
                .split_whitespace()
                .filter(|class| {
                    class.strip_prefix("language-").is_some_and(|language| {
                        (1..=32).contains(&language.len())
                            && language
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"+#._-".contains(&b))
                    })
                })
                .collect::<Vec<_>>()
                .join(" ");
            (!languages.is_empty()).then(|| languages.into())
        })
        .clean(&out)
        .to_string()
}

/// Splits bare `http(s)://` URLs out of a text run as links. Trailing sentence
/// punctuation and unbalanced closing brackets stay outside the link.
fn autolink<'a>(text: &pulldown_cmark::CowStr<'a>) -> Vec<pulldown_cmark::Event<'a>> {
    use pulldown_cmark::{CowStr, Event, LinkType, Tag, TagEnd};
    let mut events = Vec::new();
    let (mut rest, mut plain) = (text.as_ref(), String::new());
    while let Some(start) = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| {
            rest.match_indices(scheme)
                .map(|(i, _)| i)
                .find(|&i| !rest[..i].ends_with(|c: char| c.is_alphanumeric()))
        })
        .min()
    {
        let candidate = &rest[start..];
        let mut end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`'))
            .unwrap_or(candidate.len());
        loop {
            let url = &candidate[..end];
            let unbalanced = |open, close| {
                url.ends_with(close) && url.matches(close).count() > url.matches(open).count()
            };
            if url.ends_with(['.', ',', ':', ';', '!', '?', '\'', '*', '_'])
                || unbalanced('(', ')')
                || unbalanced('[', ']')
            {
                end -= 1;
            } else {
                break;
            }
        }
        let url = &candidate[..end];
        // A scheme alone is prose, not a link.
        if !url
            .split_once("://")
            .is_some_and(|(_, host)| host.chars().next().is_some_and(char::is_alphanumeric))
        {
            plain.push_str(&rest[..start + 1]);
            rest = &rest[start + 1..];
            continue;
        }
        plain.push_str(&rest[..start]);
        if !plain.is_empty() {
            events.push(Event::Text(CowStr::from(std::mem::take(&mut plain))));
        }
        events.push(Event::Start(Tag::Link {
            link_type: LinkType::Autolink,
            dest_url: CowStr::from(url.to_owned()),
            title: CowStr::Borrowed(""),
            id: CowStr::Borrowed(""),
        }));
        events.push(Event::Text(CowStr::from(url.to_owned())));
        events.push(Event::End(TagEnd::Link));
        rest = &rest[start + end..];
    }
    if events.is_empty() {
        return vec![Event::Text(text.clone())];
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        events.push(Event::Text(CowStr::from(plain)));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markdown_links_bare_urls_outside_code() {
        let html = render_markdown(
            "See https://example.com/a_(b)?q=1, (http://x.io/path). Also `https://code.example` and [named](https://n.example).\n\n```\nhttps://fenced.example\n```\nnohttps://nope and https:// alone",
        );
        assert!(html.contains(r#"<a href="https://example.com/a_(b)?q=1" rel="noopener noreferrer">https://example.com/a_(b)?q=1</a>, "#), "{html}");
        assert!(
            html.contains(
                r#"(<a href="http://x.io/path" rel="noopener noreferrer">http://x.io/path</a>)."#
            ),
            "{html}"
        );
        assert!(html.contains("<code>https://code.example</code>"), "{html}");
        assert!(
            html.contains(r#">named</a>"#) && !html.contains(">https://n.example<"),
            "{html}"
        );
        assert!(
            html.contains("https://fenced.example\n</code>")
                && !html.contains(r#"href="https://fenced"#),
            "{html}"
        );
        assert!(
            !html.contains(r#"href="https://nope"#) && html.contains("https:// alone"),
            "{html}"
        );
    }
    #[test]
    fn markdown_removes_active_content() {
        let html = render_markdown(
            "# Hello\n<script>alert(1)</script>\n[x](javascript:alert(1))\n<img src=x onerror=alert(1)>",
        );
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("onerror"));
    }
    #[test]
    fn markdown_keeps_file_links() {
        let html = render_markdown(
            "[a](file:///home/x/a.png) [b](/home/x/b.png) [line](sample.rs:42) [bad](javascript:42)",
        );
        assert!(html.contains(r#"href="file:///home/x/a.png""#));
        assert!(html.contains(r#"href="/home/x/b.png""#));
        assert!(html.contains(r#"href="./sample.rs:42""#));
        assert!(!html.contains("javascript:"));
    }
    #[test]
    fn markdown_keeps_session_links_from_formatters() {
        let html = render_markdown(
            "[@ui-hover](agentworks://sessions/28d9887c-e7c3-4096-870c-8ccf6d4eb4ae)",
        );
        assert!(
            html.contains(r#"href="agentworks://sessions/28d9887c-e7c3-4096-870c-8ccf6d4eb4ae""#)
        );
        assert!(html.contains(">@ui-hover</a>"));
    }

    #[test]
    fn markdown_marks_only_mermaid_fences() {
        let html = render_markdown_with_marker(
            "```mermaid\nflowchart LR; A-->B\n```\n<pre><code class=\"language-mermaid\">flowchart LR; X-->Y</code></pre>\n\n```js\nconst x = 1;\n```",
            "awmermaid-0123456789abcdef",
        );
        assert_eq!(
            html.matches("language-awmermaid-0123456789abcdef").count(),
            1
        );
        assert!(html.contains("language-mermaid"));
        assert!(html.contains("language-js"));
    }
    #[test]
    fn markdown_keeps_only_code_language_classes() {
        let html = render_markdown(
            "```rust\nfn main() {}\n```\n<code class=\"toast language-x\">a</code><p class=\"language-js\">b</p>",
        );
        assert!(html.contains("<code class=\"language-rust\">"));
        assert!(html.contains("<code class=\"language-x\">"));
        assert!(!html.contains("toast"));
        assert!(!html.contains("<p class"));
    }
}
