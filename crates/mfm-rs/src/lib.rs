//! Inline MFM (Misskey Markup Language) subset: the node types
//! `MfmService.toHtml` renders on the feed / AP planes — text, bold, italic,
//! strike, small, inline code. Hand scanner + visitor, pinned to the upstream
//! `packages/backend/src/core/MfmService.ts` handlers; the full mfm-js AST
//! port lands in M2 (needed by NoteCreateService).

/// One inline MFM node — the `mfm.MfmNode` subset the upstream
/// `MfmService.toHtml` visitor (src/core/MfmService.ts) renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Plain text — escaped at render time, newlines → `<br />`.
    Text(String),
    /// `**bold**` → `<b>…</b>`.
    Bold(Vec<Node>),
    /// `*italic*` / `<i>italic</i>` → `<i>…</i>`.
    Italic(Vec<Node>),
    /// `~~strike~~` → `<del>…</del>`.
    Strike(Vec<Node>),
    /// `<small>small</small>` → `<small>…</small>`.
    Small(Vec<Node>),
    /// `` `code` `` → `<code>…</code>`; the span is verbatim — markers inside
    /// are never rescanned.
    InlineCode(String),
}

/// Parse an inline MFM string. A marker only folds when its matching closer
/// exists; unterminated markup stays literal text.
pub fn parse(input: &str) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < input.len() {
        match try_open(input, i) {
            Some((node, next)) => {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                nodes.push(node);
                i = next;
            }
            None => {
                let ch = input[i..].chars().next().unwrap();
                text.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    if !text.is_empty() {
        nodes.push(Node::Text(text));
    }
    nodes
}

/// Fold one markup node at byte offset `start` if an opener + closer pair
/// begins there; yields the node and the offset just past the closer.
fn try_open(input: &str, start: usize) -> Option<(Node, usize)> {
    let rest = &input[start..];
    // inline code first: the span is verbatim, nothing inside is rescanned
    if let Some(body) = rest.strip_prefix('`') {
        if let Some(end) = body.find('`') {
            if end > 0 {
                return Some((Node::InlineCode(body[..end].to_string()), end + 2));
            }
        }
    }
    let (open, close, wrap): (&str, &str, fn(Vec<Node>) -> Node) = if rest.starts_with("**") {
        ("**", "**", Node::Bold)
    } else if rest.starts_with("~~") {
        ("~~", "~~", Node::Strike)
    } else if rest.starts_with("<small>") {
        ("<small>", "</small>", Node::Small)
    } else if rest.starts_with("<i>") {
        ("<i>", "</i>", Node::Italic)
    } else if rest.starts_with('*') {
        ("*", "*", Node::Italic)
    } else {
        return None;
    };
    let body = &rest[open.len()..];
    let end = body.find(close)?;
    if end == 0 {
        return None; // empty fold stays literal (mfm-js rejects empty spans)
    }
    Some((wrap(parse(&body[..end])), open.len() + end + close.len()))
}

/// Render nodes to HTML — the `MfmService.toHtml` visitor subset:
/// bold→`<b>`, strike→`<del>`, italic→`<i>`, small→`<small>`,
/// inlineCode→`<code>` (content escaped), text nodes escaped through
/// `escape` with newlines folded to `<br />`.
pub fn to_html(nodes: &[Node], escape: &dyn Fn(&str) -> String) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => {
                // upstream text handler: escape, then \r\n|\r|\n → `<br />`
                out.push_str(
                    &escape(t)
                        .replace("\r\n", "<br />")
                        .replace('\r', "<br />")
                        .replace('\n', "<br />"),
                );
            }
            Node::Bold(children) => {
                out.push_str("<b>");
                out.push_str(&to_html(children, escape));
                out.push_str("</b>");
            }
            Node::Italic(children) => {
                out.push_str("<i>");
                out.push_str(&to_html(children, escape));
                out.push_str("</i>");
            }
            Node::Strike(children) => {
                out.push_str("<del>");
                out.push_str(&to_html(children, escape));
                out.push_str("</del>");
            }
            Node::Small(children) => {
                out.push_str("<small>");
                out.push_str(&to_html(children, escape));
                out.push_str("</small>");
            }
            Node::InlineCode(code) => {
                out.push_str("<code>");
                out.push_str(&escape(code));
                out.push_str("</code>");
            }
        }
    }
    out
}

/// The upstream `misc/escape-html.ts` `escapeHtml` — the exact five-special
/// set (`& < > " '`, apostrophe → `&#039;`) MfmService applies to text and
/// code nodes.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render helper — pins ride the upstream escape set
    /// (`misc/escape-html.ts`, the five specials with `&#039;`).
    fn render(input: &str) -> String {
        to_html(&parse(input), &escape_html)
    }

    // Parity pins: packages/backend/src/core/MfmService.ts `toHtml` handlers
    // — bold→<b>, small→<small>, strike→<del>, italic→<i>,
    // inlineCode→<code> with escapeHtml(node.props.code) — over escapeHtml.

    #[test]
    fn bold_folds_to_b() {
        assert_eq!(render("**a**"), "<b>a</b>");
    }

    #[test]
    fn italic_folds_to_i() {
        assert_eq!(render("*a*"), "<i>a</i>");
        assert_eq!(render("<i>a</i>"), "<i>a</i>");
    }

    #[test]
    fn strike_folds_to_del() {
        assert_eq!(render("~~a~~"), "<del>a</del>");
    }

    #[test]
    fn small_folds_to_small() {
        assert_eq!(render("<small>a</small>"), "<small>a</small>");
    }

    #[test]
    fn inline_code_escapes_content_verbatim() {
        // visitor: `<code>${escapeHtml(node.props.code)}</code>`; the escaped
        // payload mirrors the pinned upstream toHtml "escape" case
        // (`&lt;p&gt;Hello, world!&lt;/p&gt;`).
        let expected = "<code>&lt;p&gt;Hello, world!&lt;/p&gt;</code>";
        assert_eq!(render("`<p>Hello, world!</p>`"), expected);
    }

    #[test]
    fn code_is_non_nesting() {
        assert_eq!(render("`**a**`"), "<code>**a**</code>");
    }

    #[test]
    fn text_nodes_escape_upstream_set() {
        assert_eq!(render("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&#039;f");
    }

    #[test]
    fn text_newlines_fold_to_br() {
        // pinned upstream toHtml cases "br" + "br alt"
        assert_eq!(render("foo\nbar\nbaz"), "foo<br />bar<br />baz");
        assert_eq!(render("foo\r\nbar\rbaz"), "foo<br />bar<br />baz");
    }

    #[test]
    fn nested_children_render() {
        assert_eq!(render("**a *b* c**"), "<b>a <i>b</i> c</b>");
        assert_eq!(render("<small>**a**</small>"), "<small><b>a</b></small>");
    }

    #[test]
    fn unterminated_markup_stays_literal() {
        assert_eq!(render("**a"), "**a");
        assert_eq!(render("a * b"), "a * b");
        assert_eq!(render("<i>a"), "&lt;i&gt;a");
    }
}
