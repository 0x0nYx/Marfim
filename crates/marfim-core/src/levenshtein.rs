//! Levenshtein edit distance (used for config did-you-mean).

pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Best "did you mean" candidate within edit distance 2, else None.
pub fn did_you_mean<'a>(input: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = distance(input, c);
        if d <= 2 && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_basics() {
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("model", "modle"), 2);
        assert_eq!(distance("abc", "abc"), 0);
    }

    #[test]
    fn did_you_mean_suggests_close_key() {
        assert_eq!(did_you_mean("modle", &["model", "provider"]), Some("model"));
        assert_eq!(did_you_mean("zzzzzz", &["model", "provider"]), None);
    }
}
