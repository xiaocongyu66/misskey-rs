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

#[cfg(test)]
mod tests {
    use super::extract_hashtags;

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
}
