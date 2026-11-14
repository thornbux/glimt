//! Fuzzy file name search in the style of fzf. The letters of the query must appear in the
//! name in order, but not next to each other, and case is ignored. Names where they come
//! together, at the start of a word or early in the name, rank first. Here is also the EXIF
//! index that `exif:` terms search, with each file's fields and keywords as one line.

use crate::model::folder::{add_sent_sizes, send_sizes};
use crate::model::list::FileRow;
use crate::support::lang::tr;
use crate::support::threads::small_alloc_threads;
use crate::support::threads::{Receiving, Took, Working, answer_channel};
use crate::support::threads::{background_threads, in_parts};
use crate::{metadata::exif, metadata::xmp};
use crate::{tf, ui::theme};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

const PREFIXES: [&str; 4] = ["exif:", "re:", "mark:", "stars:"];

/// How deep groups may nest in a search. A `(` deeper than this is an ordinary word, so text
/// pasted with thousands of them cannot overflow the stack of the parser, of the search through
/// the tree, or of dropping the tree. Nobody types groups this deep.
const MAX_NESTING: usize = 32;

/// A search as a tree. Terms (`exif:canon`, `re:png$`, a fuzzy name) are combined with and (one
/// after the other), or (`|`) and not (`-` before a prefixed term, or `-(` before a group).
#[derive(Debug, PartialEq)]
pub enum Query {
    Term(String),
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
}

impl Query {
    /// Whether a term anywhere in the query starts with one of `prefixes`, also inside a group
    /// or after a "not". What a search depends on is read from its tree, as the search reads
    /// it, never from the text typed.
    pub fn has_term_with(&self, prefixes: &[&str]) -> bool {
        match self {
            Query::Term(text) => prefixes.iter().any(|p| text.starts_with(p)),
            Query::Not(inner) => inner.has_term_with(prefixes),
            Query::And(parts) | Query::Or(parts) => parts.iter().any(|p| p.has_term_with(prefixes)),
        }
    }
}

/// The search field. It holds the query, the matches, and the EXIF index that exif: terms look
/// in.
#[derive(Default)]
pub(crate) struct Search {
    /// File name search above the file list, and the matching files best first while it has
    /// at least three letters.
    query: String,
    /// Why a re: search pattern is not a valid pattern, shown under the search field.
    error: Option<String>,
    /// The search field takes the focus when it is next drawn (/).
    wants_focus: bool,
    /// The rows among the matches of the shown file and of the file a slideshow of several
    /// panes steps from, as (file, row). They are found once for each image and search, not in
    /// every frame, because the matches can be 300,000 long.
    match_rows: std::cell::Cell<[Option<FileRow>; 2]>,
    match_size_rx: Receiving<u64>,
    /// When to sum the matches' size, once the search has stayed the same for a moment.
    match_size_due: Option<Instant>,
    /// The EXIF text of each file in the folder, in lowercase, for "exif:" searches. It is
    /// built at the first such search and dropped with the folder.
    exif_index: Option<HashMap<OsString, String>>,
    /// The files whose keywords or comment changed after they were indexed. The next "exif:"
    /// search reads only these files again, not the whole folder.
    exif_to_read_again: HashSet<OsString>,
    exif_index_rx: Receiving<(HashMap<OsString, String>, Working)>,
    /// Set when the index being read is no longer wanted (another folder), so the thread stops.
    exif_index_stop: Arc<AtomicBool>,
}

impl Search {
    /// Whether the search looks at marks or stars, so a change of them changes the matches.
    #[cfg(feature = "marks")]
    pub(crate) fn is_about_marks(&self) -> bool {
        self.has_term_with(&["mark:", "stars:"])
    }

    /// Whether a term of the search starts with one of `prefixes`, as `Query::has_term_with`
    /// says.
    fn has_term_with(&self, prefixes: &[&str]) -> bool {
        parse(&self.query).is_some_and(|q| q.has_term_with(prefixes))
    }

