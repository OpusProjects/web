// For a value carried in a query string, such as the `next` page a sign-in
// returns to: every byte but the unreserved ones and '/' escaped, so a path with
// its own query ("?host=a&b") arrives whole.
pub fn query_escape(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// A path on THIS origin, safe to redirect to: `next=https://evil.example` (or
// `//evil.example`, which a browser reads as another host) would turn a
// sign-in into an open redirect that lends the service's address to a
// phishing link.
pub fn is_local_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_path_on_this_origin_is_local() {
        assert!(is_local_path("/sources?x=1"));
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/a\\b",
            "/a b",
            "/a\nb",
            "x",
        ] {
            assert!(!is_local_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_path_with_its_own_query_survives_as_one_value() {
        assert_eq!(query_escape("/sources/a-1_b.c~"), "/sources/a-1_b.c~");
        assert_eq!(query_escape("/x?host=a&b c"), "/x%3Fhost%3Da%26b%20c");
    }
}
