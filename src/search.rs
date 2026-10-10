//! Fuzzy file name search in the style of fzf: the letters of the query must appear in the
//! name in order, but not next to each other, and case is ignored. Names where they come
//! together, at the start of a word or early in the name rank first.

/// Score of the lowercase `name` for the lowercase `query`, higher is better; `None` if it
/// does not match. Generic over bytes (ASCII names, no allocation) and chars (the rest).
fn score<T: Copy + PartialEq>(name: &[T], query: &[T], word: impl Fn(T) -> bool) -> Option<i32> {
    // A plain substring is what people usually type; it always beats a scattered match.
    if let Some(start) = name.windows(query.len()).position(|w| w == query) {
        let at_word = start == 0 || !word(name[start - 1]);
        return Some(1000 + 50 * at_word as i32 - start.min(100) as i32);
    }
    let (mut score, mut q, mut last) = (0, 0, None::<usize>);
    for (i, &c) in name.iter().enumerate() {
        if q == query.len() {
            break;
        }
        if c != query[q] {
            continue;
        }
        score += 16;
        match last {
            Some(l) if l + 1 == i => score += 15,
            Some(l) => score -= ((i - l - 1) as i32).min(10),
            None => score -= (i as i32).min(20),
        }
        if i == 0 || !word(name[i - 1]) {
            score += 10;
        }
        (q, last) = (q + 1, Some(i));
    }
    (q == query.len()).then_some(score)
}

/// Indices of the matching names, best first, then in list order. Split over the cores:
/// 300,000 names take a few milliseconds.
pub fn matches(names: &[std::ffi::OsString], query: &str) -> Vec<usize> {
    let query_chars: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let query_bytes = query
        .is_ascii()
        .then(|| query.to_ascii_lowercase().into_bytes());
    let score_one = |name: &std::ffi::OsString, buf: &mut Vec<u8>| {
        let bytes = name.as_encoded_bytes();
        match &query_bytes {
            Some(q) if bytes.is_ascii() => {
                buf.clear();
                buf.extend(bytes.iter().map(u8::to_ascii_lowercase));
                score(buf, q, |b: u8| b.is_ascii_alphanumeric())
            }
            _ => {
                let lower: Vec<char> = name
                    .to_string_lossy()
                    .chars()
                    .flat_map(char::to_lowercase)
                    .collect();
                score(&lower, &query_chars, char::is_alphanumeric)
            }
        }
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let chunk = names.len().div_ceil(threads).max(1);
    let mut hits: Vec<(i32, usize)> = std::thread::scope(|s| {
        let parts: Vec<_> = names
            .chunks(chunk)
            .enumerate()
            .map(|(c, part)| {
                let score_one = &score_one;
                s.spawn(move || {
                    let mut buf = Vec::new();
                    part.iter()
                        .enumerate()
                        .filter_map(|(i, n)| Some((score_one(n, &mut buf)?, c * chunk + i)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|p| p.join().unwrap_or_default())
            .collect()
    });
    hits.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|h| h.1).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_and_ranks() {
        let names: Vec<std::ffi::OsString> = [
            "Screenshot From 2026-04-10.png",
            "holiday_beach.jpg",
            "BEACH-sunset.JPG",
            "b_e_a_c_h.png",
            "other.png",
        ]
        .iter()
        .map(Into::into)
        .collect();
        // Case is ignored; a word-start substring ranks above one inside a word, and a
        // scattered match comes last.
        assert_eq!(super::matches(&names, "beach"), [2, 1, 3]);
        assert_eq!(super::matches(&names, "shot"), [0]);
        assert_eq!(super::matches(&names, "sfr"), [0]);
        assert!(super::matches(&names, "xyz").is_empty());
        // Names and queries outside ASCII take the char path.
        let names: Vec<std::ffi::OsString> = ["Blåbær.jpg", "BLÅBÆR2.jpg"]
            .iter()
            .map(Into::into)
            .collect();
        assert_eq!(super::matches(&names, "BÆR"), [0, 1]);
    }
}
