//! Voice profiles: recognising a saved person's voice in new meetings by comparing speaker
//! embeddings. Pure functions; storage lives in `db`.

/// Default similarity a speaker must reach to be named from a profile automatically. Calibrated on
/// AMI with CAM++: the same person in another meeting scored 0.86–0.88, different people at most
/// 0.43 (for speakers with enough clean speech).
pub const DEFAULT_MATCH_THRESHOLD: f32 = 0.6;
/// Speakers with less clean speech than this aren't matched: their embeddings are unreliable.
pub const MIN_MATCH_SPEECH_MS: u64 = 10_000;
/// Samples kept per profile and voice model (newest win).
pub const MAX_SAMPLES: usize = 10;

pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// The normalized mean of a profile's samples.
pub fn centroid(samples: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dim = samples.first()?.len();
    let mut sum = vec![0.0f32; dim];
    for s in samples.iter().filter(|s| s.len() == dim) {
        sum.iter_mut().zip(s).for_each(|(a, b)| *a += b);
    }
    Some(normalize(sum))
}

pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return -1.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub struct Candidate<'a> {
    pub embedding: &'a [f32],
    pub speech_ms: u64,
}

/// Matches meeting speakers to profiles: best pairs first, each profile and each speaker used at
/// most once, only above `threshold` and for speakers with enough speech. Returns, per speaker,
/// the matched profile id and score.
pub fn match_profiles(
    speakers: &[Candidate<'_>],
    profiles: &[(i64, Vec<f32>)],
    threshold: f32,
) -> Vec<Option<(i64, f32)>> {
    let mut pairs = Vec::new();
    for (i, s) in speakers.iter().enumerate() {
        if s.speech_ms < MIN_MATCH_SPEECH_MS {
            continue;
        }
        for (j, (_, c)) in profiles.iter().enumerate() {
            let score = similarity(s.embedding, c);
            if score >= threshold {
                pairs.push((score, i, j));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut out = vec![None; speakers.len()];
    let mut used = vec![false; profiles.len()];
    for (score, i, j) in pairs {
        if out[i].is_none() && !used[j] {
            out[i] = Some((profiles[j].0, score));
            used[j] = true;
        }
    }
    out
}

/// Embeddings are stored as little-endian f32 bytes.
pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        normalize(v.to_vec())
    }

    #[test]
    fn best_pairs_win_and_each_profile_is_used_once() {
        let dana = unit(&[1.0, 0.0, 0.0]);
        let lee = unit(&[0.0, 1.0, 0.0]);
        let profiles = vec![(10, dana.clone()), (20, lee.clone())];
        let a = unit(&[0.9, 0.1, 0.0]); // clearly Dana
        let b = unit(&[0.8, 0.0, 0.3]); // also Dana-like, but less so
        let c = unit(&[0.0, 0.0, 1.0]); // nobody
        let short = unit(&[0.0, 1.0, 0.0]); // Lee, but too little speech
        let speakers = [
            Candidate {
                embedding: &a,
                speech_ms: 30_000,
            },
            Candidate {
                embedding: &b,
                speech_ms: 30_000,
            },
            Candidate {
                embedding: &c,
                speech_ms: 30_000,
            },
            Candidate {
                embedding: &short,
                speech_ms: 4_000,
            },
        ];
        let m = match_profiles(&speakers, &profiles, 0.6);
        assert_eq!(m[0].map(|x| x.0), Some(10));
        assert_eq!(m[1], None, "Dana is already taken by a better match");
        assert_eq!(m[2], None);
        assert_eq!(m[3], None, "too little speech to trust");
    }

    #[test]
    fn centroid_and_blobs_round_trip() {
        let c = centroid(&[unit(&[1.0, 0.0]), unit(&[0.0, 1.0])]).unwrap();
        assert!((similarity(&c, &unit(&[1.0, 1.0])) - 1.0).abs() < 1e-6);
        let v = vec![0.25f32, -1.5, 3.0];
        assert_eq!(from_blob(&to_blob(&v)), v);
        assert!(centroid(&[]).is_none());
    }
}
