//! Checks of the source itself, run as tests: the layout AGENTS.md describes under "Code
//! layout", so a file cannot drift from it unnoticed.

use std::path::Path;

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

/// `text` with its comments and the insides of its string and char literals blanked, line for
/// line, so a check reads code only: a line of a string or a comment is never taken for an
/// item, an import or a function.
fn code_only(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let ident = |k: usize| k < b.len() && (b[k] == '_' || b[k].is_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let (c, next) = (b[i], b.get(i + 1).copied());
        // How many # follow an r that starts a raw string (r"…", r#"…"#, br"…"), if one does.
        let raw = (c == 'r'
            && (i == 0 || !ident(i - 1) || (b[i - 1] == 'b' && (i < 2 || !ident(i - 2)))))
        .then(|| b[i + 1..].iter().take_while(|&&h| h == '#').count())
        .filter(|&h| b.get(i + 1 + h) == Some(&'"'));
        let end = if c == '/' && next == Some('/') {
            (i..b.len()).find(|&k| b[k] == '\n').unwrap_or(b.len())
        } else if c == '/' && next == Some('*') {
            (i + 2..b.len())
                .find(|&k| b[k - 1] == '*' && b[k] == '/')
                .map_or(b.len(), |k| k + 1)
        } else if let Some(h) = raw {
            let close: Vec<char> = std::iter::once('"')
                .chain(std::iter::repeat_n('#', h))
                .collect();
            (i + 2 + h..b.len())
                .find(|&k| b[k..].starts_with(&close))
                .map_or(b.len(), |k| k + close.len())
        } else if c == '"' {
            let mut k = i + 1;
            while k < b.len() && b[k] != '"' {
                k += if b[k] == '\\' { 2 } else { 1 };
            }
            k + 1
        } else if c == '\'' && (next == Some('\\') || b.get(i + 2) == Some(&'\'')) {
            // A char literal, not a lifetime.
            (i + 2..b.len())
                .find(|&k| b[k] == '\'')
                .map_or(b.len(), |k| k + 1)
        } else {
            out.push(c);
            i += 1;
            continue;
        };
        let end = end.min(b.len());
        out.extend(b[i..end].iter().map(|&c| blank(c)));
        i = end;
    }
    out
}

/// `line` without what may come before `fn`, `struct` and the like: visibility, `unsafe`,
/// `async`, and `const` when a function follows (a const fn is a function, not a constant).
fn qualifiers_off(line: &str) -> &str {
    let mut l = line;
    loop {
        let before = l;
        for prefix in ["pub(crate) ", "pub(super) ", "pub ", "unsafe ", "async "] {
            l = l.strip_prefix(prefix).unwrap_or(l);
        }
        if let Some(rest) = l
            .strip_prefix("const ")
            .filter(|r| qualifiers_off(r).starts_with("fn "))
        {
            l = rest;
        }
        if l == before {
            return l;
        }
    }
}

/// The place of a top-level item in a file's order, from the line it starts on; `None` for a
/// line that starts none (a comment, an attribute, the rest of an item).
fn rank(line: &str) -> Option<(u8, &'static str)> {
    let l = qualifiers_off(line);
    let starts = |word: &str| {
        l.strip_prefix(word)
            .is_some_and(|rest| rest.starts_with([' ', '<', '!', '(', '"']))
    };
    let for_app = l.contains(" for App ") || l.contains(" for crate::App ");
    Some(
        if starts("use") || (starts("mod") && !l.starts_with("mod tests")) {
            (0, "use and mod")
        } else if starts("macro_rules") {
            (0, "use and mod")
        } else if starts("const") || starts("static") {
            (1, "const and static")
        } else if l == "impl App {" || l == "impl crate::App {" || (starts("impl") && for_app) {
            (3, "the impl crate::App block")
        } else if ["struct", "enum", "type", "trait", "union", "impl"]
            .iter()
            .any(|w| starts(w))
        {
            (2, "types and their impls")
        } else if starts("fn") || starts("extern") {
            (4, "functions")
        } else if l.starts_with("mod tests") {
            (5, "the tests")
        } else if l.split_once('!').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }) {
            (2, "types and their impls")
        } else {
            return None;
        },
    )
}

