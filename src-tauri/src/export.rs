//! Transcript exports: Markdown, plain text, SubRip, WebVTT and JSON. Pure functions over the
//! stored transcript, so they can be tested without the app.

use serde::Serialize;

use crate::db::{MeetingRow, UtteranceRow};

/// Longest subtitle cue (by time and characters) before a line is split at a word boundary.
const CUE_MAX_MS: i64 = 6_000;
const CUE_MAX_CHARS: usize = 84;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Text,
    Srt,
    Vtt,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "md" => Self::Markdown,
            "txt" => Self::Text,
            "srt" => Self::Srt,
            "vtt" => Self::Vtt,
            "json" => Self::Json,
            _ => return None,
        })
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Text => "txt",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::Json => "json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Text => "Text",
            Self::Srt => "SubRip subtitles",
            Self::Vtt => "WebVTT subtitles",
            Self::Json => "JSON",
        }
    }
}

/// Everything an export needs.
pub struct Doc<'a> {
    pub meeting: &'a MeetingRow,
    pub lines: &'a [UtteranceRow],
    /// Name shown for the mic speaker.
    pub me_name: &'a str,
    /// Local date and time of the meeting, already formatted.
    pub when: String,
}

impl Doc<'_> {
    fn title(&self) -> &str {
        let t = self.meeting.title.trim();
        if t.is_empty() { "Untitled meeting" } else { t }
    }

    fn speaker(&self, u: &UtteranceRow) -> String {
        if u.is_me || (u.speaker_label.is_none() && u.track_kind == "mic") {
            return self.me_name.to_owned();
        }
        u.speaker_name
            .clone()
            .or_else(|| u.speaker_label.clone())
            .unwrap_or_else(|| "Meeting".to_owned())
    }
}

pub fn render(doc: &Doc<'_>, format: Format) -> String {
    match format {
        Format::Markdown => markdown(doc),
        Format::Text => text(doc),
        Format::Srt => subtitles(doc, false),
        Format::Vtt => subtitles(doc, true),
        Format::Json => json(doc),
    }
}

