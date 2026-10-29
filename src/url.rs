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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_its_own_query_survives_as_one_value() {
        assert_eq!(query_escape("/sources/a-1_b.c~"), "/sources/a-1_b.c~");
        assert_eq!(query_escape("/x?host=a&b c"), "/x%3Fhost%3Da%26b%20c");
    }
}