    /// The search field emptied, so the whole list shows.
    pub(crate) fn clear_query(&mut self) {
        self.query.clear();
    }

    /// The files' EXIF text is read again at the next "exif:" search, as their keywords or
    /// comment changed.
    pub(crate) fn read_exif_again(&mut self, names: impl IntoIterator<Item = OsString>) {
        self.exif_to_read_again.extend(names);
    }

    /// Whether an "exif:" search waits for files whose keywords are still being written.
    pub(crate) fn exif_search_waits_for_writes(&self) -> bool {
        !self.exif_to_read_again.is_empty() && self.has_term_with(&["exif:"])
    }

    /// Drops the EXIF index of the folder and stops its reading, because another folder opens.
    pub(crate) fn forget_exif_index(&mut self) {
        self.exif_index = None;
        self.exif_to_read_again.clear();
        self.exif_index_rx = Receiving::none();
        // Its reading of every file ends, because no one waits for it now.
        self.exif_index_stop.store(true, Ordering::Relaxed);
    }

    /// Adds the sums of the matches' size that came to `size`.
    pub(crate) fn receive_match_size(&mut self, size: &mut u64) {
        add_sent_sizes(&mut self.match_size_rx, size);
    }

    /// Whether the matches' size is summed.
    pub(crate) fn match_size_known(&self) -> bool {
        self.match_size_rx.is_none() && self.match_size_due.is_none()
    }

    /// The row of `file` among the matches. `find` looks it up only for a file other than the
    /// two kept, which are the file shown (`shown`) and one other.
    pub(crate) fn row_of(
        &self,
        file: usize,
        shown: usize,
        find: impl FnOnce() -> Option<usize>,
    ) -> Option<usize> {
        let mut rows = self.match_rows.get();
        if let Some((_, row)) = rows.iter().flatten().find(|r| r.0 == file) {
            return *row;
        }
        let row = find();
        rows[(file != shown) as usize] = Some((file, row));
        self.match_rows.set(rows);
        row
    }
}

/// For tests of other modules, which set up a search or look at what it holds.
#[cfg(test)]
impl Search {
    pub(crate) fn set_query(&mut self, query: &str) {
        self.query = query.into();
    }

    pub(crate) fn set_exif_index(&mut self, index: HashMap<OsString, String>) {
        self.exif_index = Some(index);
    }

    pub(crate) fn reads_exif_again(&self, name: &str) -> bool {
        self.exif_to_read_again.contains(std::ffi::OsStr::new(name))
    }

    pub(crate) fn has_nothing_to_read_again(&self) -> bool {
        self.exif_to_read_again.is_empty()
    }

    pub(crate) fn reads_index(&self) -> bool {
        self.exif_index_rx.is_some()
    }

    /// The index being read is no longer waited for.
    pub(crate) fn drop_index_reading(&mut self) {
        self.exif_index_rx = Receiving::none();
    }
}

impl crate::App {
    // ---- The search ----

