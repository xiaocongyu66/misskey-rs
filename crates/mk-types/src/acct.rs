//! acct handling. Port of `misc/acct.ts`.
//!
//! Note: upstream uses JS `split('@', 2)`, which DISCARDS everything after
//! the second `@` (it does not keep it in the last element) — `a@b@c`
//! parses as username=a, host=b. Replicated below.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acct {
    pub username: String,
    pub host: Option<String>,
}

pub fn parse(acct: &str) -> Acct {
    let acct = acct.strip_prefix('@').unwrap_or(acct);
    let mut it = acct.splitn(3, '@');
    let username = it.next().unwrap_or("").to_string();
    let host = match it.next() {
        Some("") | None => None,
        Some(h) => Some(h.to_string()),
    };
    // Anything after the third segment is dropped, like JS split('@', 2).
    drop(it);
    Acct { username, host }
}

pub fn to_string(acct: &Acct) -> String {
    match &acct.host {
        None => acct.username.clone(),
        Some(host) => format!("{}@{}", acct.username, host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_and_remote() {
        assert_eq!(parse("alice").host, None);
        assert_eq!(parse("@alice").username, "alice");
        assert_eq!(
            parse("alice@example.com").host.as_deref(),
            Some("example.com")
        );
        assert_eq!(parse("@alice@example.com").username, "alice");
        assert_eq!(to_string(&parse("alice@example.com")), "alice@example.com");
        assert_eq!(to_string(&parse("alice")), "alice");
    }

    #[test]
    fn third_segment_dropped_like_js() {
        assert_eq!(parse("a@b@c"), parse("a@b"));
    }
}
