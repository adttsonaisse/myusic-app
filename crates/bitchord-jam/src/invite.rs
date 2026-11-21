pub const ORIGIN: &str = "https://bitchord.kushagrasingh.in";
pub const CODE_LENGTH: usize = 6;

/// Parses and normalizes a party code from the canonical public invite URL shape only.
pub fn parse_invite_url(value: Option<&str>) -> Option<String> {
    let text = value?.trim();
    let prefix = format!("{ORIGIN}/invite/");
    if text.len() < prefix.len() || !text[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        return None;
    }
    let remainder = &text[prefix.len()..];
    let code = remainder
        .split('?')
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("");
    if code.chars().count() != CODE_LENGTH || !code.chars().all(char::is_alphanumeric) {
        return None;
    }
    Some(code.to_uppercase())
}

/// Builds the canonical public invite URL for `code`.
pub fn public_invite_url(code: &str) -> String {
    format!("{ORIGIN}/invite/{}", code.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_normalizes_invite_url() {
        assert_eq!(
            Some("ABC123".to_owned()),
            parse_invite_url(Some(&format!("{ORIGIN}/invite/ABC123")))
        );
        assert_eq!(
            Some("ABC123".to_owned()),
            parse_invite_url(Some(&format!("{ORIGIN}/invite/abc123")))
        );
    }

    #[test]
    fn ignores_query_and_fragment() {
        assert_eq!(
            Some("ABC123".to_owned()),
            parse_invite_url(Some(&format!("{ORIGIN}/invite/ABC123?utm=x")))
        );
        assert_eq!(
            Some("ABC123".to_owned()),
            parse_invite_url(Some(&format!("{ORIGIN}/invite/ABC123#top")))
        );
    }

    #[test]
    fn refuses_other_hosts_schemes_lengths_and_rubbish() {
        assert_eq!(
            None,
            parse_invite_url(Some("https://example.com/invite/ABC123"))
        );
        assert_eq!(
            None,
            parse_invite_url(Some("http://bitchord.kushagrasingh.in/invite/ABC123"))
        );
        assert_eq!(
            None,
            parse_invite_url(Some(&format!("{ORIGIN}/invite/ABC12")))
        );
        assert_eq!(
            None,
            parse_invite_url(Some(&format!("{ORIGIN}/invite/ABC1234")))
        );
        assert_eq!(
            None,
            parse_invite_url(Some(&format!("{ORIGIN}/invite/ABC123/")))
        );
        assert_eq!(None, parse_invite_url(None));
        assert_eq!(None, parse_invite_url(Some("")));
        assert_eq!(None, parse_invite_url(Some("ABC123")));
    }

    #[test]
    fn built_url_round_trips() {
        assert_eq!(
            Some("ZZ99QQ".to_owned()),
            parse_invite_url(Some(&public_invite_url("zz99qq")))
        );
    }
}
