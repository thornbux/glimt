//! Fuzzy file name search in the style of fzf: the letters of the query must appear in the
//! name in order, but not next to each other, and case is ignored. Names where they come
//! together, at the start of a word or early in the name rank first. Also the EXIF index that
//! `exif:` terms search: each file's fields and keywords as one line.

use crate::Receiving;
use crate::Took;
use crate::Working;
use crate::background_threads;
use crate::in_parts;
use crate::noted;
use crate::send_sizes;
use crate::tf;
use crate::theme;
use crate::tr;
use crate::{exif, meta, small_alloc_threads};
use eframe::egui;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

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
    crate::threads::in_parts(
        names,
        crate::threads::background_threads().min(8),
        |part, at| {
            let mut buf = Vec::new();
            (part.iter().enumerate())
                .filter_map(|(i, n)| Some((score_one(n, &mut buf)?, at + i)))
                .collect()
        },
    )
}

/// The EXIF fields of every file as one lowercase line, read in parallel (small_alloc_threads).
pub fn exif_index(
    dir: &Path,
    files: &[OsString],
    stop: &std::sync::atomic::AtomicBool,
) -> HashMap<OsString, String> {
    let index = crate::threads::in_parts(files, small_alloc_threads(), |part, _| {
        let mut out = Vec::with_capacity(part.len());
        for name in part {
            // Left: the folder is gone from the window, and no one waits for this.
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let path = dir.join(name);
            // A file that breaks its reader has no text; a whole part lost would leave 1/N of
            // the folder out of every exif: search, in silence.
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let rows = exif::read(&path);
                let mut text: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
                let meta = meta::read(&path).text();
                text.push(&meta);
                text.join(" ").to_lowercase()
            }));
            out.push((name.clone(), text.unwrap_or_default()));
        }
        out
    });
    index.into_iter().collect()
}

impl crate::App {
    // ---- The search ----

