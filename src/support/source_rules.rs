//! Checks of the source itself, run as tests: the layout AGENTS.md describes under "Code
//! layout", so a file cannot drift from it unnoticed. They read each file as code only
//! (`code_lines`: comments and the insides of literals blanked), so a string or a comment is
//! never taken for an item, an import or a function.

use std::path::Path;

/// Where a top-level item stands in a file's order, as AGENTS.md gives it under "Within a
/// file"; the variants are in that order.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Place {
    Imports,
    Constants,
    Types,
    AppBlock,
    Functions,
    Tests,
}

impl Place {
    fn name(self) -> &'static str {
        match self {
            Place::Imports => "use and mod",
            Place::Constants => "const and static",
            Place::Types => "types and their impls",
            Place::AppBlock => "the impl crate::App block",
            Place::Functions => "functions",
            Place::Tests => "the tests",
        }
    }
}

/// Every source file and its text: the Rust files by their path below `src/`, also those in
/// folders (`files/config.rs`), and the translations in `po/` as `po/da.po`. A file added is
/// checked without being listed anywhere.
pub(crate) fn sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut folders = vec![src.clone()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            let name = path
                .strip_prefix(&src)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if name.ends_with(".rs") {
                files.push((name, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let po = Path::new(env!("CARGO_MANIFEST_DIR")).join("po");
    for entry in std::fs::read_dir(po).unwrap() {
        let path = entry.unwrap().path();
        let name = format!("po/{}", path.file_name().unwrap().to_str().unwrap());
        if name.ends_with(".po") {
            files.push((name, std::fs::read_to_string(&path).unwrap()));
        }
    }
    files.sort();
    files
}

/// The names a `crate::` path can start with: every module, which is a file's name without its
/// folder and `.rs`, and every folder of modules.
fn module_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (path, _) in rust_sources() {
        let mut parts: Vec<&str> = path.trim_end_matches(".rs").split('/').collect();
        names.push(parts.pop().unwrap().to_owned());
        names.extend(parts.into_iter().map(str::to_owned));
    }
    names.sort();
    names.dedup();
    names
}

/// The names a `crate::` path can start with: main.rs's own modules, a file directly in `src/`
/// or a folder of modules. A module inside a folder is not one, as `crate::config` reaches it
/// only through an import in main.rs.
fn root_names() -> Vec<String> {
    let mut names: Vec<String> = (rust_sources().into_iter())
        .map(|(path, _)| {
            path.trim_end_matches(".rs")
                .split('/')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    names.dedup();
    names
}

/// The Rust files of `sources`.
pub(crate) fn rust_sources() -> Vec<(String, String)> {
    (sources().into_iter())
        .filter(|(name, _)| name.ends_with(".rs"))
        .collect()
}

/// The text of the source file `name`.
fn source(name: &str) -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    std::fs::read_to_string(src.join(name)).unwrap()
}

/// `text`'s lines as code only: comments and the insides of string and char literals blanked,
/// and the blanks at the end of each line gone, so a line closing a block with a comment after
/// it is just `}`. Every code character keeps its line and column.
fn code_lines(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut code = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match literal_end(&chars, i) {
            Some(end) => {
                let blank = |&c: &char| if c == '\n' { '\n' } else { ' ' };
                code.extend(chars[i..end].iter().map(blank));
                i = end;
            }
            None => {
                code.push(chars[i]);
                i += 1;
            }
        }
    }
    code.lines().map(|l| l.trim_end().to_owned()).collect()
}

/// Where the comment or literal that starts at `i` ends (one past its last character); `None`
/// when code starts there.
fn literal_end(c: &[char], i: usize) -> Option<usize> {
    let at = |k: usize| c.get(k).copied();
    let word = |k: usize| at(k).is_some_and(|x| x == '_' || x.is_alphanumeric());
    // One past the first `end` from `from` on, or the end of the text.
    let past = |from: usize, end: &[char]| {
        (from..c.len())
            .find(|&k| c[k..].starts_with(end))
            .map_or(c.len(), |k| k + end.len())
    };
    // An r that starts a raw string (r"…", r#"…"#, br"…") does not end a word.
    let before = i.wrapping_sub(1);
    let raw_r = !word(before) || (at(before) == Some('b') && !word(i.wrapping_sub(2)));
    match (at(i)?, at(i + 1)) {
        ('/', Some('/')) => Some((i..c.len()).find(|&k| c[k] == '\n').unwrap_or(c.len())),
        ('/', Some('*')) => Some(past(i + 2, &['*', '/'])),
        ('"', _) => {
            let mut k = i + 1;
            while k < c.len() && c[k] != '"' {
                k += if c[k] == '\\' { 2 } else { 1 };
            }
            Some((k + 1).min(c.len()))
        }
        // A char literal ('x', '\n', '\'', '\u{…}'), not a lifetime ('a, 'static).
        ('\'', Some('\\')) => Some(past(i + 3, &['\''])),
        ('\'', _) if at(i + 2) == Some('\'') => Some(i + 3),
        ('r', _) if raw_r => {
            let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
            (at(i + 1 + hashes) == Some('"')).then(|| {
                let close: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                past(i + 2 + hashes, &close)
            })
        }
        _ => None,
    }
}

/// The string literals of Rust `code` outside its comments: where each starts, and its text
/// with escapes and continued lines read. Byte strings and char literals are left out.
pub(crate) fn literals(code: &str) -> Vec<(usize, String)> {
    let b = code.as_bytes();
    let word = |i: usize| i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        let rest = &code[i..];
        let raw_hashes = (b[i] == b'r')
            .then(|| rest[1..].find(|c| c != '#'))
            .flatten()
            .filter(|&n| rest[1 + n..].starts_with('"'));
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
        } else if rest.starts_with("/*") {
            i += rest.find("*/").map_or(rest.len(), |n| n + 2);
        } else if b[i] == b'\'' {
            // A char literal ('"', '\''), or a lifetime ('a), which has no end.
            let end = match rest[1..].chars().next().unwrap_or(' ') {
                '\\' => rest.get(3..).and_then(|r| r.find('\'')).map(|n| n + 3),
                c => Some(1 + c.len_utf8()).filter(|&e| rest[e..].starts_with('\'')),
            };
            i += end.map_or(1, |e| e + 1);
        } else if let Some(hashes) = raw_hashes
            && (!word(i) || (b[i - 1] == b'b' && !word(i - 1)))
        {
            let close = format!("\"{}", "#".repeat(hashes));
            let body = 2 + hashes;
            let len = rest[body..].find(&close).unwrap_or(rest.len() - body);
            if !word(i) {
                out.push((i, rest[body..body + len].to_owned()));
            }
            i += body + len + close.len();
        } else if b[i] == b'"' {
            let (mut text, mut chars) = (String::new(), rest[1..].char_indices().peekable());
            let mut end = rest.len();
            while let Some((k, c)) = chars.next() {
                if c == '"' {
                    end = k + 2;
                    break;
                }
                if c != '\\' {
                    text.push(c);
                    continue;
                }
                match chars.next().map(|e| e.1) {
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('\n') => while chars.next_if(|e| e.1.is_whitespace()).is_some() {},
                    Some('u') => {
                        let hex: String = (chars.by_ref().map(|e| e.1))
                            .take_while(|&c| c != '}')
                            .filter(char::is_ascii_hexdigit)
                            .collect();
                        text.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                    }
                    Some(c) => text.push(c),
                    None => {}
                }
            }
            if !(i > 0 && b[i - 1] == b'b' && !word(i - 1)) {
                out.push((i, text));
            }
            i += end;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// The literals read whole: escapes, continued lines, raw and byte strings, char literals
/// and lifetimes, comments.
#[test]
fn reads_literals() {
    let code = "tr(\"a \\\"b\\\" \\\n    c\\n\\u{e9}\"); // \"no\"\n/* \"no\" */ b\"no\" '\"' '\\'' \
                &'static str r#\"x \"y\" z\"# br\"no\" '\\u{feff}' \"\"";
    let texts: Vec<String> = literals(code).into_iter().map(|l| l.1).collect();
    assert_eq!(texts, ["a \"b\" c\né", "x \"y\" z", ""]);
}

/// The rules find a function at any indent, and a `Sense` that takes a click in every spelling.
#[test]
fn finds_functions_and_clicks() {
    assert_eq!(
        function_name("    pub(crate) fn turn<T>(x: T)"),
        Some("turn")
    );
    assert_eq!(function_name("        fn inner() {"), Some("inner"));
    assert_eq!(function_name("    let f = 1;"), None);
    for click in [
        "ui.interact(rect, id, egui::Sense::CLICK)",
        "ui.allocate_rect(r, Sense::click())",
        "ui.interact(r, id, egui::Sense::click_and_drag())",
        "ui.interact(r, id, egui::Sense::all())",
        "ui.interact(r, id, Sense::hover() | Sense::CLICK)",
    ] {
        assert!(takes_a_click(click), "{click}");
    }
    for none in [
        "ui.interact(r, id, egui::Sense::hover())",
        "Sense::DRAG",
        "let s: Sense = x;",
    ] {
        assert!(!takes_a_click(none), "{none}");
    }
}

/// `line` without what may stand before `fn`, `struct` and the like: a visibility, `unsafe`,
/// `async`, and `const` or `extern "C"` when a function follows (a const fn is a function, not
/// a constant).
fn qualifiers_off(line: &str) -> &str {
    let mut l = line;
    loop {
        let before = l;
        if let Some((_, rest)) = l.strip_prefix("pub(").and_then(|r| r.split_once(") ")) {
            l = rest;
        }
        for prefix in ["pub ", "unsafe ", "async "] {
            l = l.strip_prefix(prefix).unwrap_or(l);
        }
        for prefix in ["const ", "extern \"C\" "] {
            let function = |r: &&str| qualifiers_off(r).starts_with("fn ");
            if let Some(rest) = l.strip_prefix(prefix).filter(function) {
                l = rest;
            }
        }
        if l == before {
            return l;
        }
    }
}

/// Returns the name of the function that `line` starts, at any indent (a method, or a function
/// inside another), or `None` when it starts none. Every rule that asks which function a line
/// is in finds it here, so none of them misses an indented one.
fn function_name(line: &str) -> Option<&str> {
    let rest = qualifiers_off(line.trim_start()).strip_prefix("fn ")?;
    rest.split(['(', '<']).next()
}

/// Whether `line` names a `Sense` that takes a click, in any of egui's spellings
/// (`Sense::click()`, `Sense::CLICK`, `Sense::click_and_drag()`, `Sense::all()`, a union). Every
/// spelling counts except the few that take no click, so a new one is caught, not missed.
fn takes_a_click(line: &str) -> bool {
    const NO_CLICK: &[&str] = &[
        "hover",
        "HOVER",
        "drag",
        "DRAG",
        "focusable_noninteractive",
        "FOCUSABLE",
        "empty",
    ];
    line.match_indices("Sense::").any(|(at, sense)| {
        let rest = &line[at + sense.len()..];
        let name = rest
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next();
        !NO_CLICK.contains(&name.unwrap_or(""))
    })
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The line of the `}` that closes the block whose first line is `i`: the next line at the same
/// indent that starts with `}`.
fn block_end(lines: &[String], i: usize) -> Option<usize> {
    let closes = |j: &usize| indent(&lines[*j]) == indent(&lines[i]);
    (i + 1..lines.len()).find(|j| closes(j) && lines[*j].trim_start().starts_with('}'))
}

/// The line where the tests module starts in `lines`, or their end without one.
fn tests_start(lines: &[String]) -> usize {
    (lines.iter())
        .position(|l| qualifiers_off(l) == "mod tests {")
        .unwrap_or(lines.len())
}

/// The place of the top-level item that starts on `line` (a line of `code_lines`); `None` for a
/// line that starts none (an attribute, the rest of an item, a blank).
fn place(line: &str) -> Option<Place> {
    let l = qualifiers_off(line);
    let starts = |word: &str| {
        l.strip_prefix(word)
            .is_some_and(|rest| rest.starts_with([' ', '<', '!', '(', '"']))
    };
    let for_app = l.contains(" for App ") || l.contains(" for crate::App ");
    let types = ["struct", "enum", "type", "trait", "union", "impl"];
    let macro_call = l.split_once('!').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    });
    Some(if l == "mod tests {" {
        Place::Tests
    } else if starts("use") || starts("mod") || starts("macro_rules") || starts("extern crate") {
        Place::Imports
    } else if starts("const") || starts("static") {
        Place::Constants
    } else if l == "impl App {" || l == "impl crate::App {" || (starts("impl") && for_app) {
        Place::AppBlock
    } else if types.iter().any(|w| starts(w)) {
        Place::Types
    } else if starts("fn") || starts("extern") {
        Place::Functions
    } else if macro_call {
        // A macro that makes items, such as config!.
        Place::Types
    } else {
        return None;
    })
}

/// Each file's items in AGENTS.md's order: use and mod, const and static, types with their
/// impls, one `impl crate::App` block, functions, and the tests last.
#[test]
fn items_are_in_order() {
    let mut wrong = Vec::new();
    for (name, text) in rust_sources() {
        let lines = code_lines(&text);
        let (mut last, mut apps, mut i) = (Place::Imports, 0, 0);
        while i < lines.len() {
            let line = &lines[i];
            let Some(here) = place(line) else {
                i += 1;
                continue;
            };
            if line == "impl App {" || line == "impl crate::App {" {
                apps += 1;
                if apps == 2 {
                    wrong.push(format!("{name}:{}: a second impl crate::App block", i + 1));
                }
            }
            if here < last {
                let (what, after) = (here.name(), last.name());
                wrong.push(format!("{name}:{}: {what} after {after}", i + 1));
            }
            last = here;
            // Nothing inside the tests module is ranked; whatever comes after it is.
            i = match here {
                Place::Tests => block_end(&lines, i).unwrap_or(lines.len()) + 1,
                _ => i + 1,
            };
        }
    }
    assert!(wrong.is_empty(), "out of order:\n{}", wrong.join("\n"));
}

/// A test sits in the tests of the module whose code it tests. Once main.rs's tests module held
/// what such tests start from (`app_on`, `settle`), and that drew every test that needed a whole
/// `App` into main.rs. Those helpers are in test_support.rs now, and only the tests of main.rs's
/// own code stay in main.rs.
#[test]
fn tests_sit_beside_their_code() {
    const OWN: &[&str] = &[
        "an_app_lists_its_folder_without_a_window",
        "tells_settings_problems_but_unknown_ones",
        "new_window_program",
    ];
    let code = code_lines(&source("main.rs"));
    let mut lines = code[tests_start(&code)..].iter().map(|l| l.trim());
    while let Some(line) = lines.next() {
        if line == "#[test]" {
            // Past other attributes, such as #[ignore], to the function.
            let name = (lines.by_ref())
                .find(|l| !l.is_empty() && !l.starts_with("#["))
                .unwrap();
            let name = name.trim_start_matches("fn ").split('(').next().unwrap();
            assert!(
                OWN.contains(&name),
                "{name} is in main.rs: put it in the tests of the module it tests, with \
                 crate::support::test_support::app_on for an App, or list it in OWN if it tests main.rs"
            );
        }
    }
}

/// App holds its parts, each a struct in the module whose code uses it, and not much else:
/// 170 fields side by side once made every module free to touch every other's state. A new
/// part's state goes in a struct in its module; a field more here fails this.
#[test]
fn app_stays_small() {
    const MOST: usize = 42;
    let lines = code_lines(&source("main.rs"));
    let start = lines.iter().position(|l| l == "struct App {").unwrap();
    let end = block_end(&lines, start).unwrap();
    let fields = (lines[start + 1..end].iter())
        .filter(|l| indent(l) == 4 && !l.trim_start().starts_with("#["))
        .count();
    assert!(
        fields <= MOST,
        "App has {fields} fields: put a new part's state in a struct in its module, as \
         slideshow::Slideshow is, rather than in App"
    );
}

/// A part of App keeps its fields to itself: only its module changes them, and other modules
/// call methods whose names say what happens (`self.slideshow.toggle_pause()`), so the
/// compiler, not a convention, keeps a part's rules in one place. A public field in one of these
/// structs fails this.
#[test]
fn parts_keep_their_fields() {
    const PARTS: &[(&str, &str)] = &[
        ("features/check.rs", "Check"),
        ("features/duplicates.rs", "Dups"),
        ("features/print.rs", "PrintDialog"),
        ("features/remote.rs", "Listener"),
        ("files/file_ops.rs", "Work"),
        ("metadata/lightroom.rs", "Import"),
        ("metadata/marks.rs", "Marks"),
        ("model/folder.rs", "Folder"),
        ("model/list.rs", "List"),
        ("model/search.rs", "Search"),
        ("ui/dialogs.rs", "Dialogs"),
        ("ui/file_list.rs", "Panel"),
        ("ui/file_list.rs", "StatusBar"),
        ("ui/file_list.rs", "Thumbs"),
        ("ui/file_list.rs", "Tree"),
        ("ui/info_panel.rs", "Info"),
        ("ui/menus.rs", "OpenWith"),
        ("ui/preferences.rs", "Prefs"),
        ("ui/theme.rs", "FromDesktop"),
        ("ui/window.rs", "Window"),
        ("viewer/pan.rs", "Pan"),
        ("viewer/save_as.rs", "Choice"),
        ("viewer/save_as.rs", "SaveAs"),
        ("viewer/slideshow.rs", "Slideshow"),
        ("viewer/view.rs", "Decoding"),
        ("viewer/view.rs", "Viewing"),
        ("viewer/view.rs", "Zoom"),
    ];
    let mut wrong = Vec::new();
    for (file, part) in PARTS {
        let lines = code_lines(&source(file));
        let heads = [
            format!("pub(crate) struct {part} {{"),
            format!("pub struct {part} {{"),
        ];
        let Some(start) = lines.iter().position(|l| heads.contains(l)) else {
            wrong.push(format!("{file}: no struct {part}"));
            continue;
        };
        let end = block_end(&lines, start).unwrap();
        for (i, line) in lines.iter().enumerate().take(end).skip(start + 1) {
            if indent(line) == 4 && line.trim_start().starts_with("pub") {
                wrong.push(format!("{file}:{}: {part}'s field", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "make these private, and give the part a method that says what happens:\n{}",
        wrong.join("\n")
    );
}

/// A function a reader takes in whole: at most 120 lines outside the tests. The exceptions are
/// tables of one arm per action (`table_is_short`), whose arms stay a few lines each: a longer
/// arm is a method of its own, in the module of what it does.
#[test]
fn functions_stay_short() {
    const MOST: usize = 120;
    const TABLES: &[(&str, &str)] = &[
        ("ui/actions.rs", "english"),
        ("ui/actions.rs", "icon"),
        ("ui/actions.rs", "target"),
        ("ui/actions.rs", "run"),
    ];
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        for i in 0..end {
            let Some(rest) = qualifiers_off(lines[i].trim_start()).strip_prefix("fn ") else {
                continue;
            };
            let name = rest.split(['(', '<']).next().unwrap_or("");
            // A declaration without a body (in a trait, an extern block), or a function on one
            // line: nothing to measure.
            let head_end = (i..end).find(|&j| lines[j].ends_with(['{', ';', '}']));
            if head_end.is_none_or(|j| !lines[j].ends_with('{')) {
                continue;
            }
            let Some(last) = block_end(&lines, i) else {
                continue;
            };
            if !TABLES.contains(&(file.as_str(), name)) {
                if last - i + 1 > MOST {
                    let n = last - i + 1;
                    wrong.push(format!("{file}:{}: {name} has {n} lines", i + 1));
                }
                continue;
            }
            if let Err(e) = table_is_short(&lines, i, last) {
                wrong.push(format!("{file}:{}: {name} {e}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "split these into functions of what each part does:\n{}",
        wrong.join("\n")
    );
}

/// A table, lines `first..=last` of a function: a few lines that set it up, then one `match`
/// that ends it, whose arms (pattern, guard and body, a pattern's `| …` lines aside) are short.
fn table_is_short(lines: &[String], first: usize, last: usize) -> Result<(), String> {
    const SETUP: usize = 20;
    const ARM: usize = 8;
    let solid = |range: std::ops::Range<usize>| range.filter(|&j| !lines[j].is_empty()).count();
    let body = indent(&lines[first]) + 4;
    // The last match at the body's indent, which must end the function.
    let table = (first + 1..last)
        .rev()
        .find(|&j| indent(&lines[j]) == body && lines[j].trim_start().starts_with("match "))
        .filter(|&m| {
            block_end(lines, m).is_some_and(|e| (e + 1..last).all(|j| lines[j].is_empty()))
        })
        .ok_or("does not end in its match")?;
    if solid(first..table) > SETUP {
        return Err("does much before its match".into());
    }
    // An arm starts at the arms' indent with anything but the rest of a pattern or a brace.
    let arm = indent(&lines[table]) + 4;
    let starts: Vec<usize> = (table + 1..last - 1)
        .filter(|&j| {
            indent(&lines[j]) == arm
                && !lines[j].trim_start().starts_with(['|', '{', '}', ')', ']'])
        })
        .chain([last - 1])
        .collect();
    for pair in starts.windows(2) {
        // A pattern's `| …` lines sit at the arms' indent; a `||` or closure in a body is deeper.
        let pattern = (pair[0]..pair[1])
            .filter(|&j| indent(&lines[j]) == arm && lines[j].trim_start().starts_with('|'));
        if solid(pair[0]..pair[1]) - pattern.count() > ARM {
            return Err(format!("has a long arm at line {}", pair[0] + 1));
        }
    }
    Ok(())
}

/// The first name of every `crate::` path in `lines`, with its line number: in `crate::{…}`,
/// also over several lines, the first name of each entry (nested braces belong to their
/// entry); `*` for a glob.
fn crate_paths(lines: &[String]) -> Vec<(usize, String)> {
    let code = lines.join("\n");
    let line_of = |at: usize| code[..at].matches('\n').count() + 1;
    let word = |c: char| c == '_' || c.is_alphanumeric();
    let mut paths = Vec::new();
    for (at, _) in code.match_indices("crate::") {
        // Not the end of another word, nor $crate in a macro.
        if code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c == '$' || word(c))
        {
            continue;
        }
        let rest = &code[at + "crate::".len()..];
        let Some(inner) = rest.strip_prefix('{') else {
            let name: String = rest.chars().take_while(|&c| word(c) || c == '*').collect();
            paths.push((line_of(at), name));
            continue;
        };
        let (mut depth, mut name, mut taking) = (1, String::new(), true);
        // Where the name being read starts in `code`, for its line.
        let mut entry = at;
        let base = code.len() - inner.len();
        for (from, c) in inner.char_indices().map(|(k, c)| (base + k, c)) {
            match c {
                '{' => (depth, taking) = (depth + 1, false),
                '}' => depth -= 1,
                ',' if depth == 1 => {
                    paths.push((line_of(entry), std::mem::take(&mut name)));
                    taking = true;
                }
                c if depth == 1 && taking && (word(c) || c == '*') => {
                    if name.is_empty() {
                        entry = from;
                    }
                    name.push(c)
                }
                _ if !name.is_empty() => taking = false,
                _ => {}
            }
            if depth == 0 {
                paths.push((line_of(entry), name));
                break;
            }
        }
    }
    paths.retain(|(_, name)| !name.is_empty());
    paths
}

/// A name is imported from the module that defines it, `crate::files::config::Config`, not through
/// main.rs's own imports (`crate::Config`), which hide where it lives and break when main.rs
/// stops using it; and never `crate::*`.
#[test]
fn imports_name_where_defined() {
    let files = rust_sources();
    let modules = root_names();
    let main = code_lines(&source("main.rs"));
    let main_end = tests_start(&main);
    let words = [
        "struct ", "enum ", "fn ", "type ", "const ", "static ", "trait ", "mod ",
    ];
    // main.rs's lines at the top level, and in a top-level extern block (malloc_trim).
    let mut in_extern = false;
    let top: Vec<&str> = (main[..main_end].iter())
        .filter(|l| {
            let opens = qualifiers_off(l).starts_with("extern ") && l.ends_with('{');
            in_extern = (in_extern || opens) && l.as_str() != "}";
            indent(l) == 0 || in_extern
        })
        .map(|l| qualifiers_off(l.trim_start()))
        .collect();
    // An item main.rs defines there, a macro exported to the crate root, or main.rs's tests
    // module. A macro that is not exported, such as config!, is no name at the root. The code
    // without its comments is searched, so an attribute that is commented out does not count.
    let code: Vec<String> = (files.iter())
        .map(|(_, t)| code_lines(t).join("\n"))
        .collect();
    let defined = |name: &str| {
        let defines = |l: &&str| {
            words.iter().any(|w| {
                (l.strip_prefix(w).and_then(|r| r.strip_prefix(name)))
                    .is_some_and(|r| !r.starts_with(|c: char| c == '_' || c.is_alphanumeric()))
            })
        };
        let macro_rule = format!("#[macro_export]\nmacro_rules! {name} ");
        top.iter().any(defines) || code.iter().any(|c| c.contains(&macro_rule)) || name == "tests"
    };
    let mut wrong = Vec::new();
    for (file, text) in &files {
        for (line, name) in crate_paths(&code_lines(text)) {
            if !modules.contains(&name) && !defined(&name) {
                wrong.push(format!("{file}:{line}: crate::{name}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "import these from the module that defines them:\n{}",
        wrong.join("\n")
    );
}

/// In a file's own code `super::` is the crate root, main.rs: the same way round its imports
/// as `crate::Name`. It is for the tests and inline modules, where it is the file's module.
#[test]
fn super_only_inside_modules() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];
            if qualifiers_off(line).starts_with("mod ") && line.ends_with('{') {
                i = block_end(&lines, i).unwrap_or(lines.len()) + 1;
                continue;
            }
            if line.contains("super::") {
                wrong.push(format!("{file}:{}", i + 1));
            }
            i += 1;
        }
    }
    assert!(
        wrong.is_empty(),
        "super:: outside the tests:\n{}",
        wrong.join("\n")
    );
}

/// main.rs imports from its modules as every other file does, `use crate::ui::actions::…`, not
/// `use actions::…`.
#[test]
fn main_names_its_modules_with_crate() {
    let modules = module_names();
    for (i, line) in code_lines(&source("main.rs")).iter().enumerate() {
        let first = (qualifiers_off(line).strip_prefix("use "))
            .and_then(|l| l.split("::").next())
            .filter(|m| modules.iter().any(|x| x == m));
        assert!(
            first.is_none(),
            "main.rs:{}: use crate::{}",
            i + 1,
            first.unwrap_or_default()
        );
    }
}

/// AGENTS.md's table under "Code layout" has a row for every source file, and no row for a
/// file that is gone: a module added or renamed is described where a reader looks first.
#[test]
fn agents_md_lists_every_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    let rows: Vec<&str> = (agents.lines())
        .filter_map(|l| l.strip_prefix("| `src/")?.split('`').next())
        .collect();
    let files: Vec<String> = rust_sources().into_iter().map(|(n, _)| n).collect();
    let missing: Vec<&String> = (files.iter())
        .filter(|f| !rows.contains(&f.as_str()))
        .collect();
    let gone: Vec<&&str> = (rows.iter())
        .filter(|r| !files.iter().any(|f| f == *r))
        .collect();
    assert!(
        missing.is_empty() && gone.is_empty(),
        "AGENTS.md's code layout lacks {missing:?} and names {gone:?}, which are not in src/"
    );
}

/// Each plain name in backticks in a row of AGENTS.md's code layout is defined in that row's
/// file, is in one of its string literals (a program's name), or is a feature in Cargo.toml.
/// So a function moved to another module takes its name to that module's row too, even when
/// the old module still calls it. Paths, macros and options in backticks are left out.
#[test]
fn agents_md_names_what_each_module_has() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let feature = |n: &str| cargo.contains(&format!("\n{n} = ["));
    let plain = |t: &&str| {
        t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let words = |t: &str| -> Vec<String> {
        (t.split(|c: char| !c.is_ascii_alphanumeric() && c != '_'))
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let kinds = [
        "fn", "struct", "enum", "type", "const", "static", "trait", "mod",
    ];
    let mut wrong = Vec::new();
    for row in agents.lines() {
        let Some((file, rest)) = row.strip_prefix("| `src/").and_then(|r| r.split_once('`')) else {
            continue;
        };
        let text = source(file);
        let code = words(&code_lines(&text).join("\n"));
        let mut known: std::collections::HashSet<String> = (code.windows(2))
            .filter(|w| kinds.contains(&w[0].as_str()) || w[0] == "macro_rules")
            .map(|w| w[1].clone())
            .collect();
        // The words inside string literals, outside comments: a program's name, say.
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            known.extend(line.split('"').skip(1).step_by(2).flat_map(words));
        }
        let names = rest.split('`').skip(1).step_by(2).filter(plain);
        wrong.extend(
            names
                .filter(|n| !known.contains(*n) && !feature(n))
                .map(|n| format!("{file}: {n}")),
        );
    }
    assert!(
        wrong.is_empty(),
        "AGENTS.md's code layout names what is not defined in that file:\n{}",
        wrong.join("\n")
    );
}

/// A panic is caught only by `threads::caught`, the one function whose doc says why. A worker
/// that caught panics its own way once forgot it, and a broken file ended the thread. Eight
/// commits added the catch one path at a time.
#[test]
fn panics_are_caught_in_one_place() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        for (i, line) in lines[..end].iter().enumerate() {
            if line.contains("catch_unwind") && !file.ends_with("threads.rs") {
                wrong.push(format!("{file}:{}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a panic caught outside threads::caught (use it instead):\n{}",
        wrong.join("\n")
    );
}

/// A decoded image is turned by its EXIF orientation only after it is shrunk to the size shown
/// (`decode::shrunk_upright`), or where the whole image is wanted. A thumbnail of a 50 MP 16-bit
/// TIFF with orientation 6 turned whole held two full buffers in each thumbnail thread.
#[test]
fn images_are_turned_after_they_shrink() {
    // Each place that may turn, and why: the scaled JPEG is at its scaled size already, and the
    // others are for a whole image (Save As, a turned save, a RAW preview shown near its size).
    let allowed = [
        "images/decode.rs shrunk_upright",
        "images/decode.rs scaled",
        "images/decode.rs load_icc",
        "images/raw.rs load_data",
    ];
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let mut function = "";
        for (i, line) in lines[..tests_start(&lines)].iter().enumerate() {
            if let Some(name) = function_name(line) {
                function = name;
            }
            let place = format!("{file} {function}");
            if line.contains("apply_orientation(") && !allowed.contains(&place.as_str()) {
                wrong.push(format!("{file}:{} in {function}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "an image turned before it is shrunk (use decode::shrunk_upright):\n{}",
        wrong.join("\n")
    );
}

/// A process-wide copy of a setting is written only by its setter, so no place that changes the
/// setting can leave the copy behind. A setting saved but not in effect until a restart was
/// fixed three times for the user's own keys.
#[test]
fn copies_of_settings_have_one_writer() {
    const WRITERS: &[(&str, &[(&str, &str)])] = &[
        ("OWN_KEYS.write()", &[("ui/actions.rs", "use_own_keys")]),
        (
            "MAX_ANIMATION.store(",
            &[("images/decode.rs", "set_animation_limit")],
        ),
        (
            "VIEW_FILTER.store(",
            &[
                ("images/colour.rs", "set_up"),
                ("ui/menus.rs", "set_view_filter"),
            ],
        ),
        (
            "COLOR_MANAGEMENT.store(",
            &[
                ("images/colour.rs", "set_up"),
                ("ui/menus.rs", "set_color_management"),
            ],
        ),
    ];
    let wrong = outside_their_homes(WRITERS);
    assert!(
        wrong.is_empty(),
        "a copy of a setting written by hand:\n{}",
        wrong.join("\n")
    );
}

/// Returns each place outside the tests where a text of `homes` is written in code outside the
/// functions it is allowed in, as "file:line (text)". A function named "" allows the whole
/// file, and the text's own definition is allowed anywhere.
fn outside_their_homes(homes: &[(&str, &[(&str, &str)])]) -> Vec<String> {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        let mut function = String::new();
        for (i, line) in lines[..end].iter().enumerate() {
            if let Some(name) = function_name(line) {
                function = name.to_owned();
            }
            for (call, allowed) in homes {
                let at_home =
                    (allowed.iter()).any(|(f, s)| file == *f && (s.is_empty() || function == *s));
                let defined = line.contains(&format!("fn {call}"));
                if line.contains(call) && !at_home && !defined {
                    wrong.push(format!("{file}:{} ({call})", i + 1));
                }
            }
        }
    }
    wrong
}

/// A file Glimt writes from an image (a turn saved, a crop, a copy, their sidecars) takes the
/// image's metadata only through `save::derived_metadata` and `save::derived_xmp`, which say
/// what such a file keeps. The EXIF thumbnail was cleared there, but the XMP's thumbnail and a
/// PNG's Photoshop resources were copied as they were, because each kind was decided apart. The
/// readers that give an image's XMP as it is are watched too, because a copy written from one
/// would skip what `derived_xmp` drops. The panel reads it in `xmp::embedded`.
#[test]
fn derived_files_take_metadata_in_one_place() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        ("set_upright(", &[("images/save.rs", "derived_metadata")]),
        (
            "xmp_metadata(",
            &[
                ("images/save.rs", "derived_metadata"),
                ("metadata/xmp.rs", "embedded"),
            ],
        ),
        ("png_with_xmp(", &[("images/save.rs", "encode")]),
        ("set_xmp_orientation(", &[("images/save.rs", "derived_xmp")]),
        ("set_exif_metadata(", &[("images/save.rs", "encode")]),
        (
            "own_sidecar(",
            &[
                ("metadata/xmp.rs", ""),
                ("images/save.rs", "land"),
                ("images/save.rs", "derive_sidecar"),
            ],
        ),
    ];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "metadata copied to a derived file around save::derived_metadata or derived_xmp:\n{}",
        wrong.join("\n")
    );
}

/// An image written again and its sidecar change together, through `save::replace_with_sidecar`,
/// which changes the sidecar only once the image's own write can no longer fail. Before, a turn
/// saved changed the sidecar first, so a read-only image kept its pixels while its sidecar said
/// it was turned.
#[test]
fn an_image_and_its_sidecar_change_together() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        (
            "derive_sidecar(",
            &[("images/save.rs", "replace_with_sidecar")],
        ),
        (
            "replace_existing(",
            &[
                ("files/file_io.rs", ""),
                ("images/save.rs", "replace_with_sidecar"),
            ],
        ),
    ];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "an image or its sidecar written around save::replace_with_sidecar:\n{}",
        wrong.join("\n")
    );
}

/// The view's colour settings (Preferences' profiles, colour management on or off and the view
/// filter) reach only what is shown. Everything Glimt writes or hands to other programs (the
/// shared thumbnail cache, a print, the clipboard, an exported frame) goes through
/// `colour::to_srgb`, which reads none of them. Before, the settings changed the colours of the
/// shared cache and of prints.
#[test]
fn view_colours_stay_in_the_view() {
    const FOR_THE_VIEW: &[&str] = &[
        "to_display(",
        "COLOUR.read(",
        "COLOR_MANAGEMENT.load(",
        "VIEW_FILTER.load(",
    ];
    const SHOWN: &[(&str, &str)] = &[
        ("images/decode.rs", "decode"),
        ("images/decode.rs", "animation"),
        ("ui/workers.rs", "spawn_thumbnailers"),
    ];
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        if file == "images/colour.rs" {
            continue;
        }
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        let mut function = String::new();
        for (i, line) in lines[..end].iter().enumerate() {
            if let Some(name) = function_name(line) {
                function = name.to_owned();
            }
            let shown = SHOWN.iter().any(|(f, s)| file == *f && function == *s);
            if FOR_THE_VIEW.iter().any(|t| line.contains(t)) && !shown {
                wrong.push(format!("{file}:{} in {function}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the view's colour settings used outside the view (use colour::to_srgb):\n{}",
        wrong.join("\n")
    );
}

/// A dialog or window reads its Enter or Esc only through `App::answers`, which leaves the key to
/// the widget with the keyboard focus. Read from the input as a whole, the key also answered
/// while egui gave it to that widget: Shift+Delete, Tab to "Cancel" and Enter deleted the file
/// for good. A text field's own Enter (`lost_focus()` on the same line) is the field's, and the
/// shortcuts' table reads its keys in `read_keys` only while nothing has the focus. The places
/// below own their key while a widget may have the focus.
#[test]
fn keys_answer_through_one_place() {
    const READS: &[&str] = &["key_pressed(", "consume_key(", "Event::Key", "key_down("];
    const OWNERS: &[(&str, &str)] = &[
        // Esc in Preferences' search field clears the search first, and closes it after.
        ("ui/preferences.rs", "preferences_window"),
        // The projector's own window has no widget that takes the focus.
        ("viewer/view.rs", "projector_window"),
    ];
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        let mut function = String::new();
        for (i, line) in lines[..end].iter().enumerate() {
            if let Some(name) = function_name(line) {
                function = name.to_owned();
            }
            let key = line.contains("Key::Enter") || line.contains("Key::Escape");
            let read = READS.iter().any(|r| line.contains(r));
            let owner = OWNERS.iter().any(|(f, s)| file == *f && function == *s);
            if key && read && !owner && !line.contains("lost_focus()") {
                wrong.push(format!("{file}:{} in {function}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "Enter or Esc read from the whole input (use App::answers):\n{}",
        wrong.join("\n")
    );
}

/// Every module lives in the folder of what it does, so `src/` itself holds only main.rs, cli.rs
/// and one file for each folder. A folder's file holds its `//!` doc and its `mod` lines and
/// nothing else. A `pub use` there would let other files import a name through the folder, which
/// hides the module that defines it.
#[test]
fn modules_live_in_their_folder() {
    const ROOT: &[&str] = &["main.rs", "cli.rs"];
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut wrong = Vec::new();
    for (path, text) in rust_sources() {
        if ROOT.contains(&path.as_str()) {
            continue;
        }
        // A folder's file is the one beside a folder of its name, at any depth.
        if !src.join(path.trim_end_matches(".rs")).is_dir() {
            if !path.contains('/') {
                wrong.push(format!("{path}: put it in the folder of what it does"));
            }
            continue;
        }
        let lines = code_lines(&text);
        for (i, line) in lines.iter().enumerate() {
            let name = qualifiers_off(line)
                .strip_prefix("mod ")
                .and_then(|l| l.strip_suffix(';'));
            let is_mod = name.is_some_and(|n| n.chars().all(|c| c == '_' || c.is_alphanumeric()));
            let is_cfg = line.starts_with("#[cfg(") && line.ends_with(")]") && !line.contains("] ");
            if !line.is_empty() && !is_mod && !is_cfg {
                wrong.push(format!(
                    "{path}:{}: a folder's file holds only mod lines",
                    i + 1
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A test that feeds `test_support::broken` is named `survives_broken_*`. The weekly run with
/// five million broken inputs (`.github/workflows/parsers.yml`) picks its tests by that name,
/// and it would skip one named otherwise. Every spelling of the call counts, imported or by its
/// whole path.
#[test]
fn broken_inputs_are_fed_by_survives_broken_tests() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        if file.ends_with("test_support.rs") {
            continue;
        }
        let mut function = String::new();
        for (i, line) in code_lines(&text).iter().enumerate() {
            if let Some(name) = function_name(line) {
                function = name.to_owned();
            }
            let calls = line.match_indices("broken(").any(|(at, _)| {
                let before = line[..at].chars().next_back();
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
            if calls && !function.starts_with("survives_broken") {
                wrong.push(format!("{file}:{} in {function}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "test_support::broken fed outside a survives_broken_* test:\n{}",
        wrong.join("\n")
    );
}

/// A comment fits the 100 columns rustfmt keeps code to, which rustfmt does not wrap: a longer
/// one is read sideways, or wrapped by an editor in the middle of a word. Also one at the end
/// of a line of code.
#[test]
fn comments_fit_the_width() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        for (i, (line, code)) in text.lines().zip(code_lines(&text)).enumerate() {
            // Where the code ends, the rest of the line is a comment, not a string going on.
            let rest: String = line.chars().skip(code.chars().count()).collect();
            if line.chars().count() > 100 && rest.trim_start().starts_with("//") {
                wrong.push(format!("{file}:{}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "comments over 100 columns:\n{}",
        wrong.join("\n")
    );
}

/// The .deb and the .rpm install the same files, and the AppImage, the tar.gz and the Flatpak
/// each of them that is not documentation: five lists kept in step by this test, not by memory.
#[test]
fn packages_install_the_same_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
    let cargo = read("Cargo.toml");
    let deb = cargo.split_once("[package.metadata.deb]").unwrap().1;
    let (deb, rpm) = deb.split_once("[package.metadata.generate-rpm]").unwrap();
    fn quoted<'a>(line: &'a str, start: &str) -> Option<&'a str> {
        line.split_once(start)?.1.split('"').next()
    }
    // The .deb's license-file is its LICENSE.
    let mut deb: Vec<&str> = deb.lines().filter_map(|l| quoted(l, "[\"")).collect();
    let mut rpm: Vec<&str> = rpm
        .lines()
        .filter_map(|l| quoted(l, "source = \""))
        .collect();
    deb.sort();
    rpm.sort();
    assert_eq!(
        deb, rpm,
        "Cargo.toml: the .deb's and the .rpm's assets differ"
    );
    let release = read(".github/workflows/release.yml");
    let step = |name: &str| {
        let at = release.find(&format!("- name: {name}\n")).unwrap();
        release[at..]
            .split("\n      - name: ")
            .next()
            .unwrap()
            .to_owned()
    };
    let others = [
        ("the tar.gz", step("tar.gz")),
        ("the AppImage", step("AppImage")),
        ("the Flatpak", read("flatpak/io.github.thornbux.Glimt.yml")),
    ];
    let docs = ["README.md", "CHANGELOG.md", "examples/"];
    let mut wrong = Vec::new();
    for file in rpm
        .iter()
        .filter(|f| !docs.iter().any(|d| f.starts_with(d)))
    {
        // The binary is built to target/<the target>/release in the release workflow.
        let name = file.trim_start_matches("target/");
        for (package, text) in &others {
            if !text.contains(name) {
                wrong.push(format!("{package}: {file}"));
            }
        }
    }
    assert!(wrong.is_empty(), "not installed:\n{}", wrong.join("\n"));
}

/// Every optional feature is in `full`, which the packages are built with, and has a row in
/// README's table. CI builds with --all-features, so a feature left out of `full` would pass
/// every check and still be missing from every package.
#[test]
fn full_has_every_feature() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
    let cargo = read("Cargo.toml");
    let readme = read("README.md");
    let features = cargo.split_once("\n[features]\n").unwrap().1;
    let features = features.split("\n[").next().unwrap();
    let names = features
        .lines()
        .filter_map(|l| Some(l.split_once(" = ")?.0));
    let full = features.lines().find(|l| l.starts_with("full = ")).unwrap();
    let mut wrong = Vec::new();
    for name in names.filter(|n| !["full", "test-hooks"].contains(n)) {
        if !full.contains(&format!("\"{name}\"")) {
            wrong.push(format!("Cargo.toml: {name} is not in full"));
        }
        if !readme.contains(&format!("\n| `{name}` |")) {
            wrong.push(format!(
                "README.md: {name} has no row in the features table"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The file list is drawn in two places at once, the side panel and the grid in the image area,
/// each in its own form. The functions that draw it take that form from `file_list`'s
/// parameters, so a setting read there would draw the side panel as the grid, or the other way.
/// The settings are read only where the side panel picks its form and where they are set, so a
/// helper the drawing calls cannot read them either, whatever the settings are called through.
#[test]
fn the_list_is_drawn_in_the_form_its_caller_asks() {
    const HOMES: &[&str] = &["side_panel", "set_thumbnails"];
    let lines = code_lines(&source("ui/file_list.rs"));
    let end = tests_start(&lines);
    let mut function = String::new();
    let mut wrong = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        if let Some(name) = function_name(line) {
            function = name.to_owned();
        }
        let setting = [".thumbnails", ".icons"].iter().any(|s| {
            (line.match_indices(s)).any(|(at, _)| {
                let after = line[at + s.len()..].chars().next();
                !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
        });
        if setting && !HOMES.contains(&function.as_str()) {
            wrong.push(format!("ui/file_list.rs:{} in {function}", i + 1));
        }
    }
    assert!(
        wrong.is_empty(),
        "the list's form read from the settings, not from file_list's parameters:\n{}",
        wrong.join("\n")
    );
}

/// Every widget drawn by hand that takes a click tells a screen reader what it is: a row or
/// cell of the list through `speak_item`, the image through `speak_image`, and the others with
/// `widget_info`. egui's own widgets do this themselves, but a widget drawn by hand says
/// nothing, and Orca was silent in the grid in the image area.
#[test]
fn every_clickable_widget_drawn_by_hand_speaks() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        for (i, line) in lines[..end].iter().enumerate() {
            let near = &lines[i..(i + 4).min(end)];
            let speaks =
                || (near.iter()).any(|l| l.contains("speak_") || l.contains("widget_info"));
            if takes_a_click(line) && !speaks() {
                wrong.push(format!("{file}:{}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a widget that takes a click with nothing for a screen reader:\n{}",
        wrong.join("\n")
    );
}
