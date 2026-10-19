//! Small shared text utilities (mirrors `misc/` helpers as needed).

/// Lazy hashtag extraction for the M2 note creator. Upstream parses via MFM;
/// a whitespace/punctuation-bounded `#tag` scanner reproduces the e2e
/// fixtures. Returned normalized (lowercase), de-duplicated. `#` in the
/// middle of a word (e.g. `plane#x`) is not a tag.
pub fn extract_hashtags(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let n = bytes.len();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    let mut k = 0usize;
    while k < n {
        if bytes[k] == b'#' && (k == 0 || !is_word(bytes[k - 1])) {
            let start = k + 1;
            let mut end = start;
            while end < n && is_word(bytes[end]) {
                end += 1;
            }
            if end > start {
                let tag = text[start..end].to_lowercase();
                if !out.contains(&tag) {
                    out.push(tag);
                }
                k = end;
                continue;
            }
        }
        k += 1;
    }
    out
}

/// (`username`, optional `@host`) mention extraction. LOOSE port of the MFM
/// mention tokenizer used by `misc/extract-mentions.ts` (upstream parses via
/// `mfm-js` nodes and resolves through `RemoteUserResolveService`): a mention
/// is `@` + `[a-zA-Z0-9_]+` + optional `@[host]`, not preceded by a word char
/// or another `@` (so `me@example.com` is not a mention). Match strings
/// de-duplicated in order. Mention limits (role policy `mentionLimit`) are the
/// caller's check upstream (`NoteCreateService.create`).
pub fn extract_mentions(text: &str) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let bytes = text.as_bytes();
    let n = bytes.len();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let is_host = |b: u8| is_word(b) || b == b'.' || b == b'-';
    let mut k = 0usize;
    while k < n {
        if bytes[k] == b'@' && (k == 0 || (!is_word(bytes[k - 1]) && bytes[k - 1] != b'@')) {
            let us = k + 1;
            let mut ue = us;
            while ue < n && is_word(bytes[ue]) {
                ue += 1;
            }
            if ue > us {
                // optional host part (`@alice@remote.example`); `@.` local hint
                let mut host: Option<String> = None;
                let mut after = ue;
                if after < n && bytes[after] == b'@' && after + 1 < n && is_host(bytes[after + 1]) {
                    let hs = after + 1;
                    let mut he = hs;
                    while he < n && is_host(bytes[he]) {
                        he += 1;
                    }
                    if he > hs {
                        let h = text[hs..he].to_string();
                        if h != "." {
                            host = Some(h);
                        }
                        after = he;
                    }
                }
                let username = text[us..ue].to_string();
                if !out.contains(&(username.clone(), host.clone())) {
                    out.push((username, host));
                }
                k = after;
                continue;
            }
        }
        k += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{extract_hashtags, extract_mentions};

    #[test]
    fn extracts_basic_and_dedups() {
        assert_eq!(extract_hashtags("hello #Monday world"), vec!["monday"]);
        assert_eq!(
            extract_hashtags("#a #A tail #b_c, #done."),
            vec!["a", "b_c", "done"]
        );
        assert!(extract_hashtags("no tags plane#x").is_empty());
        assert!(extract_hashtags("#").is_empty());
        assert!(extract_hashtags("").is_empty());
    }

    #[test]
    fn mentions_basic_forms() {
        // bare mention, punctuation-trailing, mid-text
        assert_eq!(
            extract_mentions("hello @alice"),
            vec![("alice".to_string(), None)]
        );
        assert_eq!(
            extract_mentions("hi @bob! and @carol, @dave."),
            vec![
                ("bob".to_string(), None),
                ("carol".to_string(), None),
                ("dave".to_string(), None),
            ]
        );
        // dedupe
        assert_eq!(
            extract_mentions("@alice @alice"),
            vec![("alice".to_string(), None)]
        );
    }

    #[test]
    fn mention_boundaries_and_host() {
        // `email@host` is not a mention (word char right before @)
        assert!(extract_mentions("me@example.com").is_empty());
        // host variants stay attached to the mention
        assert_eq!(
            extract_mentions("@alice@remote.example"),
            vec![("alice".to_string(), Some("remote.example".to_string()))]
        );
        // local-hint host form keeps username only when host is our own
        assert_eq!(
            extract_mentions("@alice@local.host"),
            vec![("alice".to_string(), Some("local.host".to_string()))]
        );
    }
}

/// `UtilityService.concatNoteContentsForKeyWordCheck`: cw/text join WITH no
/// separator (contents may continue), then newline-separated poll choices.
pub fn concat_note_contents_for_keyword_check(
    cw: Option<&str>,
    text: Option<&str>,
    poll_choices: &[String],
) -> String {
    let mut s = String::new();
    if let Some(cw) = cw {
        s.push_str(cw);
    }
    if let Some(text) = text {
        s.push_str(text);
    }
    s.push('\n');
    s.push_str(&poll_choices.join("\n"));
    s.push('\n');
    s
}

