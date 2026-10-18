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
