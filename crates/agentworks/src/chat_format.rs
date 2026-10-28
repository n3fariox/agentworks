//! `chat.agent` Starlark formatters rewrite agent message lines before Markdown
//! rendering. Display only: the stored transcript and what the agent sees are
//! unchanged, and the result is sanitized like any other Markdown.
use crate::tool_progress::Hub;

/// Lines past this many are shown as written.
const MAX_LINES: usize = 2000;

/// Whether each `\n`-separated line is prose. Fenced code blocks, fences
/// included, are not; an unclosed fence runs to the end, as in CommonMark.
pub fn prose_lines(text: &str) -> Vec<bool> {
    let mut fence: Option<(u8, usize)> = None;
    text.split('\n')
        .map(|line| {
            let trimmed = line.trim_start_matches(' ');
            let indented = line.len() - trimmed.len() > 3;
            let marker = trimmed.bytes().next().filter(|b| matches!(b, b'`' | b'~'));
            let run = marker.map_or(0, |m| trimmed.bytes().take_while(|&b| b == m).count());
            let rest = &trimmed[run..];
            match fence {
                Some((open, width)) => {
                    if !indented && marker == Some(open) && run >= width && rest.trim().is_empty() {
                        fence = None;
                    }
                    false
                }
                // A backtick fence's info string cannot contain a backtick.
                None if !indented && run >= 3 && !(marker == Some(b'`') && rest.contains('`')) => {
                    fence = marker.map(|m| (m, run));
                    false
                }
                None => true,
            }
        })
        .collect()
}

/// Each `\n`-separated line's replacement from enabled `kind` formatters, or
/// None to keep it; None overall when no formatter of that kind is enabled.
/// Lines are passed in order, without a CRLF carriage return.
pub fn format(
    hub: &Hub,
    kind: &str,
    provider: &str,
    cwd: &str,
    text: &str,
) -> Option<Vec<Option<String>>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let selected: Vec<usize> = prose_lines(text)
        .into_iter()
        .enumerate()
        .filter(|&(_, prose)| prose)
        .map(|(i, _)| i)
        .take(MAX_LINES)
        .collect();
    let input = selected
        .iter()
        .map(|&i| lines[i].trim_end_matches('\r').to_owned())
        .collect();
    let formatted = hub.format_chat(kind, provider, cwd, input)?;
    let mut result = vec![None; lines.len()];
    for (i, line) in selected.into_iter().zip(formatted) {
        result[i] = line;
    }
    Some(result)
}

/// An agent message's Markdown with each formatted line replaced.
pub fn apply(hub: &Hub, provider: &str, cwd: &str, text: &str) -> String {
    let Some(formatted) = format(hub, "chat.agent", provider, cwd, text) else {
        return text.to_owned();
    };
    text.split('\n')
        .zip(formatted)
        .map(|(line, formatted)| match formatted {
            Some(f) => f + if line.ends_with('\r') { "\r" } else { "" },
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_code_is_not_prose() {
        let text = "a\n```rust\ncode\n``\n```\nb\n   ~~~~\n~~~\nc\n~~~~\n```x`y\nd\n    ```\ne";
        let prose: Vec<_> = text
            .split('\n')
            .zip(prose_lines(text))
            .filter(|(_, p)| *p)
            .map(|(l, _)| l)
            .collect();
        assert_eq!(prose, ["a", "b", "```x`y", "d", "    ```", "e"]);
        assert_eq!(
            prose_lines("```\nunclosed\nstill code"),
            [false, false, false]
        );
    }

    #[test]
    fn without_formatters_text_is_unchanged() {
        let hub = Hub::empty();
        let text = "OPS-1\r\n```\nOPS-2\n```\n\n";
        assert_eq!(apply(&hub, "claude", "/work", text), text);
    }
}
