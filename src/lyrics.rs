//! Lyrics data + pure helpers (LRC parsing, title cleaning, URL encoding).
//! No external dependencies, so this file can be unit-tested on its own:
//!     rustc --edition 2021 --test src/lyrics.rs -o /tmp/lyrics_test && /tmp/lyrics_test

pub struct Lyrics {
    /// (timestamp in ms, text). For unsynced lyrics the timestamps are all 0.
    pub lines: Vec<(u64, String)>,
    pub synced: bool,
    pub instrumental: bool,
}

impl Lyrics {
    /// Index of the line being sung right now; -1 before the first line.
    pub fn current_index(&self, pos_ms: u64, dur_ms: u64) -> isize {
        if self.lines.is_empty() {
            return -1;
        }
        if self.synced {
            self.lines.partition_point(|(t, _)| *t <= pos_ms + 120) as isize - 1
        } else if dur_ms > 0 {
            // Unsynced: estimate from how far through the song we are.
            let f = (pos_ms as f64 / dur_ms as f64).clamp(0.0, 0.999);
            (f * self.lines.len() as f64) as isize
        } else {
            0
        }
    }

    /// Fractional line index for scrolling synced lyrics: an integer while a line is
    /// being sung, easing to the next one over the last `LEAD_MS` before it starts
    /// (so the new line is fully highlighted exactly at its timestamp). It depends only
    /// on the playback position, so it adds no lag of its own. -1 = before the first line.
    pub fn scroll_pos(&self, pos_ms: u64) -> f32 {
        const LEAD_MS: u64 = 260;
        if self.lines.is_empty() {
            return -1.0;
        }
        let cur = self.lines.partition_point(|(t, _)| *t <= pos_ms) as isize - 1;
        let Some((next_t, _)) = self.lines.get((cur + 1) as usize) else {
            return cur as f32;
        };
        let next_t = *next_t;
        let cur_t = if cur >= 0 { self.lines[cur as usize].0 } else { 0 };
        let span = next_t.saturating_sub(cur_t).min(LEAD_MS).max(1);
        let start = next_t - span;
        if pos_ms <= start {
            return cur as f32;
        }
        let f = ((pos_ms - start) as f32 / span as f32).clamp(0.0, 1.0);
        cur as f32 + f * f * (3.0 - 2.0 * f)
    }

    /// Text of line `idx`; empty (instrumental gap) lines show as a note.
    pub fn text_at(&self, idx: isize) -> Option<&str> {
        if idx < 0 {
            return None;
        }
        let (_, t) = self.lines.get(idx as usize)?;
        Some(if t.trim().is_empty() {
            "♪"
        } else {
            t.as_str()
        })
    }
}

/// "[mm:ss.xx]" -> milliseconds. Returns None for metadata tags like "[ar:Artist]".
fn parse_ts(tag: &str) -> Option<u64> {
    let (m, s) = tag.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let s: f64 = s.trim().parse().ok()?;
    if !(0.0..1000.0).contains(&s) {
        return None;
    }
    Some(m * 60_000 + (s * 1000.0).round() as u64)
}

