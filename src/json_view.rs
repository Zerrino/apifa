//! Syntax highlighting for pretty-printed JSON on the dark code column.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};

use crate::theme;

const MAX_HIGHLIGHT: usize = 600_000;

fn fmt(color: Color32) -> TextFormat {
    TextFormat { font_id: FontId::monospace(13.0), color, ..Default::default() }
}

pub fn highlight(src: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    if src.len() > MAX_HIGHLIGHT {
        job.append(src, 0.0, fmt(theme::CLOUDS));
        return job;
    }
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut plain_start = 0;
    let flush = |job: &mut LayoutJob, from: usize, to: usize| {
        if to > from {
            job.append(&src[from..to], 0.0, fmt(theme::SILVER));
        }
    };
    while i < bytes.len() {
        let c = bytes[i];
        let token_end;
        let color;
        if c == b'"' {
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => j += 2,
                    b'"' => {
                        j += 1;
                        break;
                    }
                    _ => j += 1,
                }
            }
            token_end = j.min(bytes.len());
            let mut k = token_end;
            while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                k += 1;
            }
            color = if k < bytes.len() && bytes[k] == b':' { theme::CYAN } else { theme::GREEN };
        } else if c == b'-' || c.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || matches!(bytes[j], b'.' | b'e' | b'E' | b'+' | b'-')) {
                j += 1;
            }
            token_end = j;
            color = theme::YELLOW;
        } else if src[i..].starts_with("true") || src[i..].starts_with("false") {
            token_end = i + if c == b't' { 4 } else { 5 };
            color = theme::ORANGE;
        } else if src[i..].starts_with("null") {
            token_end = i + 4;
            color = theme::CONCRETE;
        } else {
            i += 1;
            continue;
        }
        flush(&mut job, plain_start, i);
        job.append(&src[i..token_end], 0.0, fmt(color));
        i = token_end;
        plain_start = i;
    }
    flush(&mut job, plain_start, bytes.len());
    job
}

const MAX_MATCHES: usize = 10_000;

/// Byte offsets of every occurrence of `needle` in `text` (ASCII case-insensitive).
pub fn find_all(text: &str, needle: &str) -> Vec<usize> {
    let needle = needle.to_ascii_lowercase();
    let mut starts = Vec::new();
    if needle.is_empty() {
        return starts;
    }
    let hay = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle) {
        starts.push(from + i);
        from += i + needle.len();
        if starts.len() >= MAX_MATCHES {
            break;
        }
    }
    starts
}

/// Every occurrence of `needle` (ASCII case-insensitive) in `job`, highlighted; `current` stands out.
/// Returns the new job and the byte offset of each match.
#[cfg(test)]
pub fn mark(job: &LayoutJob, needle: &str, current: usize) -> (LayoutJob, Vec<usize>) {
    let starts = find_all(&job.text, needle);
    let ranges: Vec<(usize, usize, bool)> = starts.iter().enumerate().map(|(i, &s)| (s, s + needle.len(), i == current)).collect();
    (mark_ranges(job, &ranges), starts)
}

/// Highlights byte ranges of `job` (start, end, is the current match), sorted and not overlapping.
pub fn mark_ranges(job: &LayoutJob, ranges: &[(usize, usize, bool)]) -> LayoutJob {
    if ranges.is_empty() {
        return job.clone();
    }
    let mut out = job.clone();
    out.sections.clear();
    let mut m = 0;
    for sec in &job.sections {
        let (mut a, b) = (sec.byte_range.start.0, sec.byte_range.end.0);
        while a < b {
            while m < ranges.len() && ranges[m].1 <= a {
                m += 1;
            }
            let (piece_end, hit) = match ranges.get(m) {
                Some(&(ms, me, cur)) if ms <= a => (me.min(b), Some(cur)),
                Some(&(ms, _, _)) => (ms.min(b), None),
                None => (b, None),
            };
            let mut format = sec.format.clone();
            if let Some(cur) = hit {
                format.background = if cur { theme::ORANGE } else { theme::YELLOW };
                format.color = theme::NAVY_DEEP;
            }
            out.sections.push(egui::text::LayoutSection {
                leading_space: if a == sec.byte_range.start.0 { sec.leading_space } else { 0.0 },
                byte_range: egui::epaint::text::ByteIndex(a)..egui::epaint::text::ByteIndex(piece_end),
                format,
            });
            a = piece_end;
        }
    }
    out
}