    /// Fuzzy search box above the file list.
    pub(crate) fn search_field(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if !self.search.is_empty() && ui.small_button(theme::icon::CLOSE).clicked() {
                self.search.clear();
                self.update_search(ui.ctx());
                self.scroll_to_cur = true;
            }
            // A fixed ID: the X button appearing in front of it must not make it a new
            // widget, which would lose the focus after the first letter.
            let hint = match self.config.fuzzy_search {
                true => tr("Search file names (3+ letters), re: pattern, or exif: camera, date …"),
                false => tr("Filter file names by pattern, or exif: camera, date …"),
            };
            let edit = egui::TextEdit::singleline(&mut self.search)
                .id(egui::Id::unique("file search"))
                .hint_text(hint);
            // Geeqie's file filter has its Case checkbox to the right; before it, the switch
            // between fuzzy and regular expression search. Laid out from the right, so the
            // field takes what is left: a row wider than the panel would widen it every frame.
            let geeqie = !self.config.fuzzy_search;
            let mut changed = false;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if geeqie
                    && ui
                        .checkbox(&mut self.config.search_case, tr("Case"))
                        .on_hover_text(tr("Tell upper and lower case apart"))
                        .changed()
                {
                    self.config.save();
                    changed = true;
                }
                if noted(&ui.selectable_label(geeqie, ".*"), ".*")
                    .on_hover_text(tr("Regular expression instead of fuzzy search"))
                    .clicked()
                {
                    self.config.fuzzy_search = geeqie;
                    self.config.save();
                    changed = true;
                }
                let field = ui.add(edit.desired_width(ui.available_width()));
                if std::mem::take(&mut self.focus_search) {
                    field.request_focus();
                }
                changed |= field.changed();
            });
            if changed {
                self.update_search(ui.ctx());
            }
        });
        if let Some(e) = &self.search_error {
            ui.colored_label(ui.visuals().error_fg_color, tf!("Not a pattern: {e}", e));
        }
    }

    /// Run the search again; file indices change when the list does.
    pub(crate) fn update_search(&mut self, ctx: &egui::Context) {
        let query = self.search.trim().to_owned();
        let query = query.as_str();
        self.match_rows.set(Default::default());
        // Other rows: a Shift range of the keys is no longer the rows it was.
        self.list.range_end = None;
        // A place "[n/m]" told for another pane: another search may have as many matches.
        self.status_place = None;
        self.search_error = None;
        self.list.matches = self.search_matches(ctx, query);
        // Size of the matches in the background; a new search drops the receiver, which
        // stops the old sum.
        self.list.match_size = 0;
        self.match_rx = Receiving::none();
        // Not at each letter typed: each would copy every match's name, 500,000 for an "a".
        let wait = std::time::Duration::from_millis(200);
        self.match_size_due = self.list.matches.is_some().then(|| Instant::now() + wait);
        ctx.request_repaint_after(wait);
    }

    /// The size of the matches, in the background, once the search has stayed the same a moment.
    pub(crate) fn match_size(&mut self, ctx: &egui::Context) {
        let Some(due) = self.match_size_due else {
            return;
        };
        // Asked again each frame: a frame drawn before it is due drops the earlier request.
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            return ctx.request_repaint_after(wait);
        }
        self.match_size_due = None;
        let Some(m) = &self.list.matches else { return };
        let names: Vec<OsString> = m.iter().map(|&i| self.list.files[i].clone()).collect();
        let (tx, rx) = mpsc::channel();
        let (dir, ctx) = (self.dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            let sizes = names
                .iter()
                .filter_map(|n| Some(dir.join(n).metadata().ok()?.len()));
            send_sizes(sizes, tx, &ctx);
        });
        self.match_rx = Receiving::of(rx);
    }

    /// The exif: search index once it is read, and the search again with it.
    pub(crate) fn receive_exif_index(&mut self, ctx: &egui::Context) {
        let index = match self.exif_index_rx.take() {
            Took::Nothing => return,
            Took::Answer(index) => index,
            // Without any index the search would start reading again at once: an empty one,
            // and the user told.
            Took::Gone => {
                self.stopped(tr("The EXIF index"));
                HashMap::new()
            }
        };
        // The files read again go into the index there is; else this is the whole index.
        match &mut self.exif_index {
            Some(old) => old.extend(index),
            None => self.exif_index = Some(index),
        }
        // Read from the list as it was: names that came since are read too.
        self.exif_follow_list();
        self.update_search(ctx);
    }

    /// Names in the list the `exif:` index has not read (renamed, moved or copied in, new in a
    /// folder read again, from subfolders turned on) are read with the next search, as edited
    /// ones are: a name it lacked was found by no `exif:` search at all.
    pub(crate) fn exif_follow_list(&mut self) {
        if let Some(index) = &self.exif_index {
            let missing = self.list.files.iter().filter(|n| !index.contains_key(*n));
            self.exif_stale.extend(missing.cloned());
        }
    }

    /// The files a search finds: one term as it always was (fuzzy names best first), or terms
    /// combined (`sunset ( exif:canon | exif:nikon ) -re:png$`, see `parse`) in list
    /// order; `None` for no search.
    pub(crate) fn search_matches(
        &mut self,
        ctx: &egui::Context,
        query: &str,
    ) -> Option<Vec<usize>> {
        match parse(query)? {
            Query::Term(term) => self.term_matches(ctx, &term, 3),
            tree => Some(self.query_matches(ctx, &tree)),
        }
    }

    /// The files a part of a combined search finds, in order: an and keeps the order of its
    /// first part that is no "not", an or the order of its parts one after the other.
    pub(crate) fn query_matches(&mut self, ctx: &egui::Context, query: &Query) -> Vec<usize> {
        use Query;
        let n = self.list.files.len();
        let mask = |hits: &[usize]| {
            let mut mask = vec![false; n];
            hits.iter().for_each(|&i| mask[i] = true);
            mask
        };
        match query {
            Query::Term(term) => self.term_matches(ctx, term, 1).unwrap_or_default(),
            Query::Not(inner) => {
                let out = mask(&self.query_matches(ctx, inner));
                (0..n).filter(|&i| !out[i]).collect()
            }
            Query::Or(parts) => {
                let mut seen = vec![false; n];
                let mut found = Vec::new();
                for part in parts {
                    for i in self.query_matches(ctx, part) {
                        if !std::mem::replace(&mut seen[i], true) {
                            found.push(i);
                        }
                    }
                }
                found
            }
            Query::And(parts) => {
                let lead = parts.iter().position(|p| !matches!(p, Query::Not(_)));
                let mut found: Vec<usize> = match lead {
                    Some(at) => self.query_matches(ctx, &parts[at]),
                    None => (0..n).collect(),
                };
                for (at, part) in parts.iter().enumerate() {
                    if Some(at) == lead {
                        continue;
                    }
                    // A "not" left out directly; the others kept where both find.
                    let (inner, keep) = match part {
                        Query::Not(inner) => (inner.as_ref(), false),
                        part => (part, true),
                    };
                    let hit = mask(&self.query_matches(ctx, inner));
                    found.retain(|&i| hit[i] == keep);
                }
                found
            }
        }
    }

    /// The files one term of a search finds: `exif:`, `re:`, `mark:`, `stars:`, or the names
    /// (fuzzy from `min` letters, or a regular expression with fuzzy search off).
    pub(crate) fn term_matches(
        &mut self,
        ctx: &egui::Context,
        term: &str,
        min: usize,
    ) -> Option<Vec<usize>> {
        // Geeqie's Case checkbox, shown beside the field in its mode.
        let case = self.config.search_case && !self.config.fuzzy_search;
        let regex = |this: &mut Self, pattern: &str| match regex(pattern, case) {
            Ok(re) => regex_matches(&this.list.files, &re),
            Err(e) => {
                this.search_error = Some(e);
                Vec::new()
            }
        };
        match (term.strip_prefix("exif:"), term.strip_prefix("re:")) {
            (Some(words), _) => Some(self.exif_matches(ctx, words)),
            (_, Some(pattern)) if !pattern.is_empty() => Some(regex(self, pattern)),
            _ => {
                self.mark_matches(term)
                    .or_else(|| match self.config.fuzzy_search {
                        true => {
                            (term.chars().count() >= min).then(|| matches(&self.list.files, term))
                        }
                        // As Geeqie's file filter: a regular expression, from the first letter.
                        false => (!term.is_empty()).then(|| regex(self, term)),
                    })
            }
        }
    }

    /// Files whose EXIF contains every word, or that a regular expression after "re:" finds, in
    /// list order. The first such search in a folder reads the EXIF of all files in the
    /// background (the busy pointer shows meanwhile) and finds nothing until that is done.
    /// shortcut: files added after the index was built do not match until the folder is
    /// reopened; add them to the index as the list grows if that is missed.
    pub(crate) fn exif_matches(&mut self, ctx: &egui::Context, words: &str) -> Vec<usize> {
        let re = match words.strip_prefix("re:").map(|p| regex(p, false)) {
            Some(Err(e)) => {
                self.search_error = Some(e);
                return Vec::new();
            }
            Some(Ok(re)) => Some(re),
            None => None,
        };
        // Files edited since: read again alone, while the search goes on with the index there is.
        // Not while keywords are still being written: the marks stay until they are, so files
        // read before their sidecar landed are read again after (the frame that sees the job
        // end searches again).
        let writing = (self.file_jobs.iter()).any(|j| j.sidecars && Arc::strong_count(j) > 1);
        if !writing
            && self.exif_index.is_some()
            && !self.exif_stale.is_empty()
            && self.exif_index_rx.is_none()
        {
            let (tx, rx) = mpsc::channel();
            let names: Vec<OsString> = self.exif_stale.drain().collect();
            let (dir, ctx) = (self.dir.clone(), ctx.clone());
            let pointer = Working::new(&self.busy);
            self.exif_index_stop = Arc::default();
            let stop = self.exif_index_stop.clone();
            std::thread::spawn(move || {
                let _ = tx.send(exif_index(&dir, &names, &stop));
                drop(pointer);
                ctx.request_repaint();
            });
            self.exif_index_rx = Receiving::of(rx);
        }
        let Some(index) = &self.exif_index else {
            if self.exif_index_rx.is_none() {
                // Marks made while keywords are written are kept for after this first index.
                if !writing {
                    self.exif_stale.clear();
                }
                let (tx, rx) = mpsc::channel();
                let (dir, files, ctx) = (self.dir.clone(), self.list.files.clone(), ctx.clone());
                let pointer = Working::new(&self.busy);
                self.exif_index_stop = Arc::default();
                let stop = self.exif_index_stop.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(exif_index(&dir, &files, &stop));
                    drop(pointer);
                    ctx.request_repaint();
                });
                self.exif_index_rx = Receiving::of(rx);
            }
            return Vec::new();
        };
        // The index is lowercase, as the words are made; a regular expression ignores case.
        let words = words.to_lowercase();
        let words: Vec<&str> = words.split_whitespace().collect();
        let found = |text: &String| match &re {
            Some(re) => re.is_match(text.as_bytes()),
            None => words.iter().all(|w| text.contains(w)),
        };
        // In parallel: 500,000 lookups at each letter typed.
        in_parts(&self.list.files, background_threads().min(8), |part, at| {
            (part.iter().enumerate())
                .filter(|(_, f)| index.get(*f).is_some_and(found))
                .map(|(i, _)| at + i)
                .collect()
        })
    }

    /// "mark:2" shows the files with mark 2, "stars:3" those with three stars or more.
    #[cfg(feature = "marks")]
    pub(crate) fn mark_matches(&self, query: &str) -> Option<Vec<usize>> {
        let (mark, n) = match (query.strip_prefix("mark:"), query.strip_prefix("stars:")) {
            (Some(n), _) => (
                true,
                n.trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|n| (1..=6).contains(n))?,
            ),
            (_, Some(n)) => (false, n.trim().parse::<u8>().ok()?),
            _ => return None,
        };
        let hit = |(bits, stars): (u8, u8)| {
            if mark {
                bits & (1 << (n - 1)) != 0
            } else {
                stars >= n
            }
        };
        let names = self.marked_names(hit);
        Some(
            (0..self.list.files.len())
                .filter(|&i| names.contains(self.list.files[i].as_os_str()))
                .collect(),
        )
    }

    /// The names in the list of the files whose (marks, stars) `hit` takes: from the marked
    /// files, a few thousand at most, not a path made for each of 500,000 at every key press.
    #[cfg(feature = "marks")]
    pub(crate) fn marked_names(&self, hit: impl Fn((u8, u8)) -> bool) -> HashSet<&std::ffi::OsStr> {
        (self.marks.marked())
            .filter(|&(_, v)| hit(v))
            .filter_map(|(p, _)| Some(p.strip_prefix(&self.dir).ok()?.as_os_str()))
            .collect()
    }

    #[cfg(not(feature = "marks"))]
    pub(crate) fn mark_matches(&self, _: &str) -> Option<Vec<usize>> {
        None
    }
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