/// Parses synced lyrics: lines like "[01:23.45]text", possibly with several stamps.
pub fn parse_lrc(src: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for line in src.lines() {
        let mut rest = line.trim();
        let mut stamps = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            if let Some(ms) = parse_ts(&rest[1..end]) {
                stamps.push(ms);
            }
            rest = rest[end + 1..].trim_start();
        }
        for ms in stamps {
            out.push((ms, rest.to_string()));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

/// Unsynced lyrics: non-empty lines, all at t = 0.
pub fn plain_lines(src: &str) -> Vec<(u64, String)> {
    src.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| (0, l.to_string()))
        .collect()
}

/// Strips "(Official MV)", "[HD]", "ft. X", "| Channel" and trailing "Official Video" etc.
pub fn clean_title(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    // ASCII-lowercasing keeps byte offsets identical, so `find` indices stay valid.
    let lower = out.to_ascii_lowercase();
    let mut cut = out.len();
    for pat in [" ft.", " feat.", " feat ", " ft ", " | "] {
        if let Some(i) = lower.find(pat) {
            cut = cut.min(i);
        }
    }
    out.truncate(cut);

    let mut s2 = out.split_whitespace().collect::<Vec<_>>().join(" ");
    loop {
        let low = s2.to_ascii_lowercase();
        let mut changed = false;
        for suf in [
            " official music video",
            " official video",
            " official audio",
            " official mv",
            " music video",
            " lyric video",
            " lyrics",
            " audio",
            " mv",
        ] {
            if low.ends_with(suf) {
                s2.truncate(s2.len() - suf.len());
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    s2.trim().trim_end_matches('-').trim().to_string()
}

/// Percent-encodes a query-string value.
pub fn url_encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{:02X}", b)),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_multiple_stamps_metadata_and_sorting() {
        let src =
            "[ar:Someone]\n[00:01.50] hello\n[00:00.50][00:10.00]a\nno stamp here\n[bad:tag]x";
        assert_eq!(
            parse_lrc(src),
            vec![
                (500, "a".to_string()),
                (1500, "hello".to_string()),
                (10000, "a".to_string())
            ]
        );
    }

    #[test]
    fn lrc_keeps_empty_instrumental_lines() {
        let v = parse_lrc("[00:05.00]\n[00:09.25]la");
        assert_eq!(v, vec![(5000, "".to_string()), (9250, "la".to_string())]);
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(
            clean_title("Hazama - Sampai Mati (Official MV)"),
            "Hazama - Sampai Mati"
        );
        assert_eq!(
            clean_title("Radiohead - Creep (Official Video) [HD]"),
            "Radiohead - Creep"
        );
        assert_eq!(clean_title("Song Title ft. Someone"), "Song Title");
        assert_eq!(clean_title("Sampai Mati Official MV"), "Sampai Mati");
        assert_eq!(clean_title("Creep"), "Creep");
        assert_eq!(clean_title("Color Your Night"), "Color Your Night");
    }

    #[test]
    fn url_encoding() {
        assert_eq!(url_encode("a b&c"), "a%20b%26c");
        assert_eq!(url_encode("é"), "%C3%A9");
        assert_eq!(url_encode("Az-_.~09"), "Az-_.~09");
    }

    #[test]
    fn scroll_position_is_exact_at_timestamps() {
        let l = Lyrics {
            lines: vec![(1000, "a".into()), (5000, "b".into()), (5100, "c".into())],
            synced: true,
            instrumental: false,
        };
        assert_eq!(l.scroll_pos(0), -1.0);
        assert_eq!(l.scroll_pos(739), -1.0);
        assert_eq!(l.scroll_pos(870), -0.5); // halfway through the intro -> line 0 ease
        assert_eq!(l.scroll_pos(1000), 0.0);
        assert_eq!(l.scroll_pos(4000), 0.0);
        assert_eq!(l.scroll_pos(5000), 1.0);
        // lines only 100 ms apart: the ease shrinks to fit
        assert_eq!(l.scroll_pos(5050), 1.5);
        assert_eq!(l.scroll_pos(5100), 2.0);
        assert_eq!(l.scroll_pos(99_000), 2.0);
    }

    #[test]
    fn current_line_tracking() {
        let l = Lyrics {
            lines: vec![(1000, "a".into()), (5000, "b".into()), (9000, "".into())],
            synced: true,
            instrumental: false,
        };
        assert_eq!(l.current_index(0, 0), -1);
        assert_eq!(l.current_index(1000, 0), 0);
        // lines switch 120 ms early so the highlight feels in time with the singing
        assert_eq!(l.current_index(4879, 0), 0);
        assert_eq!(l.current_index(4880, 0), 1);
        assert_eq!(l.current_index(5000, 0), 1);
        assert_eq!(l.current_index(99_000, 0), 2);
        assert_eq!(l.text_at(2), Some("♪"));
        assert_eq!(l.text_at(3), None);
        assert_eq!(l.text_at(-1), None);

        let u = Lyrics {
            lines: vec![
                (0, "a".into()),
                (0, "b".into()),
                (0, "c".into()),
                (0, "d".into()),
            ],
            synced: false,
            instrumental: false,
        };
        assert_eq!(u.current_index(0, 100_000), 0);
        assert_eq!(u.current_index(50_000, 100_000), 2);
        assert_eq!(u.current_index(100_000, 100_000), 3);
    }
}
