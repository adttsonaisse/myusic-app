/// Past this point in a track, pressing Previous restarts the track instead of skipping back.
pub const BACK_RESTARTS_AFTER_MS: u64 = 10_000;

/// Index where the upcoming AutoPlay section begins (after `current_index`),
/// or `from_autoplay.len()` if no upcoming AutoPlay tracks exist.
pub fn autoplay_section_start(from_autoplay: &[bool], current_index: isize) -> usize {
    let after = (current_index + 1).clamp(0, from_autoplay.len() as isize) as usize;
    (after..from_autoplay.len())
        .find(|&i| from_autoplay[i])
        .unwrap_or(from_autoplay.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(pattern: &str, current: isize) -> usize {
        let flags: Vec<bool> = pattern.chars().map(|c| c == '~').collect();
        autoplay_section_start(&flags, current)
    }

    #[test]
    fn replicates_queue_sections_test() {
        assert_eq!(3, start("...~~~", 0));
        assert_eq!(4, start("....", 1));
        assert_eq!(5, start("..~~~~", 4));
        assert_eq!(4, start("..~~", 3));
        assert_eq!(4, start("~~~.~~", 2));
        assert_eq!(0, start("", 0));
        assert_eq!(0, start("", -1));
    }
}
