//! Prompts and map-reduce over long transcripts. Generic over [`Llm`] so the orchestration can be
//! tested without a model.
//!
//! A transcript that fits the context budget is summarized in one pass. A longer one is split
//! into parts on utterance boundaries (with a little overlap); each part is boiled down to notes
//! ("map"), and the summary is written from the notes ("reduce"). Notes that are still too long
//! are condensed again, part by part, until they fit.

use earshot_protocol::{Stage, SummaryTemplate};

/// Tokens reserved for the summary itself.
pub const SUMMARY_TOKENS: usize = 2048;
/// Tokens reserved for one part's notes.
pub const NOTES_TOKENS: usize = 1024;
pub const TITLE_TOKENS: usize = 32;
/// Room for the prompt's instructions and chat markup around the transcript.
const PROMPT_OVERHEAD: usize = 700;
/// Utterances repeated at the start of the next part, so nothing said across a boundary is lost.
const OVERLAP_LINES: usize = 3;

pub struct Chat {
    pub system: String,
    pub user: String,
}

pub trait Llm {
    /// Tokens in `text`.
    fn count(&self, text: &str) -> usize;
    /// The context window, in tokens.
    fn context(&self) -> usize;
    /// Writes a reply of at most `max_tokens`, passing each new piece of text to `stream`.
    fn generate(
        &mut self,
        chat: &Chat,
        max_tokens: usize,
        stream: &mut dyn FnMut(&str),
    ) -> Result<String, String>;
}

pub struct Output {
    pub markdown: Option<String>,
    pub title: Option<String>,
    pub parts: u32,
}

/// Room for transcript or notes in a prompt that leaves `reply` tokens for the answer.
fn budget(llm: &dyn Llm, reply: usize) -> usize {
    llm.context()
        .saturating_sub(reply + PROMPT_OVERHEAD)
        .max(256)
}

/// Splits lines into parts of at most `budget` tokens, each starting with the last few lines of
/// the part before. A single line longer than the budget becomes a part of its own (cut down).
pub fn split(lines: &[&str], budget: usize, count: &dyn Fn(&str) -> usize) -> Vec<String> {
    let costs: Vec<usize> = lines.iter().map(|l| count(l) + 1).collect();
    let mut parts = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start;
        let mut used = 0;
        while end < lines.len() && (end == start || used + costs[end] <= budget) {
            used += costs[end];
            end += 1;
        }
        parts.push(lines[start..end].join("\n"));
        if end >= lines.len() {
            break;
        }
        // Overlap, but always move forward.
        start = end.saturating_sub(OVERLAP_LINES).max(start + 1);
    }
    parts
}

const RULES: &str = "Rules:
- Use only what is said in the transcript. Never guess or add outside knowledge.
- Name an action item's owner only when the transcript makes it clear; otherwise write \"(unassigned)\".
- Refer to people by the names shown in the transcript.
- Write in the same language as the transcript.
- Be concise and specific. No title, preamble or closing remarks.";

fn summary_system(t: &SummaryTemplate) -> String {
    let sections = if t.sections.is_empty() {
        "Organise it with \"## \" headings that suit the content.".to_owned()
    } else {
        format!(
            "Use exactly these sections, in this order, each as a \"## \" heading: {}. \
             If a section has nothing, write \"None.\" under it.",
            t.sections.join(", ")
        )
    };
    let focus = t.instructions.trim();
    format!(
        "You write summaries of meeting transcripts ({name}).{focus_sep}{focus}\n\n\
         Format: Markdown. {sections} Use bullet points for lists.\n\n{RULES}",
        name = t.name,
        focus_sep = if focus.is_empty() { "" } else { " " },
    )
}

fn notes_chat(part: &str, i: usize, n: usize) -> Chat {
    Chat {
        system: format!(
            "You take notes on part of a meeting transcript, for a summary written later. \
             List, as short bullet points: topics discussed, decisions, action items (with owner \
             if clear), open questions, and key facts, numbers and dates.\n\n{RULES}"
        ),
        user: format!("Part {} of {n} of the transcript:\n\n{part}", i + 1),
    }
}

fn condense_chat(notes: &str) -> Chat {
    Chat {
        system: format!(
            "You merge notes taken on consecutive parts of a meeting into one shorter set of \
             notes. Keep every decision, action item, open question and key fact; drop repetition.\
             \n\n{RULES}"
        ),
        user: format!("Notes:\n\n{notes}"),
    }
}

fn title_chat(source: &str, from_summary: bool) -> Chat {
    Chat {
        system: "You name meetings. Reply with a title of 3 to 7 words that says what this \
                 meeting was about, specific enough to tell it apart from other meetings. Reply \
                 with the title only: no quotes, no \"Title:\", no full stop. Use the language of \
                 the meeting."
            .to_owned(),
        user: if from_summary {
            format!("Meeting summary:\n\n{source}")
        } else {
            format!("Meeting transcript:\n\n{source}")
        },
    }
}

