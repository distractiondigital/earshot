//! Turning a track's words plus diarization turns into speaker-labelled utterances, and removing
//! remote speech that leaked into the mic track ("bleed").

use earshot_protocol::{SpeakerTurn, Transcript};

use crate::db::{NewUtterance, NewWord};

/// A pause longer than this starts a new utterance.
pub const UTTERANCE_PAUSE_MS: i64 = 1_200;
/// A word this far from any speaker turn is still given to the nearest one.
const NEAREST_TURN_MS: i64 = 1_000;
/// Mic utterance is dropped as bleed when this share of its words appears, in order, in the
/// meeting track around the same time.
const BLEED_MATCH_RATIO: f32 = 0.8;
/// How far apart in time a mic utterance and its echo in the meeting track may be.
const BLEED_WINDOW_MS: i64 = 1_500;

/// The words of a transcript in order, without empty tokens.
pub fn words(t: &Transcript) -> Vec<NewWord> {
    t.segments
        .iter()
        .flat_map(|s| s.words.iter())
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| NewWord {
            start_ms: w.start_ms as i64,
            end_ms: w.end_ms as i64,
            text: w.text.clone(),
            prob: w.prob,
        })
        .collect()
}

/// For each word, the speaker whose turn overlaps it most (or the nearest turn within
/// [`NEAREST_TURN_MS`]); `None` when no turn is close.
pub fn assign(words: &[NewWord], turns: &[SpeakerTurn]) -> Vec<Option<u32>> {
    words
        .iter()
        .map(|w| {
            let (ws, we) = (w.start_ms, w.end_ms.max(w.start_ms + 1));
            let mut best: Option<(i64, u32)> = None; // (overlap, speaker)
            let mut nearest: Option<(i64, u32)> = None; // (distance, speaker)
            for t in turns {
                let (ts, te) = (t.start_ms as i64, t.end_ms as i64);
                let overlap = we.min(te) - ws.max(ts);
                if overlap > 0 {
                    if best.is_none_or(|(o, _)| overlap > o) {
                        best = Some((overlap, t.speaker));
                    }
                } else {
                    let dist = (ts - we).max(ws - te);
                    if nearest.is_none_or(|(d, _)| dist < d) {
                        nearest = Some((dist, t.speaker));
                    }
                }
            }
            best.map(|(_, s)| s).or_else(|| {
                nearest
                    .filter(|(d, _)| *d <= NEAREST_TURN_MS)
                    .map(|(_, s)| s)
            })
        })
        .collect()
}

/// Fills unknown labels from the neighbours, then removes isolated one-word flips
/// (A A B A A -> A A A A A), which are almost always alignment noise.
pub fn smooth(labels: &mut [Option<u32>]) {
    // Unknown -> previous known (or the next known at the start).
    let mut last = None;
    for l in labels.iter_mut() {
        match l {
            Some(_) => last = *l,
            None => *l = last,
        }
    }
    let first = labels.iter().find_map(|l| *l);
    for l in labels.iter_mut() {
        if l.is_none() {
            *l = first;
        }
    }
    // Single-word runs between two runs of the same other speaker.
    for i in 1..labels.len().saturating_sub(1) {
        if labels[i - 1] == labels[i + 1] && labels[i] != labels[i - 1] {
            labels[i] = labels[i - 1];
        }
    }
}

/// Groups words into utterances, breaking on a speaker change or a long pause.
pub fn group(words: &[NewWord], labels: &[Option<u32>]) -> Vec<(Option<u32>, NewUtterance)> {
    let mut out: Vec<(Option<u32>, NewUtterance)> = Vec::new();
    let mut prev_end: Option<i64> = None;
    for (w, &label) in words.iter().zip(labels) {
        let new = match (prev_end, out.last()) {
            (Some(end), Some((l, _))) => *l != label || w.start_ms - end > UTTERANCE_PAUSE_MS,
            _ => true,
        };
        if new {
            out.push((
                label,
                NewUtterance {
                    start_ms: w.start_ms,
                    ..Default::default()
                },
            ));
        }
        let (_, u) = out.last_mut().expect("pushed");
        u.end_ms = w.end_ms;
        u.text.push_str(&w.text);
        u.words.push(w.clone());
        prev_end = Some(w.end_ms);
    }
    for (_, u) in &mut out {
        u.text = u.text.trim().to_owned();
    }
    out
}