/// Plain text on the code column, as a job (to search it like highlighted JSON).
pub fn plain(src: &str, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(src, 0.0, fmt(theme::CLOUDS));
    job.wrap.max_width = wrap_width;
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_matches_across_sections() {
        let job = highlight("{\n  \"login\": \"Jdoe\",\n  \"x\": \"jdoe2\"\n}");
        let (marked, starts) = mark(&job, "JDOE", 1);
        assert_eq!(starts.len(), 2);
        assert_eq!(marked.text, job.text);
        // Sections still cover the whole text, in order.
        let mut at = 0;
        for s in &marked.sections {
            assert_eq!(s.byte_range.start.0, at);
            at = s.byte_range.end.0;
        }
        assert_eq!(at, job.text.len());
        let current: Vec<_> = marked.sections.iter().filter(|s| s.format.background == theme::ORANGE).collect();
        assert_eq!(&marked.text[current[0].byte_range.start.0..current[0].byte_range.end.0], "jdoe");
    }
}

#[cfg(test)]
mod bench {
    /// `cargo test bench_big -- --ignored --nocapture`: what a big response costs.
    #[test]
    #[ignore]
    fn bench_big() {
        use std::time::Instant;
        let user = |i: usize| {
            let mut o = serde_json::Map::new();
            for f in 0..30 {
                o.insert(format!("field_{f}"), serde_json::json!(format!("value {i} {f} lorem ipsum")));
            }
            o.insert(
                "image".into(),
                serde_json::json!({ "link": "https://cdn.example.com/users/x.jpg", "versions": { "large": "a", "medium": "b", "small": "c", "micro": "d" } }),
            );
            serde_json::Value::Object(o)
        };
        let body = serde_json::Value::Array((0..4000).map(user).collect());
        let t = Instant::now();
        let pretty = serde_json::to_string_pretty(&body).unwrap();
        println!("pretty JSON: {:.1} MB in {:?}", pretty.len() as f64 / 1e6, t.elapsed());
        let t = Instant::now();
        let job = super::highlight(&pretty);
        println!("highlight: {} sections in {:?}", job.sections.len(), t.elapsed());
        let t = Instant::now();
        let _clone = job.clone();
        println!("BEFORE every frame — clone of the job: {:?}", t.elapsed());
        let t = Instant::now();
        let (_m, n) = super::mark(&job, "", 0);
        println!("BEFORE every frame — mark, no search: {:?} ({} matches)", t.elapsed(), n.len());
        let t = Instant::now();
        let mut leaves = Vec::new();
        crate::flow::flatten("", &serde_json::json!({ "users": { "body": body } }), &mut leaves);
        println!("flatten: {} leaves in {:?}", leaves.len(), t.elapsed());
        let t = Instant::now();
        let lines = crate::widgets::line_starts(&pretty);
        println!("NOW once per response — line index: {} lines in {:?}", lines.len(), t.elapsed());
        let t = Instant::now();
        let hits = super::find_all(&pretty, "value 3999");
        println!("NOW once per search — find_all: {} matches in {:?}", hits.len(), t.elapsed());
        let t = Instant::now();
        for i in 1000..1060 {
            let _ = super::highlight(&pretty[lines[i]..lines[i + 1] - 1]);
        }
        println!("NOW every frame — colour the ~60 visible lines: {:?}", t.elapsed());
        let t = Instant::now();
        let p = serde_json::to_string_pretty(&body).unwrap();
        println!("log preview (whole value serialized, then cut to 4000 chars): {:?} for {} bytes", t.elapsed(), p.len());
    }
}
