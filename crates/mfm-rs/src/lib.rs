//! MFM (Misskey Markup Language) subset: the node types `MfmService.toHtml`
//! renders on the feed / AP planes — text, bold, italic, strike, small,
//! inline code, block code, quote, center, emoji code. Hand scanner +
//! visitor, pinned to the upstream `packages/backend/src/core/MfmService.ts`
//! handlers; the full mfm-js AST port lands in M2 (needed by
//! NoteCreateService).

/// One MFM node — the `mfm.MfmNode` subset the upstream
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
    /// Fenced ```` ``` ```` block → `<pre><code>…</code></pre>`; the body is
    /// verbatim. The mfm-js `lang` prop is dropped — the pinned upstream
    /// blockCode arm never reads it.
    BlockCode(String),
    /// `> …` line run → `<blockquote>…</blockquote>`; the stripped lines are
    /// re-parsed (which is how `>> …` nests).
    Quote(Vec<Node>),
    /// `<center>…</center>` line → `<div style="text-align: center;">…</div>`;
    /// the inner span is re-parsed with the inline-only rule set.
    Center(Vec<Node>),
    /// `:name:` custom emoji → `\u{200B}:name:\u{200B}` (the upstream arm
    /// wraps the code in zero-width spaces, it does not emit an `<img>`).
    EmojiCode(String),
    /// `#tag` → `<a href="{config.url}/tags/tag" rel="tag">#tag</a>`; the
    /// rendered href rides the `url` argument of [`to_html`].
    Hashtag(String),
}

/// The upstream `IMentionedRemoteUsers` entry (src/models/Note.ts) — the
/// fields the mention arm reads for the href override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionedRemoteUser {
    pub username: String,
    pub host: String,
    /// `remoteUser.url` — preferred over `uri` when present.
    pub url: Option<String>,
    pub uri: String,
}

/// Scanner context: `inline_only` selects the mfm-js `inline` alternation
/// (block rules dropped — it is what quote/center content runs under), and
/// `depth` mirrors the mfm-js `nestLimit` (20) so `>>>>…` spam cannot recurse
/// without bound.
#[derive(Clone, Copy)]
struct Ctx {
    inline_only: bool,
    depth: u32,
}

/// Parse an MFM string. A marker only folds when its matching closer exists;
/// unterminated markup stays literal text.
pub fn parse(input: &str) -> Vec<Node> {
    parse_ctx(
        input,
        Ctx {
            inline_only: false,
            depth: 0,
        },
    )
}

/// The mfm-js `inline` alternation — block rules dropped (center/quote
/// content runs under it upstream).
fn parse_inline(input: &str, depth: u32) -> Vec<Node> {
    parse_ctx(
        input,
        Ctx {
            inline_only: true,
            depth,
        },
    )
}