/// Each file's items in AGENTS.md's order: use and mod, const and static, types with their
/// impls, one `impl crate::App` block, functions, and the tests last.
#[test]
fn items_are_in_order() {
    let mut wrong = Vec::new();
    for (name, text) in sources().into_iter().filter(|(n, _)| n.ends_with(".rs")) {
        let (mut last, mut last_what, mut apps) = (0, "use and mod", 0);
        let code = code_only(&text);
        let mut in_tests = false;
        for (i, line) in code.lines().enumerate() {
            // Inside the tests module nothing is ranked; after its end, everything is.
            if in_tests {
                in_tests = line != "}";
                continue;
            }
            let Some((r, what)) = rank(line) else {
                continue;
            };
            if line == "impl App {" || line == "impl crate::App {" {
                apps += 1;
                if apps == 2 {
                    wrong.push(format!("{name}:{}: a second impl crate::App block", i + 1));
                }
            }
            if r < last {
                wrong.push(format!("{name}:{}: {what} after {last_what}", i + 1));
            }
            (last, last_what) = (r, what);
            in_tests = r == 5 && !line.trim_end().ends_with('}');
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
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap();
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
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")).unwrap();
    let app = text.split("\nstruct App {\n").nth(1).unwrap();
    let app = app.split("\n}\n").next().unwrap();
    let fields = (app.lines())
        .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
        .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("#["))
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
    for (file, text) in sources().into_iter().filter(|(n, _)| n.ends_with(".rs")) {
        let code = code_only(&text);
        let lines: Vec<&str> = code.lines().collect();
        let end = (lines.iter())
            .position(|l| l.starts_with("mod tests"))
            .unwrap_or(lines.len());
        for i in 0..end {
            let line = lines[i];
            let indent = line.len() - line.trim_start().len();
            let Some(rest) = qualifiers_off(line.trim_start()).strip_prefix("fn ") else {
                continue;
            };
            if line.trim_end().ends_with(';') {
                continue;
            }
            let name = rest.split(['(', '<']).next().unwrap_or("");
            // On one line: nothing to measure, and its indent's next `}` is another item's.
            if line.trim_end().ends_with('}') {
                continue;
            }
            let close = format!("{}}}", " ".repeat(indent));
            let Some(last) = (i..end).find(|&j| lines[j] == close) else {
                continue;
            };
            if TABLES.contains(&(file.as_str(), name)) {
                for k in i..last {
                    // An arm's block opens after `=>`, or on a line of its own after a guard.
                    // (`=> {`, `=> if … {`, `=> match … {`).
                    let opens = (lines[k].contains("=> ") && lines[k].trim_end().ends_with('{'))
                        || (lines[k].trim() == "{" && lines[k - 1].trim_end().ends_with("=>"));
                    if !opens {
                        continue;
                    }
                    let pad = lines[k].len() - lines[k].trim_start().len();
                    let shut = (k..last).find(|&j| {
                        lines[j].len() - lines[j].trim_start().len() == pad
                            && lines[j].trim_start().starts_with('}')
                    });
                    if shut.is_some_and(|j| j - k > ARM) {
                        wrong.push(format!("{file}:{}: an arm of {name} is long", k + 1));
                    }
                }
            } else if last - i + 1 > MOST {
                let n = last - i + 1;
                wrong.push(format!("{file}:{}: {name} has {n} lines", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "split these into functions of what each part does:\n{}",
        wrong.join("\n")
    );
}

/// A name is imported from the module that defines it, `crate::config::Config`, not through
/// main.rs's own imports (`crate::Config`), which hide where it lives and break when main.rs
/// stops using it. main.rs names its modules the same way, `crate::actions::…`.
#[test]
fn imports_name_where_defined() {
    let all = sources();
    let rust = || all.iter().filter(|(n, _)| n.ends_with(".rs"));
    let modules: Vec<&str> = rust().map(|(n, _)| n.trim_end_matches(".rs")).collect();
    let main = &all.iter().find(|(n, _)| n == "main.rs").unwrap().1;
    let main_code = code_only(main);
    let main_items = main_code.split("\nmod tests {").next().unwrap();
    // What main.rs defines itself, the macros, and main.rs's tests module.
    let defined = |name: &str| {
        let words = [
            "struct ", "enum ", "fn ", "type ", "const ", "static ", "trait ", "mod ",
        ];
        let defines = |l: &str| {
            let l = qualifiers_off(l.trim_start());
            words.iter().any(|w| {
                l.strip_prefix(w).is_some_and(|r| {
                    r.starts_with(name)
                        && !r[name.len()..].starts_with(|c: char| c == '_' || c.is_alphanumeric())
                })
            })
        };
        main_items.lines().any(defines)
            || rust().any(|(_, t)| t.contains(&format!("macro_rules! {name} ")))
            || name == "tests"
    };
    let first_word = |s: &str| -> String {
        let s = s.trim_start();
        s.chars()
            .take_while(|c| *c == '_' || c.is_alphanumeric())
            .collect()
    };
    let mut wrong = Vec::new();
    for (file, text) in rust() {
        let code = code_only(text);
        let line_of = |at: usize| code[..at].matches('\n').count() + 1;
        // Every `crate::` path: its first name, or in `crate::{…}` (over several lines too)
        // the first name of each entry at the top of the braces.
        for (at, _) in code.match_indices("crate::") {
            let before = code[..at].chars().next_back();
            if before.is_some_and(|c| c == '$' || c == '_' || c.is_alphanumeric()) {
                continue;
            }
            let rest = &code[at + "crate::".len()..];
            let mut names = Vec::new();
            match rest.strip_prefix('{') {
                Some(inner) => {
                    // The first name of each entry at depth 1; a nested `{…}` is that name's.
                    let (mut depth, mut name, mut taking) = (1, String::new(), true);
                    for c in inner.chars() {
                        match c {
                            '{' => (depth, taking) = (depth + 1, false),
                            '}' => depth -= 1,
                            ',' if depth == 1 => {
                                names.push(std::mem::take(&mut name));
                                taking = true;
                            }
                            c if depth == 1 && taking && (c == '_' || c.is_alphanumeric()) => {
                                name.push(c)
                            }
                            _ if !name.is_empty() => taking = false,
                            _ => {}
                        }
                        if depth == 0 {
                            names.push(name);
                            break;
                        }
                    }
                }
                None => names.push(first_word(rest)),
            }
            for name in &names {
                if !name.is_empty() && !modules.contains(&name.as_str()) && !defined(name) {
                    wrong.push(format!("{file}:{}: crate::{name}", line_of(at)));
                }
            }
        }
        // A file's super is the crate root, main.rs, outside its tests and inline modules: the
        // same way round its imports.
        let mut inside = false;
        for (i, line) in code.lines().enumerate() {
            if inside {
                inside = line != "}";
                continue;
            }
            let item = qualifiers_off(line);
            if item.starts_with("mod ") && line.trim_end().ends_with('{') {
                inside = true;
                continue;
            }
            if line.contains("super::") {
                wrong.push(format!("{file}:{}: super:: outside the tests", i + 1));
            }
            let bare = line.strip_prefix("use ").map(first_word);
            if file == "main.rs" && bare.is_some_and(|m| modules.contains(&m.as_str())) {
                wrong.push(format!("main.rs:{}: use without crate::", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "import these from the module that defines them:\n{}",
        wrong.join("\n")
    );
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
    let files: Vec<String> = (sources().into_iter())
        .map(|(n, _)| n)
        .filter(|n| n.ends_with(".rs"))
        .collect();
    let missing: Vec<&String> = files
        .iter()
        .filter(|f| !rows.contains(&f.as_str()))
        .collect();
    let gone: Vec<&&str> = rows
        .iter()
        .filter(|r| !files.iter().any(|f| f == *r))
        .collect();
    assert!(
        missing.is_empty() && gone.is_empty(),
        "AGENTS.md's code layout lacks {missing:?} and names {gone:?}, which are not in src/"
    );
}

/// A comment fits the 100 columns rustfmt keeps code to, which rustfmt does not wrap: a longer
/// one is read sideways, or wrapped by an editor in the middle of a word.
#[test]
fn comments_fit_the_width() {
    let mut wrong = Vec::new();
    for (file, text) in sources().into_iter().filter(|(n, _)| n.ends_with(".rs")) {
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") && line.chars().count() > 100 {
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
