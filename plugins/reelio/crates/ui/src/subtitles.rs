//! SubRip document editing and WebVTT inspection. Port of `features/subtitles/editorModel.ts`
//! and `features/subtitles/preview.ts` (inspection only; the overlay binding is host-side).

use std::collections::BTreeSet;

pub const MAX_TIME: i64 = 359_999_999;
pub const MAX_CUES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtCue {
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SrtDocument {
    pub cues: Vec<SrtCue>,
}

/// `HH:MM:SS,mmm` (SRT clock).
pub fn cue_clock(ms: i64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// Strict SRT clock parser (`HH:MM:SS,mmm`, dot accepted as separator).
pub fn parse_cue_clock(value: &str) -> Result<i64, String> {
    let b = value.as_bytes();
    let shape = b.len() == 12
        && b[2] == b':'
        && b[5] == b':'
        && (b[8] == b',' || b[8] == b'.')
        && value
            .char_indices()
            .all(|(i, c)| matches!(i, 2 | 5 | 8) || c.is_ascii_digit());
    if !shape {
        return Err("Use HH:MM:SS,mmm".into());
    }
    let n = |r: std::ops::Range<usize>| value[r].parse::<i64>().map_err(|e| e.to_string());
    let (h, m, s, ms) = (n(0..2)?, n(3..5)?, n(6..8)?, n(9..12)?);
    if m > 59 || s > 59 {
        return Err("Minutes and seconds must be below 60".into());
    }
    Ok(((h * 60 + m) * 60 + s) * 1000 + ms)
}

/// Validation messages shown beside the cue list. Empty means the document can be saved.
pub fn cue_issues(doc: &SrtDocument) -> Vec<String> {
    let mut issues = vec![];
    if doc.cues.is_empty() {
        issues.push("Add at least one cue before saving.".into());
    }
    if doc.cues.len() > MAX_CUES {
        issues.push("Maximum 10,000 cues.".into());
    }
    let mut previous: Option<&SrtCue> = None;
    for (c, cue) in doc.cues.iter().enumerate() {
        let label = format!("Cue {}", c + 1);
        if cue.start_ms < 0 || cue.end_ms <= cue.start_ms || cue.end_ms > MAX_TIME {
            issues.push(format!("{label}: invalid timing."));
        }
        if cue.text.trim().is_empty() {
            issues.push(format!("{label}: enter subtitle text."));
        }
        // Mirrors /\n\s*\n|\r|\0/: an interior whitespace-only line, CR or NUL.
        let lines: Vec<&str> = cue.text.split('\n').collect();
        let interior_blank = lines.len() >= 3
            && lines[1..lines.len() - 1]
                .iter()
                .any(|l| l.trim().is_empty());
        if interior_blank || cue.text.contains('\r') || cue.text.contains('\0') {
            issues.push(format!(
                "{label}: blank text lines/control characters cannot be saved in SRT."
            ));
        }
        if let Some(prev) = previous {
            if cue.start_ms < prev.start_ms {
                issues.push(format!(
                    "{label}: starts before the previous cue (sort if unintended)."
                ));
            } else if cue.start_ms < prev.end_ms {
                issues.push(format!("{label}: overlaps the previous cue."));
            }
        }
        let seconds = ((cue.end_ms - cue.start_ms) as f64 / 1000.0).max(0.001);
        if cue.text.chars().count() as f64 / seconds > 25.0 {
            issues.push(format!("{label}: more than 25 characters per second."));
        }
        if cue.text.split('\n').any(|l| l.chars().count() > 42) {
            issues.push(format!("{label}: a line exceeds 42 characters."));
        }
        previous = Some(cue);
    }
    issues
}

/// Shift the cues in `ids` by whole milliseconds; the whole result must stay within bounds.
pub fn shift_cues(
    doc: &SrtDocument,
    ids: &BTreeSet<String>,
    offset: i64,
) -> Result<SrtDocument, String> {
    if offset.abs() > 86_400_000 {
        return Err("Shift must be whole milliseconds within 24 hours.".into());
    }
    let cues: Vec<SrtCue> = doc
        .cues
        .iter()
        .map(|c| {
            if ids.contains(&c.id) {
                SrtCue {
                    start_ms: c.start_ms + offset,
                    end_ms: c.end_ms + offset,
                    ..c.clone()
                }
            } else {
                c.clone()
            }
        })
        .collect();
    if cues.iter().any(|c| c.start_ms < 0 || c.end_ms > MAX_TIME) {
        return Err("Shift would place a cue outside supported time bounds.".into());
    }
    Ok(SrtDocument { cues })
}

/// Split at a time inside the cue and a text caret (character index). `new_id` supplies the
/// identity of the second half, which is always new.
pub fn split_cue(
    doc: &SrtDocument,
    id: &str,
    at: i64,
    caret: usize,
    new_id: impl FnOnce() -> String,
) -> Result<SrtDocument, String> {
    let index = doc
        .cues
        .iter()
        .position(|c| c.id == id)
        .ok_or("Split time must be inside the selected cue.")?;
    let cue = &doc.cues[index];
    if at <= cue.start_ms || at >= cue.end_ms {
        return Err("Split time must be inside the selected cue.".into());
    }
    let chars: Vec<char> = cue.text.chars().collect();
    if caret > chars.len() {
        return Err("Place the cursor between complete characters before splitting.".into());
    }
    let left: String = chars[..caret].iter().collect::<String>().trim().to_string();
    let right: String = chars[caret..].iter().collect::<String>().trim().to_string();
    if left.is_empty() || right.is_empty() {
        return Err("Place the text cursor between the two parts before splitting.".into());
    }
    let mut cues = doc.cues.clone();
    cues.splice(
        index..=index,
        [
            SrtCue {
                end_ms: at,
                text: left,
                ..cue.clone()
            },
            SrtCue {
                id: new_id(),
                start_ms: at,
                end_ms: cue.end_ms,
                text: right,
            },
        ],
    );
    Ok(SrtDocument { cues })
}

/// Merge a cue with the following one (adjacent only).
pub fn merge_cue(doc: &SrtDocument, id: &str) -> Result<SrtDocument, String> {
    let index = doc.cues.iter().position(|c| c.id == id);
    let (Some(i), true) = (
        index,
        index.map(|i| i + 1 < doc.cues.len()).unwrap_or(false),
    ) else {
        return Err("Select a cue with a following cue to merge.".into());
    };
    let (a, b) = (&doc.cues[i], &doc.cues[i + 1]);
    let mut cues = doc.cues.clone();
    cues.splice(
        i..=i + 1,
        [SrtCue {
            id: a.id.clone(),
            start_ms: a.start_ms.min(b.start_ms),
            end_ms: a.end_ms.max(b.end_ms),
            text: format!("{}\n{}", a.text, b.text).trim().to_string(),
        }],
    );
    Ok(SrtDocument { cues })
}

/// Literal, case-sensitive replacement in the chosen cues only.
pub fn replace_text(
    doc: &SrtDocument,
    find: &str,
    replacement: &str,
    ids: &BTreeSet<String>,
) -> Result<SrtDocument, String> {
    if find.is_empty() {
        return Err("Enter text to find.".into());
    }
    Ok(SrtDocument {
        cues: doc
            .cues
            .iter()
            .map(|c| {
                if ids.contains(&c.id) {
                    SrtCue {
                        text: c.text.replace(find, replacement),
                        ..c.clone()
                    }
                } else {
                    c.clone()
                }
            })
            .collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleCue {
    pub start: i64,
    pub end: i64,
    pub text: String,
}

/// `(HH:)MM:SS.mmm` as in the reference `timestamp()`: minutes and seconds below 60.
fn vtt_timestamp(text: &str) -> Result<i64, String> {
    let invalid = || "Invalid WebVTT timestamp".to_string();
    let (hms, millis) = text.rsplit_once('.').ok_or_else(invalid)?;
    if millis.len() != 3 || !millis.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let parts: Vec<&str> = hms.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return Err(invalid());
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !parts.iter().all(|p| digits(p)) {
        return Err(invalid());
    }
    let (mi, se) = (parts[parts.len() - 2], parts[parts.len() - 1]);
    if se.len() != 2 || mi.len() != 2 {
        return Err(invalid());
    }
    let num = |s: &str| s.parse::<i64>().map_err(|_| invalid());
    let (minutes, seconds) = (num(mi)?, num(se)?);
    if minutes >= 60 || seconds >= 60 {
        return Err("Invalid WebVTT timestamp range".into());
    }
    let hours = if parts.len() == 3 { num(parts[0])? } else { 0 };
    let total = ((hours * 60 + minutes) * 60 + seconds) * 1000 + num(millis)?;
    if total > 31_536_000_000 {
        return Err("Subtitle timestamp exceeds the supported range".into());
    }
    Ok(total)
}

/// Inspect a WebVTT preview. Rejects non-WebVTT, oversized (4 MiB), empty or >10,000-cue input.
pub fn inspect_vtt(text: &str) -> Result<Vec<SubtitleCue>, String> {
    if text.len() > 4 * 1024 * 1024 || !text.starts_with("WEBVTT") {
        return Err("Invalid or oversized subtitle preview".into());
    }
    let normalized = text.replace('\r', "");
    let mut cues = vec![];
    for block in normalized.split("\n\n") {
        let lines: Vec<&str> = block.split('\n').collect();
        let Some(index) = lines.iter().position(|l| l.contains(" --> ")) else {
            continue;
        };
        let (start, end) = lines[index].split_once(" --> ").unwrap_or(("", ""));
        let end_token = end.trim().split_whitespace().next().unwrap_or("");
        let cue = SubtitleCue {
            start: vtt_timestamp(start.trim())?,
            end: vtt_timestamp(end_token)?,
            text: lines[index + 1..].join("\n").trim_end().to_string(),
        };
        if cue.end <= cue.start {
            return Err("Subtitle cue has a non-positive duration".into());
        }
        cues.push(cue);
        if cues.len() > MAX_CUES {
            return Err("Preview supports up to 10,000 subtitle cues".into());
        }
    }
    if cues.is_empty() {
        return Err("No displayable subtitle cues found".into());
    }
    Ok(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(id: &str, s: i64, e: i64, t: &str) -> SrtCue {
        SrtCue {
            id: id.into(),
            start_ms: s,
            end_ms: e,
            text: t.into(),
        }
    }

    #[test]
    fn clocks_round_trip_and_reject_malformed_values() {
        assert_eq!(cue_clock(3_723_004), "01:02:03,004");
        assert_eq!(parse_cue_clock("01:02:03,004"), Ok(3_723_004));
        assert!(parse_cue_clock("01:60:03,004").is_err());
        assert!(parse_cue_clock("1:02:03,004").is_err());
    }

    #[test]
    fn issues_report_overlap_missing_text_and_reading_speed() {
        let doc = SrtDocument {
            cues: vec![cue("a", 0, 1000, "hello"), cue("b", 500, 2000, "   ")],
        };
        let issues = cue_issues(&doc);
        assert!(issues.iter().any(|i| i.contains("overlaps")));
        assert!(issues.iter().any(|i| i.contains("enter subtitle text")));
        let fast = SrtDocument {
            cues: vec![cue("a", 0, 100, &"x".repeat(10))],
        };
        assert!(cue_issues(&fast)
            .iter()
            .any(|i| i.contains("25 characters")));
    }

    #[test]
    fn shift_affects_only_selected_cues_and_keeps_input() {
        let doc = SrtDocument {
            cues: vec![cue("a", 0, 1000, "a"), cue("b", 2000, 3000, "b")],
        };
        let ids: BTreeSet<String> = ["b".to_string()].into();
        let out = shift_cues(&doc, &ids, 500).unwrap();
        assert_eq!(out.cues[0].start_ms, 0);
        assert_eq!(out.cues[1].start_ms, 2500);
        assert_eq!(doc.cues[1].start_ms, 2000, "input is not mutated");
        assert!(
            shift_cues(&doc, &ids, -2500).is_err(),
            "shift below zero refused"
        );
    }

    #[test]
    fn split_creates_a_new_identity_and_merge_keeps_the_first() {
        let doc = SrtDocument {
            cues: vec![cue("a", 0, 4000, "one two")],
        };
        let split = split_cue(&doc, "a", 2000, 3, || "new".into()).unwrap();
        assert_eq!(split.cues[0].text, "one");
        assert_eq!(split.cues[1].id, "new");
        assert_eq!(split.cues[1].text, "two");
        let merged = merge_cue(&split, "a").unwrap();
        assert_eq!(merged.cues.len(), 1);
        assert_eq!(merged.cues[0].id, "a");
        assert_eq!(merged.cues[0].end_ms, 4000);
    }

    #[test]
    fn replace_is_literal_and_scoped() {
        let doc = SrtDocument {
            cues: vec![cue("a", 0, 1, "Hi hi"), cue("b", 2, 3, "Hi")],
        };
        let ids: BTreeSet<String> = ["a".to_string()].into();
        let out = replace_text(&doc, "hi", "yo", &ids).unwrap();
        assert_eq!(out.cues[0].text, "Hi hi".replace("hi", "yo"));
        assert_eq!(out.cues[1].text, "Hi");
    }

    #[test]
    fn webvtt_inspection_preserves_timing_and_rejects_malformed_input() {
        let vtt = "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello\n\n00:01:00.000 --> 00:01:01.000 align:start\nWorld\nline two\n";
        let cues = inspect_vtt(vtt).unwrap();
        assert_eq!(
            cues[0],
            SubtitleCue {
                start: 1000,
                end: 2500,
                text: "Hello".into()
            }
        );
        assert_eq!(cues[1].start, 60_000);
        assert_eq!(cues[1].text, "World\nline two");
        assert!(inspect_vtt("nope").is_err());
        assert!(inspect_vtt("WEBVTT\n\n00:00:02.000 --> 00:00:01.000\nX\n").is_err());
        assert!(inspect_vtt("WEBVTT\n").is_err());
    }
}
