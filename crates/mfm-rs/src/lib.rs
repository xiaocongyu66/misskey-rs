//! MFM (Misskey Markup Language) subset: the node types `MfmService.toHtml`
//! renders on the feed / AP planes — text, bold, italic, strike, small,
//! inline code, block code, quote, center, emoji code, hashtag, mention,
//! link, url, search. Hand scanner + visitor, pinned to the upstream
//! `packages/backend/src/core/MfmService.ts` handlers; the full mfm-js AST
//! port lands in M2 (needed by NoteCreateService).

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
    /// `@user` / `@user@host` → `<a href="…" class="u-url mention">@acct</a>`;
    /// the parse keeps the matched case (the render escapes the acct as
    /// typed) and the trimmed username/host.
    Mention {
        username: String,
        host: Option<String>,
        acct: String,
    },
    /// `[text](url)` / `?[text](url)` → `<a href="…">text</a>`. `silent` is
    /// parsed but the pinned backend arm never reads it (the frontend
    /// renderer does). The label parses with the inline rule set under
    /// mfm-js `linkLabel` (mention/hashtag/url never fold inside).
    Link {
        silent: bool,
        url: String,
        children: Vec<Node>,
    },
    /// Bare `https://…` or bracketed `<https://…>` → `<a href="…">url</a>`;
    /// the props keep the raw matched url (the render normalizes through
    /// `new URL`), `brackets` mirrors the mfm-js `urlAlt` prop.
    Url { url: String, brackets: bool },
    /// A `query search` / `query 検索` line → the Google-search link; the
    /// whole line (minus its trailing newline) rides as `content`.
    Search { query: String, content: String },
    /// `$[name children]` → the upstream `fn` arm: `unixtime` renders a
    /// `<time>`, the one-child `ruby`-named fn renders `<ruby>`, anything
    /// else falls to `fnDefault` → `<i>…</i>`.
    Fn { name: String, children: Vec<Node> },
    /// `\(...\)` inline formula → `<code>…</code>` (the mathInline arm is
    /// byte-identical to inlineCode's output shape).
    MathInline(String),
    /// `\[…\]` block formula → `<pre><code>…</code></pre>` (mathBlock ==
    /// blockCode's shape).
    MathBlock(String),
    /// `<plain>…</plain>` → `<span>…</span>`; the inner span is re-parsed
    /// with the inline rule set.
    Plain(Vec<Node>),
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
/// (block rules dropped — it is what quote/center content runs under),
/// `depth` mirrors the mfm-js `nestLimit` (20) so `>>>>…` spam cannot recurse
/// without bound, and `link_label` is the mfm-js `state.linkLabel` (set while
/// a `[label](url)` label is parsed — mention/hashtag/url never fold there).
#[derive(Clone, Copy)]
struct Ctx {
    inline_only: bool,
    depth: u32,
    link_label: bool,
}

/// Parse an MFM string. A marker only folds when its matching closer exists;
/// unterminated markup stays literal text.
pub fn parse(input: &str) -> Vec<Node> {
    parse_ctx(
        input,
        Ctx {
            inline_only: false,
            depth: 0,
            link_label: false,
        },
    )
}

