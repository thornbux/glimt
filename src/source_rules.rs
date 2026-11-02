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

/// The place of a top-level item in a file's order, from the line it starts on; `None` for a
/// line that starts none (a comment, an attribute, the rest of an item).
fn rank(line: &str) -> Option<(u8, &'static str)> {
    let mut l = line;
    for prefix in [
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "unsafe ",
        "async ",
        "const fn",
    ] {
        if let Some(rest) = l.strip_prefix(prefix) {
            l = if prefix == "const fn" { "fn" } else { rest };
        }
    }
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
        for (i, line) in text.lines().enumerate() {
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
            if r == 5 {
                break;
            }
        }
    }
    assert!(wrong.is_empty(), "out of order:\n{}", wrong.join("\n"));
}
