//! Chapter clocks and derived ends. Port of `features/chapters/timing.ts`.

use crate::model::Chapter;

/// `HH:MM:SS.mmm` for a millisecond value (rounded, clamped at zero).
pub fn chapter_time(ms: f64) -> String {
    let v = (ms + 0.5).floor().max(0.0) as i64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        v / 3_600_000,
        (v / 60_000) % 60,
        (v / 1000) % 60,
        v % 1000
    )
}

/// Strict parser: exactly `HH:MM:SS.mmm` with 2-4 hour digits, minutes/seconds below 60 and a
/// total not beyond 365 days. Returns `None` for anything ambiguous or overflowing.
pub fn parse_chapter_time(text: &str) -> Option<i64> {
    if !text
        .chars()
        .all(|c| c == ':' || c == '.' || c.is_ascii_digit())
    {
        return None;
    }
    let (hours, rest) = text.split_once(':')?;
    let (minutes, rest) = rest.split_once(':')?;
    let (seconds, millis) = rest.split_once('.')?;
    if !(2..=4).contains(&hours.len())
        || minutes.len() != 2
        || seconds.len() != 2
        || millis.len() != 3
    {
        return None;
    }
    let (h, m, s, ms): (i64, i64, i64, i64) = (
        hours.parse().ok()?,
        minutes.parse().ok()?,
        seconds.parse().ok()?,
        millis.parse().ok()?,
    );
    if m > 59 || s > 59 {
        return None;
    }
    let value = h * 3_600_000 + m * 60_000 + s * 1000 + ms;
    (value <= 31_536_000_000).then_some(value)
}

/// Each chapter ends at the next distinct start, or at `duration` (or start+1 s) for the last.
pub fn chapter_ends(chapters: &mut [Chapter], duration: Option<f64>) {
    let mut starts: Vec<i64> = chapters.iter().map(|c| c.start_ms).collect();
    starts.sort_unstable();
    starts.dedup();
    let end_of = |start: i64| -> i64 {
        match starts.iter().position(|s| *s == start) {
            Some(i) if i + 1 < starts.len() => starts[i + 1],
            _ => {
                let fallback = duration.filter(|d| *d != 0.0).map(|d| d as i64);
                (start + 1).max(fallback.unwrap_or(start + 1000))
            }
        }
    };
    for c in chapters.iter_mut() {
        c.end_ms = end_of(c.start_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_round_trip_exactly() {
        for ms in [0, 1, 999, 61_250, 3_723_004, 31_536_000_000] {
            assert_eq!(parse_chapter_time(&chapter_time(ms as f64)), Some(ms));
        }
    }

    #[test]
    fn ambiguous_or_overflowing_clocks_are_rejected() {
        for bad in [
            "1:00:00.000",
            "00:60:00.000",
            "00:00:60.000",
            "00:00:00,000",
            "99999:00:00.000",
            "00:00:00.00",
        ] {
            assert_eq!(parse_chapter_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn ends_follow_next_start_without_reordering_titles() {
        let mut c = vec![
            Chapter {
                start_ms: 5000,
                end_ms: 0,
                title: "B".into(),
            },
            Chapter {
                start_ms: 0,
                end_ms: 0,
                title: "A".into(),
            },
        ];
        c.sort_by_key(|c| c.start_ms);
        chapter_ends(&mut c, Some(9000.0));
        assert_eq!((c[0].title.as_str(), c[0].end_ms), ("A", 5000));
        assert_eq!((c[1].title.as_str(), c[1].end_ms), ("B", 9000));
    }
}