/// Tidies a model's title: first line, no quotes or label, no trailing full stop, length-capped.
pub fn clean_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let marks = |c: char| matches!(c, '#' | '*' | '_' | ' ');
    let line = line.trim_matches(marks);
    let line = ["Title:", "title:", "TITLE:"]
        .iter()
        .find_map(|p| line.strip_prefix(p))
        .unwrap_or(line)
        .trim_matches(marks);
    let line = line
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’' | '`'))
        .trim_end_matches('.')
        .trim();
    if line.is_empty() {
        return None;
    }
    let mut out: String = line.chars().take(80).collect();
    if out.len() < line.len() {
        // Cut at a word boundary.
        if let Some(i) = out.rfind(' ') {
            out.truncate(i);
        }
    }
    Some(out)
}

/// Where the visible reply starts in streamed text so far, or `None` while it may still be inside
/// (or be starting) a reasoning block.
fn visible_start(seen: &str) -> Option<usize> {
    let lead = seen.len() - seen.trim_start().len();
    let rest = &seen[lead..];
    if rest.is_empty() || "<think>".starts_with(rest) {
        return None;
    }
    if rest.starts_with("<think>") {
        let end = rest.find("</think>")? + "</think>".len();
        let after = &rest[end..];
        let ws = after.len() - after.trim_start().len();
        return Some(lead + end + ws);
    }
    Some(lead)
}

/// Strips a reasoning block a model may emit despite being asked not to.
pub fn strip_thinking(text: &str) -> &str {
    match text.find("</think>") {
        Some(i) => text[i + "</think>".len()..].trim_start(),
        None => text.trim_start(),
    }
}

/// Condenses notes until they fit `budget` tokens.
fn condense(
    llm: &mut dyn Llm,
    mut notes: Vec<String>,
    budget: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    loop {
        let joined = notes.join("\n\n");
        if llm.count(&joined) <= budget || notes.len() <= 1 {
            return Ok(joined);
        }
        // Merge neighbouring notes in groups that fit, then condense each group.
        let groups = {
            let refs: Vec<&str> = notes.iter().map(String::as_str).collect();
            let count = |s: &str| llm.count(s);
            split_groups(&refs, budget, &count)
        };
        let mut next = Vec::with_capacity(groups.len());
        for g in groups {
            if cancelled() {
                return Err("cancelled".into());
            }
            let out = llm.generate(&condense_chat(&g), NOTES_TOKENS, &mut |_| {})?;
            next.push(strip_thinking(&out).trim().to_owned());
        }
        if next.len() >= notes.len() && llm.count(&next.join("\n\n")) >= llm.count(&joined) {
            // Not shrinking: give up and let the final prompt be cut.
            return Ok(next.join("\n\n"));
        }
        notes = next;
    }
}

/// Groups consecutive items (no overlap) so each group fits `budget`.
fn split_groups(items: &[&str], budget: usize, count: &dyn Fn(&str) -> usize) -> Vec<String> {
    let mut groups = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut used = 0;
    for &it in items {
        let c = count(it) + 2;
        if !cur.is_empty() && used + c > budget {
            groups.push(cur.join("\n\n"));
            cur.clear();
            used = 0;
        }
        cur.push(it);
        used += c;
    }
    if !cur.is_empty() {
        groups.push(cur.join("\n\n"));
    }
    groups
}

/// Cuts `text` to roughly `budget` tokens, keeping whole lines from the start.
fn truncate_lines(text: &str, budget: usize, count: &dyn Fn(&str) -> usize) -> String {
    let mut used = 0;
    let mut out = Vec::new();
    for line in text.lines() {
        let c = count(line) + 1;
        if used + c > budget && !out.is_empty() {
            break;
        }
        used += c;
        out.push(line);
    }
    out.join("\n")
}

/// Writes the summary (if a template is given) and the title (if asked).
pub fn run(
    llm: &mut dyn Llm,
    transcript: &str,
    template: Option<&SummaryTemplate>,
    want_title: bool,
    progress: &mut dyn FnMut(Stage, f32),
    token: &mut dyn FnMut(&str),
    cancelled: &dyn Fn() -> bool,
) -> Result<Output, String> {
    let check = || {
        if cancelled() {
            Err("cancelled".to_owned())
        } else {
            Ok(())
        }
    };
    let mut parts = 1;
    let mut markdown = None;

    if let Some(t) = template {
        let room = budget(llm, SUMMARY_TOKENS);
        let system = summary_system(t);
        let user = if llm.count(transcript) <= room {
            format!("Transcript:\n\n{transcript}")
        } else {
            // Map: notes on each part.
            let lines: Vec<&str> = transcript.lines().collect();
            let part_room = budget(llm, NOTES_TOKENS);
            let chunks = {
                let count = |s: &str| llm.count(s);
                split(&lines, part_room, &count)
            };
            parts = chunks.len() as u32;
            let mut notes = Vec::with_capacity(chunks.len());
            for (i, chunk) in chunks.iter().enumerate() {
                check()?;
                progress(Stage::Notes, i as f32 * 100.0 / chunks.len() as f32);
                let out = llm.generate(
                    &notes_chat(chunk, i, chunks.len()),
                    NOTES_TOKENS,
                    &mut |_| {},
                )?;
                notes.push(format!(
                    "Notes on part {} of {}:\n{}",
                    i + 1,
                    chunks.len(),
                    strip_thinking(&out).trim()
                ));
            }
            progress(Stage::Notes, 100.0);
            // Reduce.
            let joined = condense(llm, notes, room, cancelled)?;
            let joined = {
                let count = |s: &str| llm.count(s);
                truncate_lines(&joined, room, &count)
            };
            format!(
                "The meeting was long, so notes were taken on each part of it in turn. Write the \
                 summary of the whole meeting from these notes.\n\n{joined}"
            )
        };
        check()?;
        progress(Stage::Summarize, 0.0);
        // Hold back a leading reasoning block from the stream.
        let mut seen = String::new();
        let mut emitted = 0;
        let out = llm.generate(&Chat { system, user }, SUMMARY_TOKENS, &mut |piece| {
            seen.push_str(piece);
            if let Some(start) = visible_start(&seen) {
                let from = start.max(emitted);
                if from < seen.len() {
                    token(&seen[from..]);
                }
                emitted = seen.len();
            }
        })?;
        progress(Stage::Summarize, 100.0);
        let md = strip_thinking(&out).trim().to_owned();
        if !md.is_empty() {
            markdown = Some(md);
        }
    }

    let mut title = None;
    if want_title {
        check()?;
        progress(Stage::Title, 0.0);
        let room = budget(llm, TITLE_TOKENS);
        let chat = match &markdown {
            Some(md) => title_chat(md, true),
            None => {
                let count = |s: &str| llm.count(s);
                title_chat(&truncate_lines(transcript, room, &count), false)
            }
        };
        let out = llm.generate(&chat, TITLE_TOKENS, &mut |_| {})?;
        title = clean_title(strip_thinking(&out));
        progress(Stage::Title, 100.0);
    }

    Ok(Output {
        markdown,
        title,
        parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One token per word; replies with canned text and records the prompts it was given.
    struct Fake {
        ctx: usize,
        prompts: Vec<String>,
        reply: Box<dyn Fn(&Chat) -> String>,
    }

    impl Llm for Fake {
        fn count(&self, text: &str) -> usize {
            text.split_whitespace().count()
        }
        fn context(&self) -> usize {
            self.ctx
        }
        fn generate(
            &mut self,
            chat: &Chat,
            _max: usize,
            stream: &mut dyn FnMut(&str),
        ) -> Result<String, String> {
            self.prompts.push(chat.user.clone());
            let r = (self.reply)(chat);
            for w in r.split_inclusive(' ') {
                stream(w);
            }
            Ok(r)
        }
    }

    fn words(s: &str) -> usize {
        s.split_whitespace().count()
    }

    fn template() -> SummaryTemplate {
        SummaryTemplate {
            name: "General meeting".into(),
            instructions: "Focus on outcomes.".into(),
            sections: vec!["Summary".into(), "Action items".into()],
        }
    }

    #[test]
    fn split_respects_budget_and_overlaps() {
        let lines: Vec<String> = (0..20)
            .map(|i| format!("[00:{i:02}] A: one two three"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let parts = split(&refs, 30, &words);
        assert!(parts.len() > 1);
        for p in &parts {
            assert!(words(p) + p.lines().count() <= 30, "{p}");
        }
        // Each part after the first starts with the previous part's last lines.
        for w in parts.windows(2) {
            let prev: Vec<&str> = w[0].lines().collect();
            let first = w[1].lines().next().unwrap();
            assert!(prev[prev.len().saturating_sub(OVERLAP_LINES)..].contains(&first));
        }
        // Every line appears somewhere.
        for l in &refs {
            assert!(parts.iter().any(|p| p.contains(l)));
        }
    }

    #[test]
    fn split_handles_a_line_longer_than_the_budget() {
        let long = "word ".repeat(50);
        let parts = split(&[long.as_str(), "short line"], 10, &words);
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn short_transcript_is_one_pass_and_streams() {
        let mut llm = Fake {
            ctx: 4000,
            prompts: vec![],
            reply: Box::new(|c| {
                if c.system.starts_with("You name") {
                    "\"Budget review for Q3.\"".into()
                } else {
                    "## Summary\nAll good.".into()
                }
            }),
        };
        let mut streamed = String::new();
        let out = run(
            &mut llm,
            "[00:01] Dana: hello there",
            Some(&template()),
            true,
            &mut |_, _| {},
            &mut |t| streamed.push_str(t),
            &|| false,
        )
        .unwrap();
        assert_eq!(out.parts, 1);
        assert_eq!(out.markdown.as_deref(), Some("## Summary\nAll good."));
        assert_eq!(streamed, "## Summary\nAll good.");
        assert_eq!(out.title.as_deref(), Some("Budget review for Q3"));
        assert_eq!(llm.prompts.len(), 2);
    }

    #[test]
    fn long_transcript_is_mapped_then_reduced() {
        let lines: Vec<String> = (0..400)
            .map(|i| {
                format!(
                    "[{:02}:{:02}] Sam: point number {i} about the plan",
                    i / 60,
                    i % 60
                )
            })
            .collect();
        let transcript = lines.join("\n");
        let mut llm = Fake {
            ctx: 2048 + 700 + 1500,
            prompts: vec![],
            reply: Box::new(|c| {
                if c.system.starts_with("You take notes") {
                    "- a note".into()
                } else {
                    "## Summary\nDone.".into()
                }
            }),
        };
        let mut stages = vec![];
        let out = run(
            &mut llm,
            &transcript,
            Some(&template()),
            false,
            &mut |s, _| stages.push(s),
            &mut |_| {},
            &|| false,
        )
        .unwrap();
        assert!(out.parts > 1, "{}", out.parts);
        assert_eq!(out.markdown.as_deref(), Some("## Summary\nDone."));
        assert!(stages.contains(&Stage::Notes));
        let last = llm.prompts.last().unwrap();
        assert!(last.contains("Notes on part 1 of"));
        assert!(!last.contains("point number"));
    }

    #[test]
    fn title_only_uses_the_transcript() {
        let mut llm = Fake {
            ctx: 4000,
            prompts: vec![],
            reply: Box::new(|_| "Title: Hiring plan for design team.".into()),
        };
        let out = run(
            &mut llm,
            "[00:01] Dana: we need two designers",
            None,
            true,
            &mut |_, _| {},
            &mut |_| {},
            &|| false,
        )
        .unwrap();
        assert_eq!(out.markdown, None);
        assert_eq!(out.title.as_deref(), Some("Hiring plan for design team"));
        assert!(llm.prompts[0].contains("two designers"));
    }

    #[test]
    fn cancelling_stops_before_generating() {
        let mut llm = Fake {
            ctx: 4000,
            prompts: vec![],
            reply: Box::new(|_| "x".into()),
        };
        let r = run(
            &mut llm,
            "[00:01] A: hi",
            Some(&template()),
            true,
            &mut |_, _| {},
            &mut |_| {},
            &|| true,
        );
        assert!(r.is_err());
        assert!(llm.prompts.is_empty());
    }

    #[test]
    fn thinking_is_hidden_from_the_stream() {
        let mut llm = Fake {
            ctx: 4000,
            prompts: vec![],
            reply: Box::new(|_| "<think> hmm </think> ## Summary".into()),
        };
        let mut streamed = String::new();
        let out = run(
            &mut llm,
            "[00:01] A: hi",
            Some(&template()),
            false,
            &mut |_, _| {},
            &mut |t| streamed.push_str(t),
            &|| false,
        )
        .unwrap();
        assert_eq!(out.markdown.as_deref(), Some("## Summary"));
        assert_eq!(streamed.trim(), "## Summary");
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(
            clean_title("  \n\"Weekly sync.\"\nmore").as_deref(),
            Some("Weekly sync")
        );
        assert_eq!(
            clean_title("**Title:** Launch plan").as_deref(),
            Some("Launch plan")
        );
        assert_eq!(
            clean_title("## Roadmap review").as_deref(),
            Some("Roadmap review")
        );
        assert_eq!(clean_title("   "), None);
        let long = clean_title(&"word ".repeat(40)).unwrap();
        assert!(long.len() <= 80 && !long.ends_with(' '));
    }

    #[test]
    fn system_prompt_lists_sections_and_rules() {
        let s = summary_system(&template());
        assert!(s.contains("Summary, Action items"));
        assert!(s.contains("Focus on outcomes."));
        assert!(s.contains("(unassigned)"));
        let free = summary_system(&SummaryTemplate {
            sections: vec![],
            ..template()
        });
        assert!(free.contains("headings that suit"));
    }
}