    /// Fuzzy search box above the file list.
    pub(crate) fn search_field(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if !self.search.query.is_empty()
                && crate::ui::menus::small_button(ui, theme::icon::CLOSE).clicked()
            {
                self.search.query.clear();
                self.update_search(ui.ctx());
                self.panel.scroll_to_shown();
            }
            // The field has a fixed ID, so the X button appearing in front of it does not make
            // it a new widget, which would lose the focus after the first letter.
            let hint = match self.config.fuzzy_search {
                true => tr("Search file names (3+ letters), re: pattern, or exif: camera, date …"),
                false => tr("Filter file names by pattern, or exif: camera, date …"),
            };
            let edit = egui::TextEdit::singleline(&mut self.search.query)
                .id(egui::Id::unique("file search"))
                .hint_text(hint);
            // Geeqie's file filter has its Case checkbox to the right, and before it the switch
            // between fuzzy and regular expression search. The row is laid out from the right,
            // so the field takes what is left, because a row wider than the panel would widen it
            // every frame.
            let geeqie = !self.config.fuzzy_search;
            let mut changed = false;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if geeqie
                    && crate::ui::menus::checkbox(
                        ui,
                        &mut self.config.search_case_sensitive,
                        tr("Case"),
                    )
                    .on_hover_text(tr("Tell upper and lower case apart"))
                    .changed()
                {
                    self.config.save();
                    changed = true;
                }
                if crate::ui::menus::selectable(ui, geeqie, ".*")
                    .on_hover_text(tr("Regular expression instead of fuzzy search"))
                    .clicked()
                {
                    self.config.fuzzy_search = geeqie;
                    self.config.save();
                    changed = true;
                }
                let field = ui.add(edit.desired_width(ui.available_width()));
                if std::mem::take(&mut self.search.wants_focus) {
                    field.request_focus();
                }
                changed |= field.changed();
            });
            if changed {
                self.update_search(ui.ctx());
            }
        });
        if let Some(e) = &self.search.error {
            ui.colored_label(ui.visuals().error_fg_color, tf!("Not a pattern: {e}", e));
        }
    }

    /// Runs the search again, because file indices change when the list does.
    pub(crate) fn update_search(&mut self, ctx: &egui::Context) {
        let query = self.search.query.trim().to_owned();
        let query = query.as_str();
        self.search.match_rows.set(Default::default());
        // The rows are others now, so a Shift range of the keys is no longer the rows it was.
        self.list.set_range_end(None);
        // A position "[n/m]" told for another pane is made again, because another search may have
        // as many matches.
        self.status_bar.forget_position_text();
        self.search.error = None;
        let matches = self.search_matches(ctx, query);
        // The selected files that the search hides are kept apart from the selection, so an
        // action acts on what is shown. Those that the search shows again are selected again.
        if self.list.set_matches(matches) {
            self.selection_changed(ctx);
        }
        // The size of the matches is summed in the background. A new search drops the receiver,
        // which stops the old sum.
        self.list.set_match_size(0);
        self.search.match_size_rx = Receiving::none();
        // It is not summed at each letter typed, because each would copy every match's name,
        // 500,000 for an "a".
        let wait = std::time::Duration::from_millis(200);
        self.search.match_size_due = self.list.matches().is_some().then(|| Instant::now() + wait);
        ctx.request_repaint_after(wait);
    }

    /// The size of the matches, in the background, once the search has stayed the same a moment.
    pub(crate) fn sum_match_size(&mut self, ctx: &egui::Context) {
        let Some(due) = self.search.match_size_due else {
            return;
        };
        // This is asked again each frame, because a frame drawn before it is due drops the
        // earlier request.
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            return ctx.request_repaint_after(wait);
        }
        self.search.match_size_due = None;
        let Some(m) = self.list.matches() else {
            return;
        };
        let names: Vec<OsString> = m.iter().map(|&i| self.list.files()[i].clone()).collect();
        let (tx, rx) = mpsc::channel();
        let (dir, ctx) = (self.folder.dir().to_path_buf(), ctx.clone());
        // No busy pointer: the matches' size in the status bar, a look-up nobody waits for, shown
        // when it comes.
        std::thread::spawn(move || {
            let sizes = names
                .iter()
                .filter_map(|n| Some(dir.join(n).metadata().ok()?.len()));
            send_sizes(sizes, tx, &ctx);
        });
        self.search.match_size_rx = Receiving::of(rx);
    }

    /// Takes the exif: search index once it is read, and runs the search again with it.
    pub(crate) fn receive_exif_index(&mut self, ctx: &egui::Context) {
        // The busy pointer goes when this returns, once the search has the index.
        let (index, _pointer) = match self.search.exif_index_rx.take() {
            Took::Nothing => return,
            Took::Answer(answer) => (answer.0, Some(answer.1)),
            // Without any index, the search would start reading again at once, so it gets an
            // empty one, and the user is told.
            Took::Gone => {
                self.tell_background_stopped(tr("The EXIF index"));
                (HashMap::new(), None)
            }
        };
        // The files read again go into the index there is. Otherwise this is the whole index.
        match &mut self.search.exif_index {
            Some(old) => old.extend(index),
            None => self.search.exif_index = Some(index),
        }
        // The index was read from the list as it was, so names that came since are read too.
        self.index_new_names();
        self.update_search(ctx);
    }

    /// Makes the next search read the names in the list that the `exif:` index has not read
    /// (renamed, moved or copied in, new in a folder read again, or from subfolders turned on),
    /// as it reads edited ones. Before, a name it lacked was found by no `exif:` search at all.
    pub(crate) fn index_new_names(&mut self) {
        if let Some(index) = &self.search.exif_index {
            let missing = self.list.files().iter().filter(|n| !index.contains_key(*n));
            self.search.exif_to_read_again.extend(missing.cloned());
        }
    }

    /// The files a search finds. One term finds them as it always did (fuzzy names best first),
    /// and terms combined (`sunset ( exif:canon | exif:nikon ) -re:png$`, see `parse`) find
    /// them in list order. Returns `None` for no search.
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

    /// The files a part of a combined search finds, in order. An and keeps the order of its
    /// first part that is no "not", and an or keeps the order of its parts one after the other.
    pub(crate) fn query_matches(&mut self, ctx: &egui::Context, query: &Query) -> Vec<usize> {
        use Query;
        let n = self.list.files().len();
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
                    // A "not" is left out directly, and the other parts keep what both find.
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
        let case = self.config.search_case_sensitive && !self.config.fuzzy_search;
        let regex = |this: &mut Self, pattern: &str| match regex(pattern, case) {
            Ok(re) => regex_matches(this.list.files(), &re),
            Err(e) => {
                this.search.error = Some(e);
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
                            (term.chars().count() >= min).then(|| matches(self.list.files(), term))
                        }
                        // As in Geeqie's file filter, it is a regular expression, from the
                        // first letter.
                        false => (!term.is_empty()).then(|| regex(self, term)),
                    })
            }
        }
    }

    /// Files whose EXIF contains every word, or that a regular expression after "re:" finds, in
    /// list order. The first such search in a folder reads the EXIF of all files in the
    /// background (the busy pointer shows meanwhile) and finds nothing until that is done.
    pub(crate) fn exif_matches(&mut self, ctx: &egui::Context, words: &str) -> Vec<usize> {
        let re = match words.strip_prefix("re:").map(|p| regex(p, false)) {
            Some(Err(e)) => {
                self.search.error = Some(e);
                return Vec::new();
            }
            Some(Ok(re)) => Some(re),
            None => None,
        };
        // Files edited since are read again alone, while the search goes on with the index
        // there is. That waits while keywords are still being written, and the marks stay until
        // they are, so files read before their sidecar landed are read again after. (The frame
        // that sees the job end searches again.)
        let writing = self.file_operations.is_writing_sidecars();
        if !writing
            && self.search.exif_index.is_some()
            && !self.search.exif_to_read_again.is_empty()
            && self.search.exif_index_rx.is_none()
        {
            let (tx, rx) = answer_channel(&self.busy);
            let names: Vec<OsString> = self.search.exif_to_read_again.drain().collect();
            let (dir, ctx) = (self.folder.dir().to_path_buf(), ctx.clone());
            let pointer = Working::new(&self.busy);
            self.search.exif_index_stop = Arc::default();
            let stop = self.search.exif_index_stop.clone();
            std::thread::spawn(move || {
                let _ = tx.send(exif_index(&dir, &names, &stop));
                drop(pointer);
                ctx.request_repaint();
            });
            self.search.exif_index_rx = Receiving::of(rx);
        }
        let Some(index) = &self.search.exif_index else {
            if self.search.exif_index_rx.is_none() {
                // Marks made while keywords are written are kept for after this first index.
                if !writing {
                    self.search.exif_to_read_again.clear();
                }
                let (tx, rx) = answer_channel(&self.busy);
                let (dir, files, ctx) = (
                    self.folder.dir().to_path_buf(),
                    self.list.files().to_vec(),
                    ctx.clone(),
                );
                let pointer = Working::new(&self.busy);
                self.search.exif_index_stop = Arc::default();
                let stop = self.search.exif_index_stop.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(exif_index(&dir, &files, &stop));
                    drop(pointer);
                    ctx.request_repaint();
                });
                self.search.exif_index_rx = Receiving::of(rx);
            }
            return Vec::new();
        };
        // The index is lowercase, and the words are made so too, while a regular expression
        // ignores case.
        let words = words.to_lowercase();
        let words: Vec<&str> = words.split_whitespace().collect();
        let found = |text: &String| match &re {
            Some(re) => re.is_match(text.as_bytes()),
            None => words.iter().all(|w| text.contains(w)),
        };
        // This runs in parallel, because it is 500,000 lookups at each letter typed.
        in_parts(
            self.list.files(),
            background_threads().min(8),
            |part, at| {
                (part.iter().enumerate())
                    .filter(|(_, f)| index.get(*f).is_some_and(found))
                    .map(|(i, _)| at + i)
                    .collect()
            },
        )
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
            (0..self.list.files().len())
                .filter(|&i| names.contains(self.list.files()[i].as_os_str()))
                .collect(),
        )
    }

    /// The names in the list of the files whose (marks, stars) `hit` takes. They come from the
    /// marked files, a few thousand at most, rather than a path made for each of 500,000 at
    /// every key press.
    #[cfg(feature = "marks")]
    pub(crate) fn marked_names(&self, hit: impl Fn((u8, u8)) -> bool) -> HashSet<&std::ffi::OsStr> {
        (self.marks.marked())
            .filter(|&(_, v)| hit(v))
            .filter_map(|(p, _)| Some(p.strip_prefix(self.folder.dir()).ok()?.as_os_str()))
            .collect()
    }

    #[cfg(not(feature = "marks"))]
    pub(crate) fn mark_matches(&self, _: &str) -> Option<Vec<usize>> {
        None
    }

    /// /. It focuses the search field, and shows it with the file list (out of full screen) if
    /// it was hidden. The / itself is not typed into it.
    pub(crate) fn focus_search_field(&mut self, ctx: &egui::Context, fullscreen: bool) {
        // The field shows if it was hidden, with the file list (out of full screen),
        // and the / itself is not typed into it.
        if !self.config.search_bar {
            self.config.search_bar = true;
            self.config.save();
        }
        self.panel.show();
        if fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        ctx.input_mut(|i| (i.events).retain(|e| *e != egui::Event::Text("/".into())));
        // The focus is given when the field is drawn, because egui drops the focus on a widget
        // not drawn yet.
        self.search.wants_focus = true;
    }
}

