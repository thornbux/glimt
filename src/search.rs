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

/// A `re:` search, ignoring case unless `case`; for a pattern that is not one, why, to show
/// while typing.
pub fn regex(pattern: &str, case: bool) -> Result<regex::bytes::Regex, String> {
    regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(!case)
        .build()
        .map_err(|e| {
            // The last line says what is wrong; the ones before point at where.
            let text = e.to_string();
            let last = text.lines().last().unwrap_or_default();
            last.trim_start_matches("error: ").to_owned()
        })
}

/// Indices of the names `re` finds, in list order, split over the cores like `matches`.
pub fn regex_matches(names: &[std::ffi::OsString], re: &regex::bytes::Regex) -> Vec<usize> {
    let hits = in_parallel(names, |name, _| {
        re.is_match(name.as_encoded_bytes()).then_some(0)
    });
    hits.into_iter().map(|h| h.1).collect()
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
    let mut hits = in_parallel(names, score_one);
    hits.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|h| h.1).collect()
}

/// (score, index) of the names `score_one` matches, in list order, split over the cores; each
/// thread lends it a buffer of its own.
fn in_parallel(
    names: &[std::ffi::OsString],
    score_one: impl Fn(&std::ffi::OsString, &mut Vec<u8>) -> Option<i32> + Sync,
) -> Vec<(i32, usize)> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let chunk = names.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
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
    })
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

    /// re: finds names by a regular expression, ignoring case, in list order; a broken one
    /// says why in one line.
    #[test]
    fn finds_by_regex() {
        let names: Vec<std::ffi::OsString> =
            ["IMG_0012.JPG", "img_0013.png", "Blåbær 7.jpg", "notes.txt"]
                .iter()
                .map(Into::into)
                .collect();
        let find = |p| super::regex_matches(&names, &super::regex(p, false).unwrap());
        assert_eq!(find(r"^img_\d+\.jpe?g$"), [0]);
        assert_eq!(find(r"\.(jpg|png)$"), [0, 1, 2]);
        assert_eq!(find("blåbær \\d"), [2]);
        assert!(find("^x").is_empty());
        assert_eq!(super::regex("(", false).unwrap_err(), "unclosed group");
        // Case, as Geeqie's checkbox.
        let re = super::regex("^img", true).unwrap();
        assert_eq!(super::regex_matches(&names, &re), [1]);
    }
}
