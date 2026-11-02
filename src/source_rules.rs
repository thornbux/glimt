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

/// Every source file's name and text, read from `src/`: the Rust files and the translations
/// (`.po`). A file added is checked without being listed anywhere.
pub(crate) fn sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<(String, String)> = std::fs::read_dir(&src)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".rs") || name.ends_with(".po"))
        .map(|name| {
            let text = std::fs::read_to_string(src.join(&name)).unwrap();
            (name, text)
        })
        .collect();
    files.sort();
    files
}

/// The Rust files of `sources`.
fn rust_sources() -> Vec<(String, String)> {
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

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The line of the `}` that closes the block whose first line is `i`: the next line at the same
/// indent that starts with `}`.
fn block_end(lines: &[String], i: usize) -> Option<usize> {
    let closes = |j: &usize| indent(&lines[*j]) == indent(&lines[i]);
    (i + 1..lines.len()).find(|j| closes(j) && lines[*j].trim_start().starts_with('}'))
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

/// A test sits in the tests of the module whose code it tests. main.rs's tests module has what
/// such tests start from (`app_on`, `settle`), which drew every test that needed a whole `App`
/// into it once; only the tests of main.rs's own code stay there.
#[test]
fn tests_sit_beside_their_code() {
    const OWN: &[&str] = &[
        "an_app_lists_its_folder_without_a_window",
        "tells_settings_problems_but_unknown_ones",
        "parses_start_options",
        "new_window_program",
    ];
    let text = source("main.rs");
    let tests = text.split("\nmod tests {").nth(1).unwrap();
    let mut lines = tests.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "#[test]" {
            // Past other attributes, such as #[ignore], to the function.
            let name = (lines.by_ref())
                .map(str::trim)
                .find(|l| !l.starts_with("#["))
                .unwrap();
            let name = name.trim_start_matches("fn ").split('(').next().unwrap();
            assert!(
                OWN.contains(&name),
                "{name} is in main.rs: put it in the tests of the module it tests, with \
                 crate::tests::app_on for an App, or list it in OWN if it tests main.rs"
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

/// A function a reader takes in whole: at most 120 lines outside the tests. The exceptions are
/// tables of one arm per action, whose arms stay a few lines each: a longer arm is a method
/// of its own, in the module of what it does.
#[test]
fn functions_stay_short() {
    const MOST: usize = 120;
    const TABLES: &[(&str, &str)] = &[
        ("actions.rs", "english"),
        ("actions.rs", "icon"),
        ("actions.rs", "run"),
    ];
    const ARM: usize = 6;
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = (lines.iter())
            .position(|l| l == "mod tests {")
            .unwrap_or(lines.len());
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
            for k in (i..last).filter(|&k| lines[k].contains("=>")) {
                // From the `=>` line, or the `{` after a guard, to where the next arm starts.
                let start = if lines[k + 1].trim() == "{" { k + 1 } else { k };
                let stop = (start + 1..=last)
                    .find(|&j| indent(&lines[j]) <= indent(&lines[start]))
                    .unwrap_or(last);
                let closes = lines[stop].trim_start().starts_with(['}', ')', ']']);
                if stop - start + usize::from(closes) > ARM {
                    wrong.push(format!("{file}:{}: an arm of {name} is long", k + 1));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "split these into functions of what each part does:\n{}",
        wrong.join("\n")
    );
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
        for c in inner.chars() {
            match c {
                '{' => (depth, taking) = (depth + 1, false),
                '}' => depth -= 1,
                ',' if depth == 1 => {
                    paths.push((line_of(at), std::mem::take(&mut name)));
                    taking = true;
                }
                c if depth == 1 && taking && (word(c) || c == '*') => name.push(c),
                _ if !name.is_empty() => taking = false,
                _ => {}
            }
            if depth == 0 {
                paths.push((line_of(at), name));
                break;
            }
        }
    }
    paths.retain(|(_, name)| !name.is_empty());
    paths
}

/// A name is imported from the module that defines it, `crate::config::Config`, not through
/// main.rs's own imports (`crate::Config`), which hide where it lives and break when main.rs
/// stops using it; and never `crate::*`.
#[test]
fn imports_name_where_defined() {
    let files = rust_sources();
    let modules: Vec<&str> = files
        .iter()
        .map(|(n, _)| n.trim_end_matches(".rs"))
        .collect();
    let main = code_lines(&source("main.rs"));
    let main_end = (main.iter())
        .position(|l| l == "mod tests {")
        .unwrap_or(main.len());
    // An item main.rs defines at its top level, a macro, or main.rs's tests module.
    let words = [
        "struct ", "enum ", "fn ", "type ", "const ", "static ", "trait ", "mod ",
    ];
    let defined = |name: &str| {
        let defines = |l: &&String| {
            let l = qualifiers_off(l.trim_start());
            words.iter().any(|w| {
                (l.strip_prefix(w).and_then(|r| r.strip_prefix(name)))
                    .is_some_and(|r| !r.starts_with(|c: char| c == '_' || c.is_alphanumeric()))
            })
        };
        // At the top level, or declared in a top-level extern block (malloc_trim).
        let mut in_extern = false;
        let top: Vec<&String> = (main[..main_end].iter())
            .filter(|l| {
                let opens = qualifiers_off(l).starts_with("extern ") && l.ends_with('{');
                in_extern = (in_extern || opens) && l.as_str() != "}";
                indent(l) == 0 || in_extern
            })
            .collect();
        top.iter().any(defines)
            || files
                .iter()
                .any(|(_, t)| t.contains(&format!("macro_rules! {name} ")))
            || name == "tests"
    };
    let mut wrong = Vec::new();
    for (file, text) in &files {
        for (line, name) in crate_paths(&code_lines(text)) {
            if !modules.contains(&name.as_str()) && !defined(&name) {
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

/// main.rs names its modules as every other file does, `crate::actions::…`, not `actions::…`.
#[test]
fn main_names_its_modules_with_crate() {
    let modules: Vec<String> = (rust_sources().into_iter())
        .map(|(n, _)| n.trim_end_matches(".rs").to_owned())
        .collect();
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
