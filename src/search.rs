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

/// A search as a tree: terms (`exif:canon`, `re:png$`, a fuzzy name) combined with and (one
/// after the other), or (`|`) and not (`-` before a prefixed term, or `-(` before a group).
#[derive(Debug, PartialEq)]
pub enum Query {
    Term(String),
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
}

/// The tree of a search; `None` for an empty one. A term starts at a prefix (`exif:`, `re:`,
/// `mark:`, `stars:`) and the words after it belong to it, so `exif:canon 2024:05` stays one
/// term; words before the first prefix of a group search the names. `|` binds loosest, and
/// `(`, `-(` and `)` group, all as words of their own, so a regular expression keeps its own
/// parentheses and bars (`re:^(IMG|DSC)`). A `)` too many is left out, a missing one assumed.
pub fn parse(query: &str) -> Option<Query> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut at = 0;
    let parsed = or(&tokens, &mut at);
    // A stray ")" ends a group that was not opened: the rest is read on.
    let mut parts: Vec<Query> = parsed.into_iter().collect();
    while at < tokens.len() {
        at += 1;
        parts.extend(or(&tokens, &mut at));
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Query::And(parts)),
    }
}

const PREFIXES: [&str; 4] = ["exif:", "re:", "mark:", "stars:"];

fn prefixed(t: &str) -> bool {
    PREFIXES.iter().any(|p| t.starts_with(p))
}

/// Groups joined by `|`, up to a `)` or the end.
fn or(tokens: &[&str], at: &mut usize) -> Option<Query> {
    let mut parts: Vec<Query> = Vec::new();
    loop {
        parts.extend(and(tokens, at));
        match tokens.get(*at) {
            Some(&"|") => *at += 1,
            _ => break,
        }
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Query::Or(parts)),
    }
}

/// Terms and groups one after the other, up to a `|`, a `)` or the end.
fn and(tokens: &[&str], at: &mut usize) -> Option<Query> {
    let mut parts: Vec<Query> = Vec::new();
    // Words after a term belong to it; after a group they start a search of the names.
    let mut open_term = false;
    while let Some(&token) = tokens.get(*at) {
        match token {
            "|" | ")" => break,
            "(" | "-(" => {
                *at += 1;
                let inner = or(tokens, at);
                if tokens.get(*at) == Some(&")") {
                    *at += 1;
                }
                if let Some(inner) = inner {
                    parts.push(match token {
                        "-(" => Query::Not(Box::new(inner)),
                        _ => inner,
                    });
                }
                open_term = false;
            }
            _ => {
                *at += 1;
                match token.strip_prefix('-').filter(|rest| prefixed(rest)) {
                    Some(rest) => {
                        parts.push(Query::Not(Box::new(Query::Term(rest.to_owned()))));
                        open_term = true;
                    }
                    None if open_term && !prefixed(token) => {
                        let last = match parts.last_mut() {
                            Some(Query::Not(inner)) => inner.as_mut(),
                            Some(last) => last,
                            None => unreachable!("a term is open"),
                        };
                        if let Query::Term(text) = last {
                            text.push(' ');
                            text.push_str(token);
                        }
                    }
                    None => {
                        parts.push(Query::Term(token.to_owned()));
                        open_term = true;
                    }
                }
            }
        }
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Query::And(parts)),
    }
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
    crate::in_parts(names, crate::background_threads().min(8), |part, at| {
        let mut buf = Vec::new();
        (part.iter().enumerate())
            .filter_map(|(i, n)| Some((score_one(n, &mut buf)?, at + i)))
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

    /// A search read as a tree: prefixes start terms and words after one belong to it, `-`
    /// negates a prefixed term or a group, `|` binds loosest, `(` `)` group as words of their
    /// own, and a regular expression keeps its parentheses and bars.
    #[test]
    fn parses_searches() {
        use super::Query::{And, Not, Or, Term};
        let t = |s: &str| Term(s.to_owned());
        let not = |q| Not(Box::new(q));
        let p = |q: &str| super::parse(q);
        assert_eq!(p("exif:canon 2024:05"), Some(t("exif:canon 2024:05")));
        assert_eq!(
            p("sunset re:^IMG -exif:iphone stars:3"),
            Some(And(vec![
                t("sunset"),
                t("re:^IMG"),
                not(t("exif:iphone")),
                t("stars:3")
            ]))
        );
        assert_eq!(
            p("sunset ( exif:canon | exif:nikon 2024 ) -( re:png$ | stars:1 )"),
            Some(And(vec![
                t("sunset"),
                Or(vec![t("exif:canon"), t("exif:nikon 2024")]),
                not(Or(vec![t("re:png$"), t("stars:1")])),
            ]))
        );
        assert_eq!(p("a b | c"), Some(Or(vec![t("a b"), t("c")])));
        // Its own parentheses and bars stay in a regular expression.
        assert_eq!(p("re:^(IMG|DSC)_"), Some(t("re:^(IMG|DSC)_")));
        // A name with a dash is a name; a missing ")" is assumed, one too many left out.
        assert_eq!(p("-draft"), Some(t("-draft")));
        assert_eq!(p("( mark:2"), Some(t("mark:2")));
        assert_eq!(
            p("mark:2 ) stars:3"),
            Some(And(vec![t("mark:2"), t("stars:3")]))
        );
        assert_eq!(p("| ( ) |"), None);
        assert_eq!(p("  "), None);
    }

    /// Searches broken in thousands of ways: read without a panic or an endless loop.
    #[test]
    fn survives_broken_searches() {
        let seed = b"sunset ( exif:canon | -exif:nikon 2024 ) -( re:^(a|b) | stars:1 ) mark:2";
        for bytes in crate::broken(seed) {
            super::parse(&String::from_utf8_lossy(&bytes));
        }
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
