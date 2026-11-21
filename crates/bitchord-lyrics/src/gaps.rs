use crate::line::LyricLine;

/// Shorter instrumental breaks aren't worth interrupting the line for.
pub const MIN_GAP_MS: i64 = 4_000;

/// Marks instrumental stretches with blank gap lines (`text == ""`), matching
/// how LRC files mark breaks with bare timestamps.
///
/// A break is only inserted after a line that carries an explicit end
/// ([`LyricLine::has_known_end`]), and only when `end_ms > time_ms` so the gap
/// marker never collides with the line's own timestamp.
pub fn with_instrumental_gaps(lines: Vec<LyricLine>) -> Vec<LyricLine> {
    let Some(first) = lines.first() else {
        return lines;
    };
    let mut out = Vec::with_capacity(lines.len() + 4);
    // Nothing stands for the intro, so give the run-up its own break.
    if first.time_ms >= MIN_GAP_MS {
        out.push(LyricLine::new(0, ""));
    }
    let mut iter = lines.into_iter().peekable();
    while let Some(line) = iter.next() {
        let gap_at = iter.peek().and_then(|next| {
            if !line.has_known_end() {
                return None;
            }
            let end_ms = line.end_ms();
            let silence = next.time_ms - end_ms;
            // A marker sharing its line's stamp could never be reached: the
            // cursor takes the last line whose stamp has passed.
            (silence >= MIN_GAP_MS && end_ms > line.time_ms).then_some(end_ms)
        });
        out.push(line);
        if let Some(at) = gap_at {
            out.push(LyricLine::new(at, ""));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn line_synced(rows: &[(i64, Option<i64>, &str)]) -> Vec<LyricLine> {
        let lines = rows
            .iter()
            .map(|&(time, duration, text)| LyricLine {
                time_ms: time,
                text: text.to_owned(),
                sung_until_ms: duration.filter(|&d| d > 0).map(|d| time + d),
                ..LyricLine::default()
            })
            .collect();
        with_instrumental_gaps(lines)
    }

    #[test]
    fn slowly_sung_line_synced_song_gets_no_break_between_lines() {
        let lines = line_synced(&[
            (19_740, Some(10_020), "first slow line"),
            (29_760, Some(9_910), "second slow line"),
            (39_670, Some(10_000), "third slow line"),
        ]);
        assert_eq!(1, lines.iter().filter(|l| l.is_gap()).count());
        assert!(lines[0].is_gap());
        assert_eq!(3, lines.iter().filter(|l| !l.is_gap()).count());
    }

    #[test]
    fn no_break_ever_shares_a_stamp_with_the_line_it_follows() {
        let lines = line_synced(&[
            (19_740, Some(10_020), "one"),
            (29_760, Some(9_910), "two"),
            (39_670, Some(10_000), "three"),
        ]);
        let sung_stamps: HashSet<i64> = lines
            .iter()
            .filter(|l| !l.is_gap())
            .map(|l| l.time_ms)
            .collect();
        let clashes: Vec<&LyricLine> = lines
            .iter()
            .filter(|l| l.is_gap() && sung_stamps.contains(&l.time_ms))
            .collect();
        assert!(clashes.is_empty());
    }

    #[test]
    fn line_synced_source_with_no_durations_gets_no_synthesised_breaks() {
        let lines = line_synced(&[(1_000, None, "one"), (20_000, None, "two")]);
        assert_eq!(0, lines.iter().filter(|l| l.is_gap()).count());
    }

    #[test]
    fn stated_line_end_short_of_next_stamp_is_marked_as_a_break() {
        let lines = line_synced(&[
            (1_000, Some(2_000), "before the solo"),
            (30_000, Some(2_000), "after the solo"),
        ]);
        let gaps: Vec<&LyricLine> = lines
            .iter()
            .filter(|l| l.is_gap() && l.time_ms > 0)
            .collect();
        assert_eq!(1, gaps.len());
        assert_eq!(3_000, gaps[0].time_ms);
    }
}