/// `UtilityService.isKeyWordIncluded`: plain keywords are substring tests
/// (space = AND of every word); `/pattern/flags` entries are regexps
/// (upstream RE2; flags i/m/s/x ported onto rust regex inline flags).
pub fn is_keyword_included(text: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() || text.is_empty() {
        return false;
    }
    let regexp_regexp = regex::Regex::new(r"^/(.+)/(.*)$").unwrap();
    keywords
        .iter()
        .any(|filter| match regexp_regexp.captures(filter) {
            Some(caps) => {
                let body = &caps[1];
                let flags = &caps[2];
                let mut inline = String::from("(?");
                if flags.contains('i') {
                    inline.push('i');
                }
                if flags.contains('m') {
                    inline.push('m');
                }
                if flags.contains('s') {
                    inline.push('s');
                }
                if flags.contains('x') {
                    inline.push('x');
                }
                inline.push(')');
                let pattern = if inline == "(?)" {
                    body.to_string()
                } else {
                    format!("{inline}{body}")
                };
                regex::Regex::new(&pattern)
                    .map(|re| re.is_match(text))
                    .unwrap_or(false)
            }
            None => filter.split(' ').all(|kw| text.contains(kw)),
        })
}

#[cfg(test)]
mod keyword_tests {
    use super::*;

    #[test]
    fn keyword_modes_match_upstream_fixtures() {
        let words = vec!["test".to_string()];
        assert!(is_keyword_included("hogetesthuge", &words));
        assert!(!is_keyword_included("hoge", &words));

        let re = vec!["/Test/i".to_string()];
        assert!(is_keyword_included("hogetesthuge", &re));

        let and = vec!["Test hoge".to_string()];
        assert!(is_keyword_included("hogeTesthuge", &and));
    }
}

// unit-ap: upstream `test/unit/extract-mentions.ts` pinned beside the
// equivalent impl (`extract_mentions` above; upstream
// `src/misc/extract-mentions.ts`). Upstream walks the parsed mfm tree
// (`mfm.extract(node => node.type === 'mention')`) and returns every
// mention node's props `{username, acct, host}` with NO dedupe (removal is
// an explicit upstream TODO); the interim text-level scanner dedupes as the
// notes_create contract (pinned by `mentions_basic_forms` above) — the two
// upstream cases carry no repeated mention, so the pins hold either way.
// Parity ledger for the rest of the upstream test/unit AP/MFM set — all
// SKIPs until their surfaces exist:
//   ap-request.ts   -> pins ApRequestCreator.createSignedPost/Get (rsa-sha256
//                      signing string: `(request-target)`, digest, host) +
//                      check-against-url.ts assertActivityMatchesUrl; there
//                      is no HTTP-Signature generator anywhere in-tree (mk-ap
//                      is an empty stub) and signing is federation-active
//                      surface — skipped per lane policy.
//   ApMfmService.ts -> pins getNoteHtml (hashtag/mention/emoji-in-tree →
//                      ap-tag html + `_misskey_content` flag); mfm-rs is an
//                      empty stub, no AST to walk — skipped.
//   MfmService.ts   -> pins toHtml (br/escape/no-span) + fromHtml (p/pre/
//                      code/blockquote/link/ruby/mention/hashtag); no html
//                      renderer exists (fetch_resource's `mfm_lite_html` is
//                      an escape-only feed helper, not the contract) —
//                      skipped.
//   activitypub.ts  -> pins ApPerson/ApNote/ApImage/ApRenderer/JsonLd
//                      services through a MockResolver + DB (minimum object
//                      parse, name truncate, alsoKnownAs, collection
//                      visibility, featured fetch trials, renderDocument
//                      dimensions, JSON-LD compaction); none of those
//                      services exist (mk-ap stub) — skipped.
mod unit_ap {
    use super::extract_mentions;

    #[test]
    fn upstream_simple() {
        // 'simple': extractMentions(parse('@foo @bar @baz'))
        assert_eq!(
            extract_mentions("@foo @bar @baz"),
            vec![
                ("foo".to_string(), None),
                ("bar".to_string(), None),
                ("baz".to_string(), None),
            ]
        );
    }

    #[test]
    fn upstream_nested() {
        // 'nested': extractMentions(parse('@foo **@bar** @baz')) — the bold
        // child node's mention must survive the walk
        assert_eq!(
            extract_mentions("@foo **@bar** @baz"),
            vec![
                ("foo".to_string(), None),
                ("bar".to_string(), None),
                ("baz".to_string(), None),
            ]
        );
    }
}