/// The mfm-js `inline` alternation — block rules dropped (center/quote
/// content runs under it upstream).
fn parse_inline(input: &str, depth: u32, link_label: bool) -> Vec<Node> {
    parse_ctx(
        input,
        Ctx {
            inline_only: true,
            depth,
            link_label,
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

/// mfm-js `fn`: `$[` + name (up to a space) + (space + children) + `]`. The
/// children re-parse with the inline rule set (nesting rides the depth ctx);
/// an empty children span stays literal.
fn try_fn(input: &str, start: usize, ctx: Ctx) -> Option<(Node, usize)> {
    if ctx.link_label || !input[start..].starts_with("$[") {
        return None;
    }
    let rest = &input[start + 2..];
    let name_len = rest
        .find(|c: char| c == ' ' || c == ']')
        .filter(|&n| n > 0)?;
    if !name_len == 0 {
        return None;
    }
    let name = &rest[..name_len];
    let after_name = &rest[name_len..];
    if let Some(after_space) = after_name.strip_prefix(' ') {
        let end = after_space.find(']')?;
        if end == 0 {
            return None;
        }
        let children_src = &after_space[..end];
        let children = parse_inline(children_src, ctx.depth, false);
        let consumed = 2 + name_len + 1 + end + 1;
        Some((
            Node::Fn {
                name: name.to_string(),
                children,
            },
            start + consumed,
        ))
    } else {
        None
    }
}

/// mfm-js `mathInline`: `\(` + `[^\n]+?` + `\)` — verbatim body, single line.
fn try_math_inline(input: &str, start: usize) -> Option<(Node, usize)> {
    let rest = input[start..].strip_prefix("\\(")?;
    let end = rest.find("\\)")?;
    let body = &rest[..end];
    if body.is_empty() || body.contains('\n') {
        return None;
    }
    Some((Node::MathInline(body.to_string()), start + 2 + end + 2))
}

/// mfm-js `mathBlock`: `\[` at a line begin + verbatim multi-line body +
/// `\]` at a line end (newLine.option wraps tolerated on both sides).
fn try_math_block(input: &str, start: usize) -> Option<(Node, usize)> {
    let mut i = start;
    if let Some(n) = newline_len(input, i) {
        if at_line_begin(input, i + n) {
            i += n;
        }
    }
    let rest = input[i..].strip_prefix("\\[")?;
    let end = rest.find("\\]")?;
    let body = &rest[..end];
    if body.is_empty() {
        return None;
    }
    let after = end + 2;
    if !at_line_end(input, i + 2 + after) {
        return None;
    }
    let mut next = i + 2 + after;
    if let Some(n) = newline_len(input, next) {
        next += n;
    }
    Some((Node::MathBlock(body.to_string()), next))
}

/// mfm-js `plain`: `<plain>` + children + `</plain>` — the inner span
/// re-parses with the inline rule set (nestLimit rides the ctx depth).
fn try_plain(input: &str, start: usize, ctx: Ctx) -> Option<(Node, usize)> {
    let rest = input[start..].strip_prefix("<plain>")?;
    let end = rest.find("</plain>")?;
    let children_src = &rest[..end];
    if children_src.is_empty() {
        return None;
    }
    let children = parse_inline(children_src, ctx.depth, false);
    Some((Node::Plain(children), start + 7 + end + 8))
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
        if let Some(hit) = try_math_block(input, start) {
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
    if !ctx.link_label {
        if let Some(hit) = try_mention(input, start) {
            return Some(hit);
        }
        if let Some(hit) = try_hashtag(input, start) {
            return Some(hit);
        }
    }
    let rest = &input[start..];
    // `$[fn …]` — the mfm-js `fn` rule: name up to the space, children
    // re-parsed inline until the `]` closer (nesting rides the depth ctx).
    if let Some(hit) = try_fn(input, start, ctx) {
        return Some(hit);
    }
    // `\(...\)` inline math — verbatim body like inlineCode (mfm-js
    // mathInline `[^\n]+?`), no rescanning.
    if let Some(hit) = try_math_inline(input, start) {
        return Some(hit);
    }
    // `<plain>…</plain>` — children re-parsed with the inline rule set.
    if let Some(hit) = try_plain(input, start, ctx) {
        return Some(hit);
    }
    // inline code first: the span is verbatim, nothing inside is rescanned.
    // Guards mirror the mfm-js inlineCode regex `[^\n`]+?` with the `(?!`)
    // closer lookahead: no newline inside, and the closer's next char may not
    // be another backtick (otherwise ``` splits into code + text).
    if let Some(body) = rest.strip_prefix('`') {
        if let Some(end) = body.find('`') {
            if end > 0 && !body[..end].contains('\n') && !body[end + 1..].starts_with('`') {
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
        return try_link_family(input, start, ctx);
    };
    let body = &rest[open.len()..];
    let end = match body.find(close) {
        Some(end) => end,
        None => return try_link_family(input, start, ctx),
    };
    if end == 0 {
        // empty fold stays literal (mfm-js rejects empty spans)
        return try_link_family(input, start, ctx);
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
                link_label: ctx.link_label,
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
            link_label: ctx.link_label,
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
        ctx.link_label,
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

/// mfm-js mention: `'@' + [a-z0-9_.-]+ ('@' + [a-z0-9_.-]+)?` over
/// case-insensitive ASCII classes, never preceded by an ASCII alnum. Trailing
/// `.`/`-` runs trim off the host (an empty trim invalidates and drops the
/// host) and off the username when no host remains; a trimmed-empty or
/// `.`/`-`-leading username, a `.`/`-`-leading host, or a trailing run on the
/// username while a host is present invalidates the mention — the whole
/// matched span then rides as literal text.
fn try_mention(input: &str, start: usize) -> Option<(Node, usize)> {
    if !input[start..].starts_with('@') {
        return None;
    }
    if let Some(prev) = input[..start].chars().next_back() {
        if prev.is_ascii_alphanumeric() {
            return None;
        }
    }
    let mut j = start + 1;
    while let Some(&b) = input.as_bytes().get(j) {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-') {
            j += 1;
        } else {
            break;
        }
    }
    if j == start + 1 {
        return None;
    }
    let mut username = input[start + 1..j].to_string();
    let mut host: Option<String> = None;
    if input.as_bytes().get(j) == Some(&b'@') {
        let host_start = j + 1;
        let mut k = host_start;
        while let Some(&b) = input.as_bytes().get(k) {
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-') {
                k += 1;
            } else {
                break;
            }
        }
        if k > host_start {
            host = Some(input[host_start..k].to_string());
            j = k;
        }
    }
    let match_end = j;
    let mut invalid = false;
    // host trim: `/[.-]+$/` — an empty trim invalidates the mention and drops
    // the host, which re-arms the username trim below (upstream sets
    // modifiedHost = null there).
    let host = host.and_then(|h| {
        let t = h.trim_end_matches(['.', '-']);
        if t.is_empty() {
            invalid = true;
            None
        } else {
            Some(t.to_string())
        }
    });
    // username trim only while no host remains; a trailing run with a host
    // present invalidates the whole mention
    if host.is_none() {
        let trim = username.trim_end_matches(['.', '-']).len();
        if trim != username.len() {
            username.truncate(trim);
        }
    } else if username.ends_with(['.', '-']) {
        invalid = true;
    }
    if username.is_empty() || username.starts_with(['.', '-']) {
        invalid = true;
    }
    if host.as_ref().is_some_and(|h| h.starts_with(['.', '-'])) {
        invalid = true;
    }
    if invalid {
        // upstream returns the whole matched span as text
        return Some((Node::Text(input[start..match_end].to_string()), match_end));
    }
    let acct = match &host {
        Some(h) => format!("@{username}@{h}"),
        None => format!("@{username}"),
    };
    // upstream consumes only the acct (`success(index + acct.length, …)`), so
    // trailing trimmed chars stay behind and ride as text
    let consumed = start + acct.len();
    Some((
        Node::Mention {
            username,
            host,
            acct,
        },
        consumed,
    ))
}

/// The mfm-js tail of the `full` alternation — the `[`/`<`/`https://`/search
/// rules, which only ever match where the earlier inline openers cannot.
fn try_link_family(input: &str, start: usize, ctx: Ctx) -> Option<(Node, usize)> {
    if let Some(hit) = try_url_alt(input, start, ctx.link_label) {
        return Some(hit);
    }
    if let Some(hit) = try_link(input, start, ctx) {
        return Some(hit);
    }
    if let Some(hit) = try_url(input, start, ctx.link_label) {
        return Some(hit);
    }
    if !ctx.inline_only {
        return try_search(input, start);
    }
    None
}

/// mfm-js urlAlt: `'<' 'https://' (not('>' | space) char)+ '>'` — the scheme
/// check is case-SENSITIVE (the upstream `/https?:\/\//` carries no `i` flag),
/// newlines are allowed inside, and the props mark `brackets`.
fn try_url_alt(input: &str, start: usize, link_label: bool) -> Option<(Node, usize)> {
    if link_label || !input[start..].starts_with('<') {
        return None;
    }
    let after = url_scheme_len(&input[start + 1..])?;
    let mut j = start + 1 + after;
    loop {
        let Some(ch) = input[j..].chars().next() else {
            return None; // ran out before '>'
        };
        if ch == '>' {
            break;
        }
        if matches!(ch, ' ' | '\t' | '\u{3000}') {
            return None; // notMatch(space): the alt form admits no spaces
        }
        j += ch.len_utf8();
    }
    // many(1): at least one char between the scheme and '>'
    if j == start + 1 + after {
        return None;
    }
    Some((
        Node::Url {
            url: input[start + 1..j].to_string(),
            brackets: true,
        },
        j + 1,
    ))
}

/// The upstream url/urlAlt scheme — a case-sensitive `https?://`.
fn url_scheme_len(rest: &str) -> Option<usize> {
    if rest.starts_with("https://") {
        Some(8)
    } else if rest.starts_with("http://") {
        Some(7)
    } else {
        None
    }
}

/// mfm-js url: `notLinkLabel + https?:// + item+` — an item is a `(`/`[`
/// bracket group or a char from `[.,a-z0-9_/:%#@$&?!~=+-]` (case-insensitive).
/// A trailing `[.,]+` run trims off (riding as text); a fully-empty remainder
/// invalidates the whole match.
fn try_url(input: &str, start: usize, link_label: bool) -> Option<(Node, usize)> {
    if link_label {
        return None;
    }
    let scheme = url_scheme_len(&input[start..])?;
    let mut j = start + scheme;
    while let Some(next) = url_item(input, j, 0) {
        j = next;
    }
    let content = &input[start + scheme..j];
    let trimmed = content.trim_end_matches(['.', ',']);
    if trimmed.is_empty() {
        // upstream returns the whole matched span as text
        return Some((Node::Text(input[start..j].to_string()), j));
    }
    let end = start + scheme + trimmed.len();
    Some((
        Node::Url {
            url: input[start..end].to_string(),
            brackets: false,
        },
        end,
    ))
}

/// One url item; `None` ends the url run.
fn url_item(input: &str, i: usize, depth: u32) -> Option<usize> {
    if depth < 20 {
        for (open, close) in [("(", ")"), ("[", "]")] {
            if input[i..].starts_with(open) {
                let mut j = i + 1;
                loop {
                    if input[j..].starts_with(close) {
                        return Some(j + 1);
                    }
                    j = url_item(input, j, depth + 1)?;
                }
            }
        }
    }
    let ch = input[i..].chars().next()?;
    if ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '.' | ','
                | '_'
                | '/'
                | ':'
                | '%'
                | '#'
                | '@'
                | '$'
                | '&'
                | '?'
                | '!'
                | '~'
                | '='
                | '+'
                | '-'
        )
    {
        Some(i + 1)
    } else {
        None
    }
}

/// mfm-js link: `('[' | '?[') label ']' '(' url ')'` — the label parses with
/// the inline rule set under `linkLabel` and stops at an unclaimed `]` or a
/// newline; the target must be a url/urlAlt node (urlAlt first, like the
/// upstream alt order). The whole construct fails back to text when the
/// label is empty or the tail is missing. The upstream memoizes failed label
/// boundaries against backtracking; this port just rescans.
fn try_link(input: &str, start: usize, ctx: Ctx) -> Option<(Node, usize)> {
    if ctx.link_label {
        return None;
    }
    let rest = &input[start..];
    let (silent, label_start) = if rest.starts_with("?[") {
        (true, start + 2)
    } else if rest.starts_with('[') {
        (false, start + 1)
    } else {
        return None;
    };
    let mut children: Vec<Node> = Vec::new();
    let mut text = String::new();
    let mut items = 0usize;
    let mut j = label_start;
    let label_ctx = Ctx {
        inline_only: true,
        depth: ctx.depth + 1,
        link_label: true,
    };
    let close = loop {
        match input.as_bytes().get(j) {
            None | Some(b']') | Some(b'\r') | Some(b'\n') => break j,
            _ => {}
        }
        match try_open(input, j, label_ctx) {
            Some((node, next)) => {
                items += 1;
                if !text.is_empty() {
                    children.push(Node::Text(std::mem::take(&mut text)));
                }
                children.push(node);
                j = next;
            }
            None => {
                let ch = input[j..].chars().next().unwrap();
                text.push(ch);
                items += 1;
                j += ch.len_utf8();
            }
        }
    };
    if items == 0 || !input[close..].starts_with(']') {
        return None;
    }
    if !text.is_empty() {
        children.push(Node::Text(std::mem::take(&mut text)));
    }
    let after = close + 1;
    if !input[after..].starts_with('(') {
        return None;
    }
    let target = try_url_alt(input, after + 1, false).or_else(|| try_url(input, after + 1, false));
    let (Node::Url { url, .. }, url_end) = target? else {
        return None;
    };
    if !input[url_end..].starts_with(')') {
        return None;
    }
    Some((
        Node::Link {
            silent,
            url,
            children,
        },
        url_end + 1,
    ))
}

/// mfm-js search: `newLine.option + lineBegin + query-chars(≥1) + space +
/// button + lineEnd + newLine.option` — the button is `[search]`/`[検索]` or
/// `search`/`検索` (ASCII form case-insensitive), and a query char is
/// anything that is not a newline and not the start of `space + button +
/// lineEnd`. The node's `content` (the link text) is the whole line minus the
/// trailing newline; the leading newline folds into the node like the other
/// blocks.
fn try_search(input: &str, start: usize) -> Option<(Node, usize)> {
    let mut i = start;
    if let Some(n) = newline_len(input, i) {
        if at_line_begin(input, i + n) {
            i += n;
        }
    }
    if !at_line_begin(input, i) {
        return None;
    }
    let query_start = i;
    let mut j = i;
    loop {
        if newline_len(input, j).is_some() || j == input.len() {
            return None; // the line ended without a button
        }
        let s = space_len(input, j);
        if s > 0 {
            if let Some(bend) = search_button(input, j + s) {
                if at_line_end(input, bend) {
                    if j == query_start {
                        return None; // many(1): the query cannot be empty
                    }
                    let mut end = bend;
                    if let Some(n) = newline_len(input, end) {
                        end += n;
                    }
                    return Some((
                        Node::Search {
                            query: input[query_start..j].to_string(),
                            content: input[query_start..bend].to_string(),
                        },
                        end,
                    ));
                }
            }
        }
        j += input[j..].chars().next().unwrap().len_utf8();
    }
}

/// The search button — `[search]`/`[検索]`/`search`/`検索` (ASCII form
/// case-insensitive); yields the offset past it.
fn search_button(input: &str, i: usize) -> Option<usize> {
    const BUTTONS: [&str; 4] = ["[search]", "[検索]", "search", "検索"];
    for b in BUTTONS {
        if input[i..].len() >= b.len() && input[i..i + b.len()].eq_ignore_ascii_case(b) {
            return Some(i + b.len());
        }
    }
    None
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
            Node::Mention {
                username,
                host,
                acct,
            } => {
                // upstream mention arm: a `mentionedRemoteUsers` hit wins
                // (url, else uri) — matched username/host case-insensitively,
                // and a null mention host never matches (the upstream
                // `remoteUser.host?.toLowerCase() === host?.toLowerCase()`
                // compares a string against undefined); otherwise
                // `{config.url}/{acct}` with a self-host suffix strip that
                // can never fire upstream (the config.url is an origin —
                // scheme and all — while an acct host never carries one);
                // ported verbatim regardless. The href then rides
                // `new URL(...).href` normalization.
                let remote = mentioned_remote_users.iter().find(|r| {
                    r.username.to_lowercase() == username.to_lowercase()
                        && match host.as_deref() {
                            Some(h) => r.host.to_lowercase() == h.to_lowercase(),
                            None => false,
                        }
                });
                let href = match remote {
                    Some(r) => r.url.clone().unwrap_or_else(|| r.uri.clone()),
                    None => {
                        let suffix = format!("@{url}");
                        let tail = if acct.ends_with(&suffix) {
                            acct[..acct.len() - suffix.len()].to_string()
                        } else {
                            acct.clone()
                        };
                        format!("{url}/{tail}")
                    }
                };
                match url::Url::parse(&href) {
                    Ok(u) => {
                        out.push_str("<a href=\"");
                        out.push_str(&escape(u.as_str()));
                        out.push_str("\" class=\"u-url mention\">");
                        out.push_str(&escape(acct));
                        out.push_str("</a>");
                    }
                    Err(_) => out.push_str(&escape(acct)),
                }
            }
            Node::Link {
                silent: _,
                url: target,
                children,
            } => {
                // upstream link arm: `new URL(props.url)` →
                // `<a href="${escapeHtml(url.href)}">${children}</a>`; an
                // unparseable url falls back to literal `[text](url)`. The
                // `silent` prop rides the parse only — this arm never reads
                // it (the frontend renderer does).
                match url::Url::parse(target) {
                    Ok(u) => {
                        out.push_str("<a href=\"");
                        out.push_str(&escape(u.as_str()));
                        out.push_str("\">");
                        out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                        out.push_str("</a>");
                    }
                    Err(_) => {
                        out.push('[');
                        out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                        out.push_str("](");
                        out.push_str(&escape(target));
                        out.push(')');
                    }
                }
            }
            Node::Url { url, brackets: _ } => {
                // upstream url arm: `<a href="${escapeHtml(url.href)}">${
                //   escapeHtml(node.props.url)}</a>` — the TEXT keeps the raw
                // matched url, the href rides normalization.
                match url::Url::parse(url) {
                    Ok(u) => {
                        out.push_str("<a href=\"");
                        out.push_str(&escape(u.as_str()));
                        out.push_str("\">");
                        out.push_str(&escape(url));
                        out.push_str("</a>");
                    }
                    Err(_) => out.push_str(&escape(url)),
                }
            }
            Node::Search { query, content } => {
                // upstream search arm: a google-search link; `content` (the
                // raw line) is the link text.
                out.push_str("<a href=\"");
                out.push_str(&escape(&format!(
                    "https://www.google.com/search?q={}",
                    encode_uri_component(query)
                )));
                out.push_str("\">");
                out.push_str(&escape(content));
                out.push_str("</a>");
            }
            Node::Fn { name, children } => {
                // upstream fn arm: `unixtime` → a `<time>` from the first
                // child's text; a single-child `ruby` fn → `<ruby>` with the
                // space-split text halves; anything else (and every error
                // path) rides `fnDefault` → `<i>${children}</i>`.
                match name.as_str() {
                    "unixtime" => {
                        let text = match children.first() {
                            Some(Node::Text(t)) => t.clone(),
                            _ => String::new(),
                        };
                        match text.parse::<i64>() {
                            Ok(secs) => {
                                let iso = unixtime_iso(secs);
                                if let Some(iso) = iso {
                                    out.push_str("<time datetime=\"");
                                    out.push_str(&escape(&iso));
                                    out.push_str("\">");
                                    out.push_str(&escape(&iso));
                                    out.push_str("</time>");
                                    continue;
                                }
                                out.push_str("<i>");
                                let inner = to_html(children, escape, url, mentioned_remote_users);
                                out.push_str(&inner);
                                out.push_str("</i>");
                            }
                            Err(_) => {
                                out.push_str("<i>");
                                out.push_str(&to_html(
                                    children,
                                    escape,
                                    url,
                                    mentioned_remote_users,
                                ));
                                out.push_str("</i>");
                            }
                        }
                    }
                    "ruby" => {
                        if children.len() == 1 {
                            let text = match &children[0] {
                                Node::Text(t) => t.clone(),
                                _ => String::new(),
                            };
                            let mut halves = text.splitn(2, ' ');
                            let base = halves.next().unwrap_or("");
                            let rt = halves.next().unwrap_or("");
                            out.push_str("<ruby>");
                            out.push_str(&escape(base));
                            out.push_str("<rp>(</rp><rt>");
                            out.push_str(&escape(rt));
                            out.push_str("</rt><rp>)</rp></ruby>");
                        } else {
                            let Some(Node::Text(rt)) = children.last() else {
                                out.push_str("<i>");
                                out.push_str(&to_html(
                                    children,
                                    escape,
                                    url,
                                    mentioned_remote_users,
                                ));
                                out.push_str("</i>");
                                continue;
                            };
                            out.push_str("<ruby>");
                            out.push_str(&to_html(
                                &children[..children.len() - 1],
                                escape,
                                url,
                                mentioned_remote_users,
                            ));
                            out.push_str("<rp>(</rp><rt>");
                            out.push_str(&escape(rt.trim()));
                            out.push_str("</rt><rp>)</rp></ruby>");
                        }
                    }
                    _ => {
                        out.push_str("<i>");
                        out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                        out.push_str("</i>");
                    }
                }
            }
            Node::MathInline(formula) => {
                // mathInline == inlineCode's output shape.
                out.push_str("<code>");
                out.push_str(&escape(formula));
                out.push_str("</code>");
            }
            Node::MathBlock(formula) => {
                // mathBlock == blockCode's output shape.
                out.push_str("<pre><code>");
                out.push_str(&escape(formula));
                out.push_str("</code></pre>");
            }
            Node::Plain(children) => {
                out.push_str("<span>");
                out.push_str(&to_html(children, escape, url, mentioned_remote_users));
                out.push_str("</span>");
            }
        }
    }
    out
}

/// The `unixtime` fn arm's `new Date(parseInt(text, 10) * 1000).toISOString()`
/// (civil-from-days, no external time crate; ms always `.000` like upstream).
fn unixtime_iso(secs: i64) -> Option<String> {
    let ms = secs.checked_mul(1000)?;
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let secs_of_day = secs.rem_euclid(86_400);
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
        millis
    ))
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

    #[test]
    fn mention_links_to_config_acct() {
        // mention arm fallback: `<a href="{config.url}/{acct}"
        // class="u-url mention">{acct}</a>`
        assert_eq!(
            render("@user"),
            "<a href=\"http://misskey.local/@user\" class=\"u-url mention\">@user</a>"
        );
        assert_eq!(
            render("@user@host.example"),
            "<a href=\"http://misskey.local/@user@host.example\" class=\"u-url mention\">@user@host.example</a>"
        );
        // trailing trim: the dot stays text
        assert_eq!(
            render("@user@host."),
            "<a href=\"http://misskey.local/@user@host\" class=\"u-url mention\">@user@host</a>."
        );
        // a host-ful username trailing run invalidates the whole span
        assert_eq!(render("@user.@host"), "@user.@host");
        assert_eq!(render("@user@."), "@user@.");
        assert_eq!(render("@.user"), "@.user");
        // hostless username trailing run trims into the acct
        assert_eq!(
            render("@user-"),
            "<a href=\"http://misskey.local/@user\" class=\"u-url mention\">@user</a>-"
        );
        // mid-name dots are fine; alnum-preceded '@' stays literal
        assert_eq!(
            render("@u..ser"),
            "<a href=\"http://misskey.local/@u..ser\" class=\"u-url mention\">@u..ser</a>"
        );
        assert_eq!(render("a@b"), "a@b");
    }

    #[test]
    fn mention_remote_lookup_overrides_href() {
        // upstream: a mentionedRemoteUsers hit renders its url (else uri) —
        // matched case-insensitively on username+host
        let mentioned = vec![
            MentionedRemoteUser {
                username: "remoteuser".into(),
                host: "remote.example".into(),
                url: Some("https://remote.example/@remoteuser".into()),
                uri: "https://remote.example/users/9x".into(),
            },
            MentionedRemoteUser {
                username: "nourl".into(),
                host: "remote.example".into(),
                url: None,
                uri: "https://remote.example/users/9y".into(),
            },
        ];
        let html = |input: &str| {
            to_html(
                &parse(input),
                &escape_html,
                "http://misskey.local",
                &mentioned,
            )
        };
        assert_eq!(
            html("@REMOTEUSER@remote.example"),
            "<a href=\"https://remote.example/@remoteuser\" class=\"u-url mention\">@REMOTEUSER@remote.example</a>"
        );
        assert_eq!(
            html("@nourl@remote.example"),
            "<a href=\"https://remote.example/users/9y\" class=\"u-url mention\">@nourl@remote.example</a>"
        );
        // a local (host-less) mention never matches a remote row
        assert_eq!(
            html("@remoteuser"),
            "<a href=\"http://misskey.local/@remoteuser\" class=\"u-url mention\">@remoteuser</a>"
        );
    }

    #[test]
    fn link_folds_label_and_normalizes_url() {
        // link arm: `<a href="${escapeHtml(new URL(url).href)}">${children}</a>`
        // — new URL adds the WHATWG root slash
        assert_eq!(
            render("[foo](https://example.com)"),
            "<a href=\"https://example.com/\">foo</a>"
        );
        // the label parses inline (bold folds; silent ?[ renders the same)
        assert_eq!(
            render("[**b**](https://example.com)"),
            "<a href=\"https://example.com/\"><b>b</b></a>"
        );
        assert_eq!(
            render("?[foo](https://example.com)"),
            "<a href=\"https://example.com/\">foo</a>"
        );
        // the target must be an http(s) url — anything else stays literal
        assert_eq!(render("[foo](bar)"), "[foo](bar)");
        assert_eq!(
            render("[foo](javascript:alert(1))"),
            "[foo](javascript:alert(1))"
        );
        // an empty label or missing tail falls back — the url then folds bare
        assert_eq!(
            render("[](https://example.com)"),
            "[](<a href=\"https://example.com/\">https://example.com</a>)"
        );
        assert_eq!(
            render("[a](https://x"),
            "[a](<a href=\"https://x/\">https://x</a>"
        );
        // a trailing-dot target trims out of the url node and breaks the link
        assert_eq!(
            render("[a](https://example.com.)"),
            "[a](<a href=\"https://example.com/\">https://example.com</a>.)"
        );
    }

    #[test]
    fn url_folds_bare_and_bracketed() {
        // url arm: `<a href="${new URL(url).href}">${escapeHtml(url)}</a>` —
        // the TEXT keeps the raw match, the HREF is normalized
        assert_eq!(
            render("https://example.com"),
            "<a href=\"https://example.com/\">https://example.com</a>"
        );
        // trailing [.,]+ rides back as text
        assert_eq!(
            render("https://example.com."),
            "<a href=\"https://example.com/\">https://example.com</a>."
        );
        // bracket groups nest into the url; the scheme check is case-sensitive
        assert_eq!(
            render("https://example.com/a(b)"),
            "<a href=\"https://example.com/a(b)\">https://example.com/a(b)</a>"
        );
        assert_eq!(
            render("<https://example.com/x>"),
            "<a href=\"https://example.com/x\">https://example.com/x</a>"
        );
        assert_eq!(render("HTTPS://X"), "HTTPS://X");
        assert_eq!(render("https://."), "https://.");
    }

    #[test]
    fn search_line_folds_to_google_link() {
        // search arm: `<a href="https://www.google.com/search?q={query}">${
        //   content}</a>` — the whole line is the text
        assert_eq!(
            render("foo search"),
            "<a href=\"https://www.google.com/search?q=foo\">foo search</a>"
        );
        assert_eq!(
            render("foo [search]"),
            "<a href=\"https://www.google.com/search?q=foo\">foo [search]</a>"
        );
        // earlier spaces ride the query, the last one is the separator
        assert_eq!(
            render("foo  search"),
            "<a href=\"https://www.google.com/search?q=foo%20\">foo  search</a>"
        );
        // no button / mid-line button stays literal
        assert_eq!(render("search"), "search");
        assert_eq!(render("foo search bar"), "foo search bar");
        // the line's surrounding newlines fold into the node
        assert_eq!(
            render("a\nfoo search\nbar"),
            "a<a href=\"https://www.google.com/search?q=foo\">foo search</a>bar"
        );
        // search rides the full parser inside quotes
        assert_eq!(
            render("> foo search"),
            "<blockquote><a href=\"https://www.google.com/search?q=foo\">foo search</a></blockquote>"
        );
    }

    #[test]
    fn fn_arm_unixtime_ruby_and_default() {
        // fnDefault: unknown names render `<i>${children}</i>`
        assert_eq!(render("$[tada abc]"), "<i>abc</i>");
        // fn children re-parse: bold inside the fold
        assert_eq!(render("$[sparkle **x**]"), "<i><b>x</b></i>");
        // unixtime: `<time datetime="${iso}">${iso}</time>` from the first
        // child's text (0 → 1970-01-01T00:00:00.000Z)
        assert_eq!(
            render("$[unixtime 0]"),
            "<time datetime=\"1970-01-01T00:00:00.000Z\">1970-01-01T00:00:00.000Z</time>"
        );
        // unixtime parse failure rides fnDefault
        assert_eq!(render("$[unixtime NaN]"), "<i>NaN</i>");
        // single-text-child ruby: space-split base/rt halves
        assert_eq!(
            render("$[ruby 漢字 かんじ]"),
            "<ruby>漢字<rp>(</rp><rt>かんじ</rt><rp>)</rp></ruby>"
        );
        // no space in the single child → empty rt
        assert_eq!(
            render("$[ruby 漢字]"),
            "<ruby>漢字<rp>(</rp><rt></rt><rp>)</rp></ruby>"
        );
        // unterminated $[ stays literal
        assert_eq!(render("$[tada abc"), "$[tada abc");
    }

    #[test]
    fn math_inline_and_block_shapes() {
        // mathInline == inlineCode's shape: `<code>${escape(formula)}</code>`
        assert_eq!(render("\\(a<b\\)"), "<code>a&lt;b</code>");
        // empty or newline-carrying body stays literal
        assert_eq!(render("\\(\\)"), "\\(\\)");
        assert_eq!(render("\\(a\nb\\)"), "\\(a<br />b\\)");
        // mathBlock == blockCode's shape, anchored to lines
        assert_eq!(render("\\[x+y\\]"), "<pre><code>x+y</code></pre>");
        // empty block stays literal
        assert_eq!(render("\\[\\]"), "\\[\\]");
        // surrounding newlines fold into the node
        assert_eq!(render("a\n\\[q\\]\nb"), "a<pre><code>q</code></pre>b");
    }

    #[test]
    fn plain_wraps_in_span() {
        // plain arm: `<span>${children}</span>` — children re-parse inline
        assert_eq!(render("<plain>**x**</plain>"), "<span><b>x</b></span>");
        // plain suppresses nothing visible in the pinned arm, but block
        // rules do not apply inside (quote stays literal text)
        assert_eq!(render("<plain>> q</plain>"), "<span>&gt; q</span>");
        // empty plain stays literal
        assert_eq!(render("<plain></plain>"), "&lt;plain&gt;&lt;/plain&gt;");
        // unterminated stays literal
        assert_eq!(render("<plain>x"), "&lt;plain&gt;x");
    }
}
