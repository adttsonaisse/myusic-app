/// Whether a plain lyric line is one of Genius's section markers — `[Chorus]`,
/// `[Verse 2: Artist]` — rather than words to sing.
pub fn is_genius_section_header(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('[')
        && trimmed.ends_with(']')
        && (3..=60).contains(&trimmed.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_genius_section_headers() {
        assert!(is_genius_section_header("[Chorus]"));
        assert!(is_genius_section_header("  [Verse 2: Artist]  "));
        assert!(is_genius_section_header("[A]"));
        assert!(is_genius_section_header("[후렴]"));
        assert!(!is_genius_section_header("[]"));
        assert!(!is_genius_section_header("Chorus"));
        assert!(!is_genius_section_header(&format!("[{}]", "a".repeat(59))));
    }
}
