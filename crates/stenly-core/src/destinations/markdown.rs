//! Note Markdown → Notion blocks and Slack mrkdwn.
//!
//! Only the subset Stenly writes is handled: `#`–`###` headings, paragraphs,
//! `-` bullets (one nesting level, 4-space or tab indent), `- [ ]` tasks,
//! `>` quotes / `> [!warning]` callouts, and inline `**bold**`, `*italic*`,
//! `` `code` ``, `[text](url)` and Obsidian `[[links]]`.

use serde_json::{Value, json};

/// Notion caps one text object at 2000 characters.
const NOTION_TEXT_MAX: usize = 2000;

#[derive(Debug, Clone, PartialEq)]
struct Span {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    link: Option<String>,
}

/// Parse inline Markdown into styled spans.
fn spans(s: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let (mut bold, mut italic) = (false, false);
    let mut buf = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let flush = |buf: &mut String, out: &mut Vec<Span>, bold: bool, italic: bool| {
        if !buf.is_empty() {
            out.push(Span { text: std::mem::take(buf), bold, italic, code: false, link: None });
        }
    };
    let find = |from: usize, pat: &[char]| -> Option<usize> { (from..chars.len().saturating_sub(pat.len() - 1)).find(|&j| chars[j..j + pat.len()] == *pat) };
    while i < chars.len() {
        let c = chars[i];
        // **bold**
        if c == '*' && chars.get(i + 1) == Some(&'*') && (bold || find(i + 2, &['*', '*']).is_some()) {
            flush(&mut buf, &mut out, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        }
        // *italic* / _italic_ (only at word boundaries for `_`)
        let boundary = i == 0 || !chars[i - 1].is_alphanumeric();
        if (c == '*' || (c == '_' && (italic || boundary))) && (italic || find(i + 1, &[c]).is_some_and(|j| j > i + 1)) {
            flush(&mut buf, &mut out, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        }
        // `code`
        if c == '`'
            && let Some(j) = find(i + 1, &['`'])
        {
            flush(&mut buf, &mut out, bold, italic);
            out.push(Span { text: chars[i + 1..j].iter().collect(), bold, italic, code: true, link: None });
            i = j + 1;
            continue;
        }
        // [[wiki link|alias]] → alias / target as plain text
        if c == '['
            && chars.get(i + 1) == Some(&'[')
            && let Some(j) = find(i + 2, &[']', ']'])
        {
            let inner: String = chars[i + 2..j].iter().collect();
            buf.push_str(inner.rsplit('|').next().unwrap_or(&inner));
            i = j + 2;
            continue;
        }
        // [text](url)
        if c == '['
            && let Some(j) = find(i + 1, &[']', '('])
            && let Some(k) = find(j + 2, &[')'])
        {
            let url: String = chars[j + 2..k].iter().collect();
            if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                flush(&mut buf, &mut out, bold, italic);
                out.push(Span { text: chars[i + 1..j].iter().collect(), bold, italic, code: false, link: Some(url) });
                i = k + 1;
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out, bold, italic);
    out
}

/// Inline Markdown → Notion `rich_text` (split at Notion's length limit).
pub fn rich_text(s: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for sp in spans(s) {
        let chars: Vec<char> = sp.text.chars().collect();
        for chunk in chars.chunks(NOTION_TEXT_MAX) {
            let content: String = chunk.iter().collect();
            let link = sp.link.as_ref().map(|u| json!({ "url": u })).unwrap_or(Value::Null);
            out.push(json!({
                "type": "text",
                "text": { "content": content, "link": link },
                "annotations": { "bold": sp.bold, "italic": sp.italic, "code": sp.code, "strikethrough": false, "underline": false, "color": "default" },
            }));
        }
    }
    // Notion allows 100 rich text items per block.
    out.truncate(100);
    out
}

/// Plain text (no Markdown) → rich text, for transcripts.
pub fn plain_rich_text(s: &str) -> Vec<Value> {
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(NOTION_TEXT_MAX).take(100).map(|c| json!({ "type": "text", "text": { "content": c.iter().collect::<String>() } })).collect()
}

fn block(kind: &str, rich: Vec<Value>) -> Value {
    json!({ "object": "block", "type": kind, kind: { "rich_text": rich } })
}

fn indent_level(line: &str) -> usize {
    let mut n = 0;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 4,
            _ => break,
        }
    }
    // 2–4 spaces (or a tab) = one level
    (n + 2) / 4
}

/// Note Markdown → Notion blocks (top level, with nested list children).
pub fn to_notion_blocks(md: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let flush_para = |para: &mut Vec<String>, out: &mut Vec<Value>| {
        if !para.is_empty() {
            out.push(block("paragraph", rich_text(&para.join(" "))));
            para.clear();
        }
    };
    for raw in md.lines() {
        let line = raw.trim_end();
        let t = line.trim_start();
        if t.is_empty() {
            flush_para(&mut para, &mut out);
            continue;
        }
        let level = indent_level(line);
        let item = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ "));
        if let Some(rest) = item {
            flush_para(&mut para, &mut out);
            let b = if let Some(task) = rest.strip_prefix("[ ] ").or_else(|| rest.strip_prefix("[x] ")).or_else(|| rest.strip_prefix("[X] ")) {
                let mut b = block("to_do", rich_text(task));
                b["to_do"]["checked"] = json!(!rest.starts_with("[ ]"));
                b
            } else {
                block("bulleted_list_item", rich_text(rest))
            };
            // Nested under the previous list item (Notion: children in the same request, ≤2 levels).
            let parent_kind = out.last().and_then(|p| p["type"].as_str().map(str::to_string));
            if level >= 1 && matches!(parent_kind.as_deref(), Some("bulleted_list_item" | "to_do")) {
                let pk = parent_kind.unwrap();
                let parent = out.last_mut().unwrap();
                match parent[&pk]["children"].as_array_mut() {
                    Some(ch) => ch.push(b),
                    None => parent[&pk]["children"] = json!([b]),
                }
            } else {
                out.push(b);
            }
            continue;
        }
        if let Some((hashes, text)) = t.split_once(' ')
            && !hashes.is_empty()
            && hashes.chars().all(|c| c == '#')
        {
            flush_para(&mut para, &mut out);
            let kind = match hashes.len() {
                1 => "heading_1",
                2 => "heading_2",
                _ => "heading_3",
            };
            out.push(block(kind, rich_text(text.trim())));
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            flush_para(&mut para, &mut out);
            let q = q.trim_start();
            if let Some(rest) = q.strip_prefix("[!")
                && let Some((_, text)) = rest.split_once(']')
            {
                let text = text.trim_start_matches(['-', '+']).trim();
                out.push(json!({ "object": "block", "type": "callout", "callout": { "rich_text": rich_text(text), "icon": { "emoji": "⚠️" }, "color": "default" } }));
            } else if !q.is_empty() {
                out.push(block("quote", rich_text(q)));
            }
            continue;
        }
        para.push(t.to_string());
    }
    flush_para(&mut para, &mut out);
    out
}

/// Escape for Slack mrkdwn.
fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Inline Markdown → Slack mrkdwn.
fn slack_inline(s: &str) -> String {
    let mut out = String::new();
    for sp in spans(s) {
        let mut t = slack_escape(&sp.text);
        if t.trim().is_empty() {
            out.push_str(&t);
            continue;
        }
        if let Some(u) = &sp.link {
            t = format!("<{}|{}>", u.replace('|', "%7C"), t);
        }
        if sp.code {
            t = format!("`{t}`");
        }
        // Slack needs the markers hugging the text: move edge spaces outside.
        let (lead, core, trail) = {
            let core = t.trim();
            let lead = &t[..t.len() - t.trim_start().len()];
            let trail = &t[t.trim_end().len()..];
            (lead.to_string(), core.to_string(), trail.to_string())
        };
        let mut core = core;
        if sp.italic {
            core = format!("_{core}_");
        }
        if sp.bold {
            core = format!("*{core}*");
        }
        out.push_str(&lead);
        out.push_str(&core);
        out.push_str(&trail);
    }
    out
}

/// Note Markdown → Slack mrkdwn.
pub fn to_slack(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for raw in md.lines() {
        let line = raw.trim_end();
        let t = line.trim_start();
        if t.is_empty() {
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
            continue;
        }
        let level = indent_level(line);
        let pad = "    ".repeat(level.min(3));
        if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            let l = if let Some(task) = rest.strip_prefix("[ ] ") {
                format!("{pad}☐ {}", slack_inline(task))
            } else if let Some(task) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
                format!("{pad}☑ {}", slack_inline(task))
            } else {
                format!("{pad}{} {}", if level == 0 { "•" } else { "◦" }, slack_inline(rest))
            };
            out.push(l);
            continue;
        }
        if let Some((hashes, text)) = t.split_once(' ')
            && !hashes.is_empty()
            && hashes.chars().all(|c| c == '#')
        {
            // heading: bold line (strip inner bold so markers don't cancel)
            out.push(format!("*{}*", slack_escape(text.trim()).replace('*', "")));
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            let q = q.trim_start();
            let q = match q.strip_prefix("[!").and_then(|r| r.split_once(']')) {
                Some((_, text)) => text.trim_start_matches(['-', '+']).trim(),
                None => q,
            };
            if !q.is_empty() {
                out.push(format!("> {}", slack_inline(q)));
            }
            continue;
        }
        out.push(slack_inline(t));
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// Split text into chunks of at most `max` chars at line breaks (Slack
/// section blocks hold 3000).
pub fn chunk_lines(s: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in s.lines() {
        let line: String = line.chars().take(max).collect();
        if cur.chars().count() + line.chars().count() + 1 > max && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(&line);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(v: &Value) -> String {
        v.as_array().unwrap().iter().map(|r| r["text"]["content"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn inline_styles() {
        let r = rich_text("We **agreed** on *scope*, see [doc](https://x.io) and `cfg` with [[Ana Lee|Ana]]");
        let parts: Vec<(&str, bool, bool, bool)> = r
            .iter()
            .map(|x| (x["text"]["content"].as_str().unwrap(), x["annotations"]["bold"].as_bool().unwrap(), x["annotations"]["italic"].as_bool().unwrap(), x["annotations"]["code"].as_bool().unwrap()))
            .collect();
        assert_eq!(parts[0], ("We ", false, false, false));
        assert_eq!(parts[1], ("agreed", true, false, false));
        assert_eq!(parts[3], ("scope", false, true, false));
        assert_eq!(r[5]["text"]["link"]["url"], "https://x.io");
        assert!(parts.iter().any(|p| p.0 == "cfg" && p.3));
        assert!(parts.last().unwrap().0.ends_with("with Ana"));
        // unmatched markers stay literal
        assert_eq!(plain(&json!(rich_text("2 * 3 = 6, snake_case"))), "2 * 3 = 6, snake_case");
    }

    #[test]
    fn long_text_is_split() {
        let s = "a".repeat(4500);
        let r = rich_text(&s);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["text"]["content"].as_str().unwrap().len(), 2000);
    }

    #[test]
    fn blocks() {
        let md = "## Summary\n\nWe met.\nIt went well.\n\n### Pilot\n\n- Start in Nov\n    - Two teams\n- Budget\n\n## Action items\n\n- [ ] **Me**: Send it\n\n> [!warning] Summary failed: x\n";
        let b = to_notion_blocks(md);
        let kinds: Vec<&str> = b.iter().map(|x| x["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["heading_2", "paragraph", "heading_3", "bulleted_list_item", "bulleted_list_item", "heading_2", "to_do", "callout"]);
        assert_eq!(plain(&b[1]["paragraph"]["rich_text"]), "We met. It went well.");
        assert_eq!(plain(&b[3]["bulleted_list_item"]["children"][0]["bulleted_list_item"]["rich_text"]), "Two teams");
        assert_eq!(b[6]["to_do"]["checked"], false);
        assert_eq!(plain(&b[7]["callout"]["rich_text"]), "Summary failed: x");
    }

    #[test]
    fn slack() {
        let md = "## Summary\n\nWe **agreed** on <scope> & [doc](https://x.io).\n\n### Pilot\n\n- Start\n    - Two teams\n- [ ] **Me**: Send it";
        let s = to_slack(md);
        assert_eq!(s, "*Summary*\n\nWe *agreed* on &lt;scope&gt; &amp; <https://x.io|doc>.\n\n*Pilot*\n\n• Start\n    ◦ Two teams\n☐ *Me*: Send it");
    }

    #[test]
    fn chunks() {
        let s = (0..100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let c = chunk_lines(&s, 100);
        assert!(c.iter().all(|x| x.chars().count() <= 100));
        assert_eq!(c.join("\n"), s);
    }
}