/// The score of the lowercase `name` for the lowercase `query`, higher is better. Returns
/// `None` if it does not match. It is generic over bytes (ASCII names, no allocation) and chars
/// (the rest).
fn score<T: Copy + PartialEq>(name: &[T], query: &[T], word: impl Fn(T) -> bool) -> Option<i32> {
    // A plain substring is what people usually type, so it always beats a scattered match.
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

/// The tree of a search. Returns `None` for an empty one. A term starts at a prefix (`exif:`,
/// `re:`, `mark:`, `stars:`), and the words after it belong to it, so `exif:canon 2024:05`
/// stays one term. Words before the first prefix of a group search the names. `|` binds
/// loosest, and `(`, `-(` and `)` group, all as words of their own, so a regular expression
/// keeps its own parentheses and bars (`re:^(IMG|DSC)`). A `)` too many is left out, and a
/// missing one is assumed. A group nested deeper than `MAX_NESTING` is read as words.
pub fn parse(query: &str) -> Option<Query> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut at = 0;
    let parsed = parse_or(&tokens, &mut at, 0);
    // A stray ")" ends a group that was not opened, and the rest is read on.
    let mut parts: Vec<Query> = parsed.into_iter().collect();
    while at < tokens.len() {
        at += 1;
        parts.extend(parse_or(&tokens, &mut at, 0));
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Query::And(parts)),
    }
}