/// Renumbers speakers 0, 1, 2… in order of first appearance; returns how many there are.
pub fn renumber(utterances: &mut [(Option<u32>, NewUtterance)]) -> usize {
    let mut order: Vec<u32> = Vec::new();
    for (l, _) in utterances.iter_mut() {
        if let Some(s) = l {
            let idx = order.iter().position(|o| o == s).unwrap_or_else(|| {
                order.push(*s);
                order.len() - 1
            });
            *s = idx as u32;
        }
    }
    order.len()
}

/// A speaker from before a re-run: the name the user gave it and when it spoke.
pub struct OldSpeaker {
    pub name: String,
    pub spans: Vec<(i64, i64)>,
}

/// Carries user-given names over to the speakers of a re-run. A new speaker inherits an old
/// speaker's name only when each is the other's best match by overlapping speech, and the match
/// covers at least half of the new speaker's talk time; anything less clean stays "Speaker N".
/// `new` is (0-based new speaker index, start ms, end ms) for each new utterance.
pub fn carry_names(
    old: &[OldSpeaker],
    new: &[(Option<usize>, i64, i64)],
    new_count: usize,
) -> Vec<Option<String>> {
    let mut overlap = vec![vec![0i64; old.len()]; new_count];
    let mut talk = vec![0i64; new_count];
    for &(label, start, end) in new {
        let Some(i) = label.filter(|&i| i < new_count) else {
            continue;
        };
        talk[i] += end - start;
        for (j, o) in old.iter().enumerate() {
            for &(s, e) in &o.spans {
                overlap[i][j] += (end.min(e) - start.max(s)).max(0);
            }
        }
    }
    let best_old = |i: usize| {
        (0..old.len())
            .max_by_key(|&j| overlap[i][j])
            .filter(|&j| overlap[i][j] > 0)
    };
    let best_new = |j: usize| {
        (0..new_count)
            .max_by_key(|&i| overlap[i][j])
            .filter(|&i| overlap[i][j] > 0)
    };
    (0..new_count)
        .map(|i| {
            let j = best_old(i)?;
            (best_new(j) == Some(i) && overlap[i][j] * 2 >= talk[i]).then(|| old[j].name.clone())
        })
        .collect()
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// Length of the longest common subsequence of two token lists.
fn lcs(a: &[String], b: &[String]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Removes mic utterances that repeat what the meeting track says at the same time — remote
/// voices picked up by the mic when the user isn't on headphones. Returns how many were removed.
pub fn drop_bleed(mic: &mut Vec<NewUtterance>, system_words: &[NewWord]) -> usize {
    let before = mic.len();
    mic.retain(|u| {
        let mine = tokens(&u.text);
        if mine.is_empty() {
            return true;
        }
        let theirs: Vec<String> = system_words
            .iter()
            .filter(|w| {
                w.end_ms >= u.start_ms - BLEED_WINDOW_MS && w.start_ms <= u.end_ms + BLEED_WINDOW_MS
            })
            .flat_map(|w| tokens(&w.text))
            .collect();
        if theirs.is_empty() {
            return true;
        }
        (lcs(&mine, &theirs) as f32 / mine.len() as f32) < BLEED_MATCH_RATIO
    });
    before - mic.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, s: i64, e: i64) -> NewWord {
        NewWord {
            start_ms: s,
            end_ms: e,
            text: format!(" {text}"),
            prob: 0.9,
        }
    }

    fn turn(s: u64, e: u64, speaker: u32) -> SpeakerTurn {
        SpeakerTurn {
            start_ms: s,
            end_ms: e,
            speaker,
        }
    }

    #[test]
    fn words_go_to_the_turn_they_overlap_most() {
        let words = [
            w("a", 0, 400),
            w("b", 900, 1300),
            w("c", 5000, 5200),
            w("d", 9000, 9100),
        ];
        let turns = [turn(0, 1000, 3), turn(1000, 4000, 5)];
        // "b" overlaps 3 by 100 ms and 5 by 300 ms; "c" is 1 s past turn 5's end;
        // "d" is too far from any turn.
        assert_eq!(assign(&words, &turns), [Some(3), Some(5), Some(5), None]);
    }

    #[test]
    fn smoothing_fills_gaps_and_removes_single_word_flips() {
        let mut l = [
            None,
            Some(1),
            Some(1),
            Some(2),
            Some(1),
            None,
            Some(2),
            Some(2),
        ];
        smooth(&mut l);
        assert_eq!(
            l,
            [
                Some(1),
                Some(1),
                Some(1),
                Some(1),
                Some(1),
                Some(1),
                Some(2),
                Some(2)
            ]
        );
        // A two-word run is kept: it's likely a real interjection.
        let mut l = [Some(0), Some(1), Some(1), Some(0)];
        smooth(&mut l);
        assert_eq!(l, [Some(0), Some(1), Some(1), Some(0)]);
    }

    #[test]
    fn grouping_breaks_on_speaker_change_and_pause() {
        let words = [
            w("Hi", 0, 300),
            w("there.", 300, 600),
            w("Hello.", 700, 1000),
            w("Later.", 5000, 5300),
        ];
        let labels = [Some(0), Some(0), Some(1), Some(1)];
        let u = group(&words, &labels);
        let summary: Vec<_> = u
            .iter()
            .map(|(l, u)| (*l, u.text.as_str(), u.start_ms, u.end_ms))
            .collect();
        assert_eq!(
            summary,
            [
                (Some(0), "Hi there.", 0, 600),
                (Some(1), "Hello.", 700, 1000),
                (Some(1), "Later.", 5000, 5300)
            ]
        );
    }

    #[test]
    fn speakers_are_numbered_by_first_appearance() {
        let words = [w("a", 0, 100), w("b", 2000, 2100), w("c", 4000, 4100)];
        let mut u = group(&words, &[Some(7), Some(2), Some(7)]);
        assert_eq!(renumber(&mut u), 2);
        assert_eq!(
            u.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            [Some(0), Some(1), Some(0)]
        );
    }

    #[test]
    fn names_carry_over_only_on_clean_matches() {
        let old = [
            OldSpeaker {
                name: "Dana".into(),
                spans: vec![(0, 10_000), (30_000, 40_000)],
            },
            OldSpeaker {
                name: "Lee".into(),
                spans: vec![(10_000, 20_000)],
            },
        ];
        // New run: cluster 1 is Dana's speech, cluster 0 is Lee's, cluster 2 is a new voice
        // that only grazes Dana's time.
        let new = [
            (Some(1), 0, 10_000),
            (Some(0), 10_000, 20_000),
            (Some(1), 30_000, 40_000),
            (Some(2), 39_000, 50_000),
        ];
        assert_eq!(
            carry_names(&old, &new, 3),
            [Some("Lee".to_owned()), Some("Dana".to_owned()), None]
        );
        // Two old speakers merged into one new cluster: only the bigger name carries over.
        let merged = [(Some(0), 0, 20_000), (Some(0), 30_000, 40_000)];
        assert_eq!(carry_names(&old, &merged, 1), [Some("Dana".to_owned())]);
    }

    #[test]
    fn bleed_is_removed_but_real_replies_stay() {
        let system = [
            w("The", 1000, 1200),
            w("budget", 1200, 1600),
            w("is", 1600, 1700),
            w("due", 1700, 1900),
            w("Friday.", 1900, 2400),
        ];
        let mut mic = group(
            &[
                // Echo of the meeting track, ~150 ms late and one word missed.
                w("the", 1150, 1350),
                w("budget", 1350, 1750),
                w("due", 1850, 2050),
                w("Friday", 2050, 2550),
                // The user's own reply.
                w("Sounds", 4000, 4300),
                w("good,", 4300, 4600),
                w("thanks.", 4600, 5000),
            ],
            &[Some(0); 7],
        )
        .into_iter()
        .map(|(_, u)| u)
        .collect::<Vec<_>>();
        assert_eq!(mic.len(), 2);
        assert_eq!(drop_bleed(&mut mic, &system), 1);
        assert_eq!(mic[0].text, "Sounds good, thanks.");
    }
}