/// m:ss or h:mm:ss.
fn stamp(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn duration(ms: i64) -> String {
    let m = (ms.max(0) + 30_000) / 60_000;
    if m >= 60 {
        format!("{} h {} min", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

fn markdown(doc: &Doc<'_>) -> String {
    let mut out = format!("# {}\n\n", doc.title());
    let mut meta = vec![doc.when.clone()];
    if doc.meeting.duration_ms > 0 {
        meta.push(duration(doc.meeting.duration_ms));
    }
    if !doc.meeting.tags.is_empty() {
        meta.push(
            doc.meeting
                .tags
                .iter()
                .map(|t| format!("#{t}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    out.push_str(&format!("{}\n\n## Transcript\n\n", meta.join(" · ")));
    for u in doc.lines {
        out.push_str(&format!(
            "**[{}] {}:** {}\n\n",
            stamp(u.start_ms),
            doc.speaker(u),
            u.text
        ));
    }
    out
}

fn text(doc: &Doc<'_>) -> String {
    let mut out = format!("{}\n{}\n\n", doc.title(), doc.when);
    for u in doc.lines {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            stamp(u.start_ms),
            doc.speaker(u),
            u.text
        ));
    }
    out
}

/// One subtitle cue.
struct Cue {
    start: i64,
    end: i64,
    speaker: String,
    text: String,
}

/// Splits each line into cues of at most [`CUE_MAX_MS`] / [`CUE_MAX_CHARS`] at word boundaries.
/// Lines edited by hand keep one cue: their word timings no longer match the text.
fn cues(doc: &Doc<'_>) -> Vec<Cue> {
    let mut out = Vec::new();
    for u in doc.lines {
        let speaker = doc.speaker(u);
        if u.edited || u.words.is_empty() {
            out.push(Cue {
                start: u.start_ms,
                end: u.end_ms.max(u.start_ms + 500),
                speaker,
                text: u.text.clone(),
            });
            continue;
        }
        let mut cur: Option<Cue> = None;
        for w in &u.words {
            let word = w.text.trim();
            if word.is_empty() {
                continue;
            }
            if let Some(c) = &cur
                && (w.end_ms - c.start > CUE_MAX_MS
                    || c.text.len() + 1 + word.len() > CUE_MAX_CHARS)
            {
                out.push(cur.take().expect("checked"));
            }
            match &mut cur {
                Some(c) => {
                    c.text.push(' ');
                    c.text.push_str(word);
                    c.end = w.end_ms;
                }
                None => {
                    cur = Some(Cue {
                        start: w.start_ms,
                        end: w.end_ms,
                        speaker: speaker.clone(),
                        text: word.to_owned(),
                    })
                }
            }
        }
        out.extend(cur);
    }
    out
}

fn cue_time(ms: i64, vtt: bool) -> String {
    let ms = ms.max(0);
    let (h, m, s, f) = (
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000,
    );
    let sep = if vtt { '.' } else { ',' };
    format!("{h:02}:{m:02}:{s:02}{sep}{f:03}")
}

fn subtitles(doc: &Doc<'_>, vtt: bool) -> String {
    let mut out = if vtt {
        String::from("WEBVTT\n\n")
    } else {
        String::new()
    };
    for (i, c) in cues(doc).iter().enumerate() {
        let times = format!("{} --> {}", cue_time(c.start, vtt), cue_time(c.end, vtt));
        if vtt {
            // Voice spans carry the speaker; "<" and "&" must be escaped in cue text.
            let esc = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            };
            out.push_str(&format!(
                "{times}\n<v {}>{}\n\n",
                esc(&c.speaker),
                esc(&c.text)
            ));
        } else {
            out.push_str(&format!(
                "{}\n{times}\n{}: {}\n\n",
                i + 1,
                c.speaker,
                c.text
            ));
        }
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonDoc<'a> {
    title: &'a str,
    started_at: i64,
    duration_ms: i64,
    language: Option<&'a str>,
    tags: &'a [String],
    utterances: Vec<JsonUtterance<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonUtterance<'a> {
    start_ms: i64,
    end_ms: i64,
    speaker: String,
    /// "mic" (you) or "system" (the meeting audio).
    track: &'a str,
    edited: bool,
    text: &'a str,
    words: Vec<JsonWord<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonWord<'a> {
    start_ms: i64,
    end_ms: i64,
    text: &'a str,
    prob: Option<f64>,
}

fn json(doc: &Doc<'_>) -> String {
    let j = JsonDoc {
        title: doc.title(),
        started_at: doc.meeting.started_at,
        duration_ms: doc.meeting.duration_ms,
        language: doc.meeting.language.as_deref(),
        tags: &doc.meeting.tags,
        utterances: doc
            .lines
            .iter()
            .map(|u| JsonUtterance {
                start_ms: u.start_ms,
                end_ms: u.end_ms,
                speaker: doc.speaker(u),
                track: &u.track_kind,
                edited: u.edited,
                text: &u.text,
                words: u
                    .words
                    .iter()
                    .map(|w| JsonWord {
                        start_ms: w.start_ms,
                        end_ms: w.end_ms,
                        text: w.text.trim(),
                        prob: w.prob,
                    })
                    .collect(),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&j).expect("serializes") + "\n"
}

/// A file name from the meeting title, without characters Windows or macOS reject.
pub fn file_stem(meeting: &MeetingRow, date: &str) -> String {
    let title: String = meeting
        .title
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = title.chars().take(80).collect();
    if title.is_empty() {
        format!("Meeting {date}")
    } else {
        format!("{title} {date}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::WordRow;

    fn meeting() -> MeetingRow {
        MeetingRow {
            id: 1,
            title: "Weekly sync".into(),
            started_at: 0,
            duration_ms: 65 * 60_000,
            status: "ready".into(),
            error: None,
            remote_speaker_count_hint: None,
            tags: vec!["team".into()],
            language: Some("en".into()),
            speakers: Vec::new(),
        }
    }

    fn line(
        start: i64,
        label: Option<&str>,
        name: Option<&str>,
        is_me: bool,
        words: &[(&str, i64, i64)],
    ) -> UtteranceRow {
        UtteranceRow {
            id: start,
            track_kind: if is_me { "mic".into() } else { "system".into() },
            speaker_id: Some(1),
            speaker_label: label.map(Into::into),
            speaker_name: name.map(Into::into),
            is_me,
            edited: false,
            start_ms: start,
            end_ms: words.last().map_or(start, |w| w.2),
            text: words.iter().map(|w| w.0).collect::<Vec<_>>().join(" "),
            words: words
                .iter()
                .map(|&(t, s, e)| WordRow {
                    start_ms: s,
                    end_ms: e,
                    text: format!(" {t}"),
                    prob: Some(0.9),
                })
                .collect(),
        }
    }

    fn doc_lines() -> Vec<UtteranceRow> {
        vec![
            line(
                1_000,
                Some("Speaker 1"),
                Some("Dana"),
                false,
                &[("Hello", 1_000, 1_400), ("<team>.", 1_400, 2_000)],
            ),
            line(
                3_725_000,
                Some("Me"),
                None,
                true,
                &[("Thanks", 3_725_000, 3_725_500)],
            ),
        ]
    }

    #[test]
    fn markdown_and_text_use_names_and_timestamps() {
        let m = meeting();
        let lines = doc_lines();
        let doc = Doc {
            meeting: &m,
            lines: &lines,
            me_name: "Sam",
            when: "Nov 25, 2026, 10:00".into(),
        };
        let md = render(&doc, Format::Markdown);
        assert!(
            md.starts_with("# Weekly sync\n\nNov 25, 2026, 10:00 · 1 h 5 min · #team\n"),
            "{md}"
        );
        assert!(md.contains("**[0:01] Dana:** Hello <team>.\n"));
        assert!(md.contains("**[1:02:05] Sam:** Thanks\n"));
        let txt = render(&doc, Format::Text);
        assert!(
            txt.contains("[0:01] Dana: Hello <team>.\n[1:02:05] Sam: Thanks\n"),
            "{txt}"
        );
    }

    #[test]
    fn subtitles_have_valid_times_and_escaping() {
        let m = meeting();
        let lines = doc_lines();
        let doc = Doc {
            meeting: &m,
            lines: &lines,
            me_name: "Sam",
            when: String::new(),
        };
        let srt = render(&doc, Format::Srt);
        assert!(
            srt.starts_with(
                "1\n00:00:01,000 --> 00:00:02,000\nDana: Hello <team>.\n\n2\n01:02:05,000"
            ),
            "{srt}"
        );
        let vtt = render(&doc, Format::Vtt);
        assert!(
            vtt.starts_with(
                "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<v Dana>Hello &lt;team&gt;.\n"
            ),
            "{vtt}"
        );
    }

    #[test]
    fn long_lines_split_into_short_cues() {
        let m = meeting();
        let words: Vec<(String, i64, i64)> = (0..30)
            .map(|i| (format!("word{i}"), i * 500, i * 500 + 400))
            .collect();
        let refs: Vec<(&str, i64, i64)> =
            words.iter().map(|(t, s, e)| (t.as_str(), *s, *e)).collect();
        let lines = vec![line(0, Some("Speaker 1"), None, false, &refs)];
        let doc = Doc {
            meeting: &m,
            lines: &lines,
            me_name: "Me",
            when: String::new(),
        };
        let cues = cues(&doc);
        assert!(cues.len() >= 3);
        for c in &cues {
            assert!(
                c.end - c.start <= CUE_MAX_MS && c.text.len() <= CUE_MAX_CHARS,
                "{} {}",
                c.end - c.start,
                c.text
            );
        }
        assert_eq!(
            cues.iter()
                .map(|c| c.text.split(' ').count())
                .sum::<usize>(),
            30,
            "no word lost"
        );
    }

    #[test]
    fn json_round_trips_and_names_are_safe() {
        let m = meeting();
        let lines = doc_lines();
        let doc = Doc {
            meeting: &m,
            lines: &lines,
            me_name: "Sam",
            when: String::new(),
        };
        let v: serde_json::Value = serde_json::from_str(&render(&doc, Format::Json)).unwrap();
        assert_eq!(v["utterances"][0]["speaker"], "Dana");
        assert_eq!(v["utterances"][0]["words"][1]["text"], "<team>.");
        assert_eq!(v["utterances"][1]["track"], "mic");
        let mut odd = meeting();
        odd.title = "  Q3: plan / budget?  ".into();
        assert_eq!(file_stem(&odd, "2026-11-25"), "Q3 plan budget 2026-11-25");
        odd.title = String::new();
        assert_eq!(file_stem(&odd, "2026-11-25"), "Meeting 2026-11-25");
    }
}