fn prefixed(t: &str) -> bool {
    PREFIXES.iter().any(|p| t.starts_with(p))
}

/// Parses groups joined by `|`, up to a `)` or the end, inside `depth` groups.
fn parse_or(tokens: &[&str], at: &mut usize, depth: usize) -> Option<Query> {
    let mut parts: Vec<Query> = Vec::new();
    loop {
        parts.extend(parse_and(tokens, at, depth));
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

/// Parses terms and groups one after the other, up to a `|`, a `)` or the end, inside `depth`
/// groups.
fn parse_and(tokens: &[&str], at: &mut usize, depth: usize) -> Option<Query> {
    let mut parts: Vec<Query> = Vec::new();
    // Words after a term belong to it, and after a group they start a search of the names.
    let mut open_term = false;
    while let Some(&token) = tokens.get(*at) {
        match token {
            "|" | ")" => break,
            "(" | "-(" if depth < MAX_NESTING => {
                *at += 1;
                let inner = parse_or(tokens, at, depth + 1);
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

/// A `re:` search, which ignores case unless `case`. For a pattern that is not valid, it
/// returns why, to show while typing.
pub fn regex(pattern: &str, case: bool) -> Result<regex::bytes::Regex, String> {
    regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(!case)
        .build()
        .map_err(|e| {
            // The last line says what is wrong, and the ones before point at where.
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

/// The indices of the matching names, best first, then in list order. The work is split over
/// the cores, and 300,000 names take a few milliseconds.
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

/// (score, index) of the names `score_one` matches, in list order, split over the cores. Each
/// thread lends it a buffer of its own.
fn in_parallel(
    names: &[std::ffi::OsString],
    score_one: impl Fn(&std::ffi::OsString, &mut Vec<u8>) -> Option<i32> + Sync,
) -> Vec<(i32, usize)> {
    crate::support::threads::in_parts(
        names,
        crate::support::threads::background_threads().min(8),
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
    let index = crate::support::threads::in_parts(files, small_alloc_threads(), |part, _| {
        let mut out = Vec::with_capacity(part.len());
        for name in part {
            // When the folder is gone from the window, no one waits for this.
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let path = dir.join(name);
            // A file that breaks its reader has no text. A whole part lost would leave 1/N of
            // the folder out of every exif: search, in silence.
            let text = crate::support::threads::catch_panic(|| {
                let rows = exif::read(&path);
                let mut text: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
                let meta = xmp::read(&path).text();
                text.push(&meta);
                text.join(" ").to_lowercase()
            });
            out.push((name.clone(), text.unwrap_or_default()));
        }
        out
    });
    index.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;
    use crate::ui::actions::Action;

    /// The selection holds only files the list shows. A search takes the files it hides out of
    /// it, so a file action acts on what is on screen and not on a hidden file, and no way of
    /// selecting adds a hidden one.
    #[test]
    fn a_search_keeps_the_selection_to_its_matches() {
        let dir = std::env::temp_dir().join(format!("glimt-test-sel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let names = ["a.jpg", "b.jpg", "beach.jpg", "beach2.jpg"];
        app.list.set_files(names.iter().map(Into::into).collect());
        app.list.select([1, 2].into());
        app.search.set_query("beach");
        app.update_search(&ctx);
        assert_eq!(app.list.selected(), &[2].into());
        assert_eq!(app.targets(), Some(vec![2]));
        // Ctrl+click adds the shown image the first time, but not when the search hides it.
        app.list.clear_selection();
        app.list.add_to_selection([app.list.shown(), 3]);
        app.list.toggle_selected(0);
        app.list.set_selected(1, true);
        app.set_selection(&ctx, [0, 1, 2].into());
        assert_eq!(app.list.selected(), &[2].into());
        app.list.select_one(1);
        assert!(app.list.selected().is_empty());
        app.list.add_to_selection([2, 3]);
        // A selected file that the search hides is kept apart, and is selected again when it is
        // shown again.
        app.search.set_query("beach2");
        app.update_search(&ctx);
        assert_eq!(app.list.selected(), &[3].into());
        assert_eq!(app.targets(), Some(vec![3]));
        app.search.set_query("");
        app.update_search(&ctx);
        assert_eq!(app.list.selected(), &[2, 3].into());
        // Select None, a plain click and Select All let go of the hidden selected files too.
        let ways: [fn(&mut crate::App, &eframe::egui::Context); 3] = [
            |a, c| a.run(c, Action::SelectNone),
            |a, _| a.list.select_one(3),
            |a, c| a.run(c, Action::SelectAll),
        ];
        for select in ways {
            app.list.select([1, 3].into());
            app.search.set_query("beach2");
            app.update_search(&ctx);
            select(&mut app, &ctx);
            app.search.set_query("");
            app.update_search(&ctx);
            assert!(!app.list.selected().contains(&1));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A query on its way to the one meant finds nothing for a moment: `re:` before `re:IMG`,
    /// a pattern not yet closed, a typo, or `exif:` while its index is read. The selection
    /// comes back once the query shows its files again, so an action after acts on all of them.
    #[test]
    fn a_query_typed_letter_by_letter_keeps_the_selection() {
        let dir = std::env::temp_dir().join(format!("glimt-test-typed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        let names = ["IMG_1.jpg", "IMG_2.jpg", "other.jpg"];
        app.list.set_files(names.iter().map(Into::into).collect());
        let typed = |app: &mut crate::App, steps: &[&str]| {
            app.list.select([0, 1].into());
            for q in steps {
                app.search.set_query(q);
                app.update_search(&ctx);
            }
            app.list.selected().clone()
        };
        assert_eq!(typed(&mut app, &["re:", "re:I", "re:IMG"]), [0, 1].into());
        assert_eq!(typed(&mut app, &["re:(", "re:(IMG)"]), [0, 1].into());
        assert_eq!(typed(&mut app, &["imx", "im", "img"]), [0, 1].into());
        app.config.fuzzy_search = false;
        assert_eq!(typed(&mut app, &["IMG(", "IMG(_)"]), [0, 1].into());
        app.config.fuzzy_search = true;
        // The EXIF index is read in the background, and finds nothing until it is there.
        assert!(typed(&mut app, &["exi", "exif:canon"]).is_empty());
        assert_eq!(app.targets(), None);
        assert_eq!(typed(&mut app, &["exif:canon", ""]), [0, 1].into());
        let read = |a: &crate::App| a.search.exif_index.is_some();
        assert!(crate::support::test_support::run_background_until(
            &mut app, &ctx, read
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A search by marks or stars shows the new matches after a change of them, wherever its
    /// term stands in the query, and whichever way the marks changed. Before, only a query that
    /// began with `mark:` or `stars:` was run again, and only after a mark key, so a photo given
    /// one star kept its row under `sunset stars:3`, and Select All then took it in.
    #[cfg(feature = "marks")]
    #[test]
    fn a_search_by_marks_follows_their_change_wherever_its_term_stands() {
        let dir = std::env::temp_dir().join(format!("glimt-test-bymarks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["sunset1.jpg", "sunset2.jpg", "other.jpg"];
        let paths: Vec<_> = names.iter().map(|n| dir.join(n)).collect();
        use crate::support::test_support::unlisted;
        let cases = [
            ("sunset stars:3", Action::Rate1),
            (" stars:3", Action::Rate1),
            ("( stars:3 | re:^nothing$ )", Action::Rate1),
            ("-mark:1", Action::Mark1),
        ];
        for (query, action) in cases {
            let (mut app, ctx) = app_on(&dir);
            app.list.set_files(names.iter().map(Into::into).collect());
            app.marks.rate(&unlisted(&paths), 3);
            app.search.set_query(query);
            app.update_search(&ctx);
            let shown = |app: &crate::App| app.list.matches().unwrap().contains(&0);
            assert!(shown(&app), "{query}");
            app.list.select([0].into());
            app.run(&ctx, action);
            app.receive_answers(&ctx);
            assert!(!shown(&app), "{query} after {action:?}");
            // Stars given another way than a key, such as a Lightroom import, count too.
            app.marks.rate(&unlisted(&paths[1..2]), 1);
            app.receive_answers(&ctx);
            assert!(!app.list.matches().unwrap().contains(&1) || query == "-mark:1");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What a search depends on is read from its tree, as `parse` reads it, and never by a
    /// second look at the text typed, which missed a term that did not stand first.
    #[test]
    fn what_a_search_depends_on_is_read_from_its_tree() {
        let source = include_str!("search.rs");
        for look in [
            "starts_with(",
            "contains(",
            "ends_with(",
            "strip_prefix(",
            "find(",
        ] {
            let bypass = format!("self.query.{look}");
            assert!(!source.contains(&bypass), "{bypass}: use has_term_with");
        }
        let uses = |q: &str| super::parse(q).is_some_and(|q| q.has_term_with(&["stars:"]));
        assert!(uses("a ( b | -stars:2 )") && uses(" stars:1") && !uses("a astars:1"));
    }

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
        // Case is ignored. A substring at the start of a word ranks above one inside a word,
        // and a scattered match comes last.
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

    /// A search is read as a tree. Prefixes start terms, and the words after one belong to it.
    /// `-` negates a prefixed term or a group, `|` binds loosest, `(` and `)` group as words of
    /// their own, and a regular expression keeps its parentheses and bars.
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
        // A name with a dash is a name. A missing ")" is assumed, and one too many is left out.
        assert_eq!(p("-draft"), Some(t("-draft")));
        assert_eq!(p("( mark:2"), Some(t("mark:2")));
        assert_eq!(
            p("mark:2 ) stars:3"),
            Some(And(vec![t("mark:2"), t("stars:3")]))
        );
        assert_eq!(p("| ( ) |"), None);
        assert_eq!(p("  "), None);
    }

    /// A search of 100,000 nested groups, as pasted text may be, is read with its groups cut
    /// off at `MAX_NESTING`. Before, the parser and the search through the tree called
    /// themselves once for each group, and the window's stack overflowed.
    #[test]
    fn deep_groups_are_read_as_words() {
        use super::Query;
        fn depth(q: &Query) -> usize {
            match q {
                Query::Term(_) => 1,
                Query::Not(inner) => 1 + depth(inner),
                Query::And(parts) | Query::Or(parts) => {
                    1 + parts.iter().map(depth).max().unwrap_or(0)
                }
            }
        }
        for open in ["( ", "-( "] {
            let query = super::parse(&(open.repeat(100_000) + "mark:2 a")).unwrap();
            assert!(depth(&query) <= super::MAX_NESTING + 2, "{open}");
        }
        // Up to the limit, a group is a group.
        let nested = "-( ".repeat(super::MAX_NESTING) + "mark:2";
        assert!(!format!("{:?}", super::parse(&nested)).contains("-("));
    }

    /// Searches broken in thousands of ways are read without a panic or an endless loop.
    #[test]
    fn survives_broken_searches() {
        let seed = b"sunset ( exif:canon | -exif:nikon 2024 ) -( re:^(a|b) | stars:1 ) mark:2";
        for bytes in crate::support::test_support::broken(seed) {
            super::parse(&String::from_utf8_lossy(&bytes));
        }
    }

    /// re: finds names by a regular expression, ignoring case, in list order. A broken one says
    /// why in one line.
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