fn parse_ctx(input: &str, ctx: Ctx) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < input.len() {
        match try_open(input, i, ctx) {
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

/// mfm-js `newLine` (`\r\n` | `\r` | `\n`) — its byte length at `i`, if any.
fn newline_len(input: &str, i: usize) -> Option<usize> {
    let b = input.as_bytes();
    match b.get(i)? {
        b'\r' if b.get(i + 1) == Some(&b'\n') => Some(2),
        b'\r' | b'\n' => Some(1),
        _ => None,
    }
}

/// mfm-js `lineBegin`: position 0 or right after a newline.
fn at_line_begin(input: &str, i: usize) -> bool {
    i == 0 || matches!(input.as_bytes()[i - 1], b'\r' | b'\n')
}

/// mfm-js `lineEnd`: at input end or right before a newline.
fn at_line_end(input: &str, i: usize) -> bool {
    i == input.len() || matches!(input.as_bytes()[i], b'\r' | b'\n')
}

/// One mfm-js `space` (`' ' | \u{3000} | '\t'`) at `i`.
fn space_len(input: &str, i: usize) -> usize {
    let rest = &input[i..];
    if rest.starts_with(' ') || rest.starts_with('\t') {
        1
    } else if rest.starts_with('\u{3000}') {
        '\u{3000}'.len_utf8()
    } else {
        0
    }
}

/// Fold one markup node at byte offset `start` if a rule begins there; yields
/// the node and the offset just past it. Rule order mirrors the upstream
/// mfm-js `full` / `inline` alternations.
fn try_open(input: &str, start: usize, ctx: Ctx) -> Option<(Node, usize)> {
    if !ctx.inline_only {
        if let Some(hit) = try_code_block(input, start) {
            return Some(hit);
        }
        if let Some(hit) = try_quote(input, start, ctx) {
            return Some(hit);
        }
        if let Some(hit) = try_center(input, start, ctx) {
            return Some(hit);
        }
    }
    if let Some(hit) = try_emoji_code(input, start) {
        return Some(hit);
    }
    if let Some(hit) = try_hashtag(input, start) {
        return Some(hit);
    }
    let rest = &input[start..];
    // inline code first: the span is verbatim, nothing inside is rescanned
    if let Some(body) = rest.strip_prefix('`') {
        if let Some(end) = body.find('`') {
            if end > 0 {
                return Some((Node::InlineCode(body[..end].to_string()), start + end + 2));
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
    // ABSOLUTE offset: rest/body/end are all measured within the
    // &input[start..] suffix, so the just-past-the-closer position must add
    // `start` back — parse assigns it straight onto `i`. Relative values
    // made every node starting at a nonzero offset rewind the scanner
    // ("**a *b* c**" re-emitted the mid-span literal as "b* c"; the sibling
    // single-node pins never noticed because start == 0 there).
    Some((
        wrap(parse_ctx(
            &body[..end],
            Ctx {
                inline_only: ctx.inline_only,
                depth: ctx.depth + 1,
            },
        )),
        start + open.len() + end + close.len(),
    ))
}

/// mfm-js codeBlock: `newLine.option + lineBegin + "```" + lang-chars +
/// newLine + code-chars(≥1) + newLine + "```" + lineEnd + newLine.option`.
/// The closer is the first `` \n``` `` sitting at a line end; everything
/// between is verbatim (`code = chars.join("")`, trailing fence not included).
fn try_code_block(input: &str, start: usize) -> Option<(Node, usize)> {
    let mut i = start;
    // newLine.option(): a block may swallow the newline that precedes its
    // first line ("a\n```…" folds with the `\n` gone, matching mfm-js).
    if let Some(n) = newline_len(input, i) {
        if at_line_begin(input, i + n) {
            i += n;
        }
    }
    if !at_line_begin(input, i) || !input[i..].starts_with("```") {
        return None;
    }
    i += 3;
    // lang line: chars up to the newline (may be empty); .trim()ed, and the
    // pinned render arm never reads it, so it is dropped here.
    let lang_start = i;
    while i < input.len() && !matches!(input.as_bytes()[i], b'\r' | b'\n') {
        i += 1;
    }
    let _lang = input[lang_start..i].trim();
    i += newline_len(input, i)?;
    // code: chars until the first `\n``` at a line end — a fence that has
    // trailing text on its line is body content, not a closer.
    let code_start = i;
    let mut j = i;
    loop {
        if let Some(n) = newline_len(input, j) {
            if input[j + n..].starts_with("```") && at_line_end(input, j + n + 3) {
                break;
            }
        }
        let Some(ch) = input[j..].chars().next() else {
            return None; // ran out of input: unterminated fence stays literal
        };
        j += ch.len_utf8();
    }
    if j == code_start {
        return None; // many(1): an empty body is not a code block
    }
    let code = &input[code_start..j];
    let mut end = j + newline_len(input, j)? + 3;
    if !at_line_end(input, end) {
        return None;
    }
    if let Some(n) = newline_len(input, end) {
        end += n;
    }
    Some((Node::BlockCode(code.to_string()), end))
}

/// mfm-js quote: `newLine.option ×2 + lineBegin + ('>' + space.option +
/// rest-of-line)+ + newLine.option ×2`. Only the consecutive `>`-lines fold;
/// the stripped lines are joined with `\n` and re-parsed (so `>> …` nests and
/// inner markup renders). A run whose every line is empty is not a quote.
fn try_quote(input: &str, start: usize, ctx: Ctx) -> Option<(Node, usize)> {
    if ctx.depth >= 20 {
        return None; // mfm-js nestLimit
    }
    let mut i = start;
    for _ in 0..2 {
        match newline_len(input, i) {
            Some(n) if at_line_begin(input, i + n) => i += n,
            _ => break,
        }
    }
    if !at_line_begin(input, i) {
        return None;
    }
    let mut lines: Vec<&str> = Vec::new();
    loop {
        if !input[i..].starts_with('>') {
            break;
        }
        let mut j = i + 1;
        j += space_len(input, j);
        let content_start = j;
        while j < input.len() && !matches!(input.as_bytes()[j], b'\r' | b'\n') {
            j += 1;
        }
        lines.push(&input[content_start..j]);
        match newline_len(input, j) {
            Some(n) => i = j + n,
            None => {
                i = j;
                break;
            }
        }
    }
    if lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()) {
        return None;
    }
    let node = Node::Quote(parse_ctx(
        &lines.join("\n"),
        Ctx {
            inline_only: false,
            depth: ctx.depth + 1,
        },
    ));
    for _ in 0..2 {
        match newline_len(input, i) {
            Some(n) => i += n,
            None => break,
        }
    }
    Some((node, i))
}

/// mfm-js centerTag: `newLine.option + lineBegin + "<center>" +
/// newLine.option + inline-nodes(≥1) + newLine.option + "</center>" + lineEnd
/// + newLine.option` — the whole line must be the tag pair (the close must
/// land at a line end, so `<center>a</center>z` stays literal).
fn try_center(input: &str, start: usize, ctx: Ctx) -> Option<(Node, usize)> {
    if ctx.depth >= 20 {
        return None;
    }
    let mut i = start;
    if let Some(n) = newline_len(input, i) {
        if at_line_begin(input, i + n) {
            i += n;
        }
    }
    if !at_line_begin(input, i) || !input[i..].starts_with("<center>") {
        return None;
    }
    i += "<center>".len();
    if let Some(n) = newline_len(input, i) {
        i += n;
    }
    let content_start = i;
    // content stops at the first `</center>` — or at a `\n` directly before
    // one (the notMatch(newLine.option + close) of the upstream rule).
    let content_end = loop {
        if input[i..].starts_with("</center>") {
            break i;
        }
        if let Some(n) = newline_len(input, i) {
            if input[i + n..].starts_with("</center>") {
                break i;
            }
        }
        let Some(ch) = input[i..].chars().next() else {
            return None; // no closer: stays literal
        };
        i += ch.len_utf8();
    };
    if content_end == content_start {
        return None; // many(1)
    }
    let node = Node::Center(parse_inline(
        &input[content_start..content_end],
        ctx.depth + 1,
    ));
    let mut end = content_end;
    if let Some(n) = newline_len(input, end) {
        end += n;
    }
    if !input[end..].starts_with("</center>") || !at_line_end(input, end + "</center>".len()) {
        return None;
    }
    end += "</center>".len();
    if let Some(n) = newline_len(input, end) {
        end += n;
    }
    Some((node, end))
}

/// mfm-js emojiCode: `":" [a-z0-9_+-]+ ":" (lineEnd | not-alnum)`. The
/// upstream left guard `alt([lineBegin, not-alnum])` always passes on the
/// leading `':'`, so only the right side is load-bearing (`x:emoji:` folds,
/// `:emoji:x` does not).
fn try_emoji_code(input: &str, start: usize) -> Option<(Node, usize)> {
    if !input[start..].starts_with(':') {
        return None;
    }
    let name_start = start + 1;
    let mut j = name_start;
    while let Some(&b) = input.as_bytes().get(j) {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-') {
            j += 1;
        } else {
            break;
        }
    }
    if j == name_start {
        return None; // empty name
    }
    if input.as_bytes().get(j) != Some(&b':') {
        return None;
    }
    if !at_line_end(input, j + 1) {
        // /[a-z0-9]/i — ASCII only
        let next = input[j + 1..].chars().next().unwrap();
        if next.is_ascii_alphanumeric() {
            return None;
        }
    }
    Some((Node::EmojiCode(input[name_start..j].to_string()), j + 1))
}

/// mfm-js hashtag: `'#' + item+` where an item is a `(`/`[`/`「`/`（` bracket
/// group (nestable, same-type closer) or a char outside the close set
/// `[ \u{3000}\t.,!?'"#:/[\]【】()「」（）<>\r\n]`. Not a tag: preceded by an
/// ASCII alnum, empty run, or an all-digit run. The tag text is raw (the
/// render arm escapes + encodeURIComponent's it).
fn try_hashtag(input: &str, start: usize) -> Option<(Node, usize)> {
    if !input[start..].starts_with('#') {
        return None;
    }
    // upstream: notMatch(/[a-z0-9]$/i) over the text before the '#'
    if let Some(prev) = input[..start].chars().next_back() {
        if prev.is_ascii_alphanumeric() {
            return None;
        }
    }
    let mut j = start + 1;
    while let Some(next) = hash_tag_item(input, j, 0) {
        j = next;
    }
    let tag = &input[start + 1..j];
    if tag.is_empty() || tag.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((Node::Hashtag(tag.to_string()), j))
}

/// The four nestable bracket pairs a hashtag item may open.
const HASH_TAG_BRACKETS: [(&str, &str); 4] = [("(", ")"), ("[", "]"), ("「", "」"), ("（", "）")];

/// One hashtag item; `None` ends the run (the rest stays literal text).
fn hash_tag_item(input: &str, i: usize, depth: u32) -> Option<usize> {
    if depth < 20 {
        for (open, close) in HASH_TAG_BRACKETS {
            if input[i..].starts_with(open) {
                let mut j = i + open.len();
                loop {
                    if input[j..].starts_with(close) {
                        return Some(j + close.len());
                    }
                    j = hash_tag_item(input, j, depth + 1)?;
                }
            }
        }
    }
    let ch = input[i..].chars().next()?;
    let excluded = matches!(
        ch,
        ' ' | '\t'
            | '\u{3000}'
            | '.'
            | ','
            | '!'
            | '?'
            | '\''
            | '"'
            | '#'
            | ':'
            | '/'
            | '['
            | ']'
            | '【'
            | '】'
            | '('
            | ')'
            | '「'
            | '」'
            | '（'
            | '）'
            | '<'
            | '>'
            | '\r'
            | '\n'
    );
    if excluded {
        None
    } else {
        Some(i + ch.len_utf8())
    }
}

/// JS `encodeURIComponent` — unreserved set `A-Za-z0-9 - _ . ! ~ * ' ( )`;
/// every other byte rides as UTF-8 percent-encoding (uppercase hex).
fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Render nodes to HTML — the `MfmService.toHtml` visitor subset:
/// bold→`<b>`, strike→`<del>`, italic→`<i>`, small→`<small>`,
/// inlineCode→`<code>` (content escaped), blockCode→`<pre><code>`,
/// quote→`<blockquote>`, center→`<div style="text-align: center;">`,
/// emojiCode→`\u{200B}:name:\u{200B}`, hashtag→`<a href="…/tags/…" rel="tag">`,
/// text nodes escaped through `escape` with newlines folded to `<br />`.
///
/// `url` is the upstream `config.url` (an origin, no trailing slash —
/// upstream normalizes it to `new URL(yamlUrl).origin`); hashtag hrefs hang
/// off it. `mentioned_remote_users` mirrors the upstream `mentionedRemoteUsers`
/// default `[]` — the mention arm consults it for href overrides.
pub fn to_html(
    nodes: &[Node],
    escape: &dyn Fn(&str) -> String,
    url: &str,
    mentioned_remote_users: &[MentionedRemoteUser],
) -> String {
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
                out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                out.push_str("</b>");
            }
            Node::Italic(children) => {
                out.push_str("<i>");
                out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                out.push_str("</i>");
            }
            Node::Strike(children) => {
                out.push_str("<del>");
                out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                out.push_str("</del>");
            }
            Node::Small(children) => {
                out.push_str("<small>");
                out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                out.push_str("</small>");
            }
            Node::InlineCode(code) => {
                out.push_str("<code>");
                out.push_str(&escape(code));
                out.push_str("</code>");
            }
            Node::BlockCode(code) => {
                // upstream: `<pre><code>${escapeHtml(node.props.code)}</code></pre>`
                out.push_str("<pre><code>");
                out.push_str(&escape(code));
                out.push_str("</code></pre>");
            }
            Node::Quote(children) => {
                out.push_str("<blockquote>");
                out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                out.push_str("</blockquote>");
            }
            Node::Center(children) => {
                // upstream: `<div style="text-align: center;">…</div>` (not
                // the legacy align attribute).
                out.push_str("<div style=\"text-align: center;\">");
                out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                out.push_str("</div>");
            }
            Node::EmojiCode(name) => {
                // upstream: `\u200B:${escapeHtml(node.props.name)}:\u200B` —
                // zero-width spaces, not an <img>; they keep the emoji code
                // from gluing to neighboring text in HTML consumers.
                out.push('\u{200B}');
                out.push(':');
                out.push_str(&escape(name));
                out.push(':');
                out.push('\u{200B}');
            }
            Node::Hashtag(tag) => {
                // upstream: `<a href="${escapeHtml(
                //   `${config.url}/tags/${encodeURIComponent(hashtag)}`)}" rel="tag">#…</a>`
                out.push_str("<a href=\"");
                out.push_str(&escape(&format!(
                    "{url}/tags/{}",
                    encode_uri_component(tag)
                )));
                out.push_str("\" rel=\"tag\">#");
                out.push_str(&escape(tag));
                out.push_str("</a>");
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
        to_html(&parse(input), &escape_html, "http://misskey.local", &[])
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

    // blockCode — the pinned upstream toHtml "escape" case
    // (test/unit/MfmService.ts): '```\n<p>Hello, world!</p>\n```' →
    // '<pre><code>&lt;p&gt;Hello, world!&lt;/p&gt;</code></pre>'.

    #[test]
    fn block_code_escapes_body_verbatim() {
        // the exact pinned upstream case, copied
        assert_eq!(
            render("```\n<p>Hello, world!</p>\n```"),
            "<pre><code>&lt;p&gt;Hello, world!&lt;/p&gt;</code></pre>"
        );
    }

    #[test]
    fn block_code_eats_adjacent_newlines() {
        // mfm.parse("a\n```\ncode\n```\nb") = [text "a", blockCode, text "b"]
        // — the block swallows one newline per side, so none survives.
        assert_eq!(
            render("a\n```\ncode\n```\nb"),
            "a<pre><code>code</code></pre>b"
        );
        assert_eq!(render("```js\nfoo()\n```"), "<pre><code>foo()</code></pre>");
    }

    #[test]
    fn block_code_needs_line_anchor_and_closer() {
        assert_eq!(render("```\nunterminated"), "```<br />unterminated");
        assert_eq!(render("x```\nc\n```"), "x```<br />c<br />```");
        // a fence with trailing text on its line is body, not a closer
        assert_eq!(
            render("```\na\n``` x\nb\n```"),
            "<pre><code>a\n``` x\nb</code></pre>"
        );
    }

    #[test]
    fn quote_folds_consecutive_lines() {
        // quote arm: `<blockquote>${toHtml(children)}</blockquote>`; the
        // stripped lines re-parse, so the inner newline rides the text arm
        // (`<br />`), and surrounding newlines are swallowed by the block.
        assert_eq!(render("> a\n> b"), "<blockquote>a<br />b</blockquote>");
        assert_eq!(render("> a\nb"), "<blockquote>a</blockquote>b");
        assert_eq!(
            render("x\n> a\n> b\ny"),
            "x<blockquote>a<br />b</blockquote>y"
        );
        assert_eq!(render("> a\n\nb"), "<blockquote>a</blockquote>b");
    }

    #[test]
    fn quote_nests_by_reparse() {
        // ">> a": the stripped line "> a" re-parses as a quote of its own.
        assert_eq!(
            render(">> a"),
            "<blockquote><blockquote>a</blockquote></blockquote>"
        );
    }

    #[test]
    fn quote_needs_content_and_anchor() {
        assert_eq!(render("> "), "&gt; ");
        assert_eq!(render(">a"), "<blockquote>a</blockquote>");
    }

    #[test]
    fn center_folds_whole_line() {
        // center arm: `<div style="text-align: center;">…</div>`; content is
        // re-parsed inline, and the close tag must sit at a line end.
        assert_eq!(
            render("<center>a</center>"),
            "<div style=\"text-align: center;\">a</div>"
        );
        assert_eq!(
            render("<center>**a**</center>"),
            "<div style=\"text-align: center;\"><b>a</b></div>"
        );
        assert_eq!(
            render("x\n<center>**a**</center>y"),
            "x<br />&lt;center&gt;<b>a</b>&lt;/center&gt;y"
        );
        assert_eq!(
            render("<center>a</center>\nz"),
            "<div style=\"text-align: center;\">a</div>z"
        );
    }

    #[test]
    fn emoji_code_wraps_in_zero_widths() {
        // emojiCode arm: `\u200B:${escapeHtml(name)}:\u200B`
        assert_eq!(render(":emoji:"), "\u{200B}:emoji:\u{200B}");
        assert_eq!(render(":a-b_c+d:"), "\u{200B}:a-b_c+d:\u{200B}");
        // only the right side guard is load-bearing (probed on mfm-js 0.27.0)
        assert_eq!(render(":emoji:x"), ":emoji:x");
        assert_eq!(render("x:emoji:y"), "x:emoji:y");
        assert_eq!(render("x:emoji:"), "x\u{200B}:emoji:\u{200B}");
        assert_eq!(render("::"), "::");
    }

    #[test]
    fn hashtag_links_to_config_tags() {
        // hashtag arm: `<a href="${config.url}/tags/…" rel="tag">#…</a>`
        assert_eq!(
            render("#foo"),
            "<a href=\"http://misskey.local/tags/foo\" rel=\"tag\">#foo</a>"
        );
        // close-set chars end the tag and stay text
        assert_eq!(
            render("#foo.bar"),
            "<a href=\"http://misskey.local/tags/foo\" rel=\"tag\">#foo</a>.bar"
        );
        assert_eq!(
            render("#foo bar"),
            "<a href=\"http://misskey.local/tags/foo\" rel=\"tag\">#foo</a> bar"
        );
        // bracket groups nest into the tag
        assert_eq!(
            render("#foo(bar)"),
            "<a href=\"http://misskey.local/tags/foo(bar)\" rel=\"tag\">#foo(bar)</a>"
        );
        assert_eq!(
            render("#foo(bar"),
            "<a href=\"http://misskey.local/tags/foo\" rel=\"tag\">#foo</a>(bar"
        );
        // percent-encoding rides encodeURIComponent (uppercase UTF-8 hex)
        assert_eq!(
            render("#日本語タグ"),
            "<a href=\"http://misskey.local/tags/%E6%97%A5%E6%9C%AC%E8%AA%9E%E3%82%BF%E3%82%B0\" rel=\"tag\">#日本語タグ</a>"
        );
    }

    #[test]
    fn hashtag_needs_anchor_and_non_digits() {
        assert_eq!(render("x#foo"), "x#foo"); // alnum-preceded '#'
        assert_eq!(
            render("##foo"),
            "#<a href=\"http://misskey.local/tags/foo\" rel=\"tag\">#foo</a>"
        );
        assert_eq!(render("#123"), "#123"); // all-digit run
    }
}
