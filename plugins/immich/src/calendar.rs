//! Capture-date grouping (`Immich / Annual / year / month`). Ported from Pigeon's `immich::image_date`.
//! Capture-local dates decide the grouping; invalid or missing dates stay available at the root only.

use crate::api::MONTHS;
use serde_json::Value;

/// Returns `(year, month, day)` for a valid image asset, or `None`.
pub fn image_date(asset: &Value) -> Option<(u32, usize, u32)> {
    if asset["type"] != "IMAGE" || asset["id"].as_str().unwrap_or("").is_empty() {
        return None;
    }
    let date = asset["localDateTime"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| asset["fileCreatedAt"].as_str())?;
    parse_date(date)
}

/// Parses the `YYYY-MM-DD` prefix of an ISO timestamp and checks that the date exists.
pub fn parse_date(date: &str) -> Option<(u32, usize, u32)> {
    let bytes = date.as_bytes();
    if bytes.len() < 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || ![&bytes[0..4], &bytes[5..7], &bytes[8..10]].iter().all(|s| s.iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    let year = date.get(0..4)?.parse::<u32>().ok()?;
    let month = date.get(5..7)?.parse::<usize>().ok()?;
    let day = date.get(8..10)?.parse::<u32>().ok()?;
    if year == 0 || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if day == 0 || day > days[month - 1] {
        return None;
    }
    Some((year, month, day))
}

pub fn month_name(month: usize) -> &'static str {
    MONTHS[month - 1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fallback_to_file_created_when_local_time_is_missing() {
        let fallback = json!({"id":"x","type":"IMAGE","fileCreatedAt":"2022-07-09T12:00:00Z"});
        assert_eq!(image_date(&fallback), Some((2022, 7, 9)));
    }

    #[test]
    fn invalid_dates_and_leap_years_are_validated() {
        for (date, valid) in [
            ("2000-02-29", true),
            ("1900-02-29", false),
            ("2024-02-29", true),
            ("2024-00-01", false),
            ("0000-01-01", false),
            ("2024-04-31", false),
            ("é024-01-01", false),
        ] {
            assert_eq!(image_date(&json!({"id":"x","type":"IMAGE","localDateTime":date})).is_some(), valid, "{date}");
        }
    }

    #[test]
    fn videos_and_anonymous_assets_have_no_calendar_date() {
        assert_eq!(image_date(&json!({"id":"v","type":"VIDEO","localDateTime":"2023-01-01T00:00:00"})), None);
        assert_eq!(image_date(&json!({"id":"","type":"IMAGE","localDateTime":"2023-01-01T00:00:00"})), None);
    }
}
