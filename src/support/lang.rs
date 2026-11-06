//! The interface in the user's language. The English texts are the keys: `tr("Open File…")`,
//! `tf!("Could not save {name}: {e}", name, e)` for texts with values, and `plural` for
//! `count`'s nouns. A translation is a gettext `.po` file built in (`po/no_nb.po` …); a text it
//! lacks shows in English, mixed with the translated ones, as in Geeqie. The language follows
//! `$LANGUAGE`, `$LC_ALL`, `$LC_MESSAGES` and `$LANG`, unless the `language` setting names one.
//! Command-line output, `--remote` and the Help Manual stay English.

use std::collections::HashMap;
use std::sync::RwLock;

/// `tf!("Could not save {name}: {e}", name, e = err)`: the text in the language in use, its
/// `{name}`s filled in. A name alone is a variable of that name.
#[macro_export]
macro_rules! tf {
    (@value $name:ident = $value:expr) => { $value };
    (@value $name:ident) => { $name };
    ($text:literal $(, $name:ident $(= $value:expr)?)* $(,)?) => {
        $crate::support::lang::fill(
            $crate::support::lang::tr($text),
            &[$((stringify!($name), &$crate::tf!(@value $name $(= $value)?) as &dyn ::std::fmt::Display)),*],
        )
    };
}

/// The translations built in: code (the setting), name in its own language, the language codes
/// of `$LANG` it is for, and its `.po` file. Each has every text of `no_nb.po`.
pub const LANGUAGES: &[(&str, &str, &[&str], &str)] = &[
    ("da", "Dansk", &["da"], include_str!("../../po/da.po")),
    ("de", "Deutsch", &["de"], include_str!("../../po/de.po")),
    ("es", "Español", &["es"], include_str!("../../po/es.po")),
    ("fr", "Français", &["fr"], include_str!("../../po/fr.po")),
    ("is", "Íslenska", &["is"], include_str!("../../po/is.po")),
    ("it", "Italiano", &["it"], include_str!("../../po/it.po")),
    ("nl", "Nederlands", &["nl"], include_str!("../../po/nl.po")),
    (
        "no_nb",
        "Norsk bokmål",
        &["nb", "no"],
        include_str!("../../po/no_nb.po"),
    ),
    ("pl", "Polski", &["pl"], include_str!("../../po/pl.po")),
    ("fi", "Suomi", &["fi"], include_str!("../../po/fi.po")),
    ("sv", "Svenska", &["sv"], include_str!("../../po/sv.po")),
];

/// The translation in use; `None` for English.
static CURRENT: RwLock<Option<&'static Table>> = RwLock::new(None);

/// The most forms a noun has in a translation built in (Polish: one, few, many).
const FORMS: usize = 4;

/// A translation, parsed: each English text and its translation, and each noun of `count`
/// with its forms; `code` picks the form.
#[derive(Default)]
struct Table {
    code: &'static str,
    texts: HashMap<String, String>,
    plurals: HashMap<String, Vec<String>>,
}

/// Which of a noun's forms goes with `n`, by the language's rule (its `.po`'s Plural-Forms):
/// one and many in most; 0 also one in French; 21 one in Icelandic; one, few and many in Polish.
fn plural_form(code: &str, n: usize) -> usize {
    match code {
        "fr" => (n > 1) as usize,
        "is" => (n % 10 != 1 || n % 100 == 11) as usize,
        "pl" if n == 1 => 0,
        "pl" if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => 1,
        "pl" => 2,
        _ => (n != 1) as usize,
    }
}

/// Use the language of `setting` (a code of `LANGUAGES`, "en"), or with "" the system's.
pub fn set(setting: &str) {
    let code = match setting {
        "" => from_env(),
        code => Some(code.to_owned()),
    };
    let table = code.and_then(|code| {
        let k = LANGUAGES.iter().position(|l| l.0 == code)?;
        Some(table(k))
    });
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = table;
}

/// Each translation parsed once, the first time it is chosen, and kept: its texts are handed
/// out as `&'static str`.
fn table(k: usize) -> &'static Table {
    static TABLES: std::sync::Mutex<Vec<Option<&'static Table>>> =
        std::sync::Mutex::new(Vec::new());
    let mut tables = TABLES.lock().unwrap_or_else(|e| e.into_inner());
    tables.resize(LANGUAGES.len(), None);
    tables[k].get_or_insert_with(|| {
        let table = Table {
            code: LANGUAGES[k].0,
            ..parse(LANGUAGES[k].3)
        };
        Box::leak(Box::new(table))
    })
}

/// The translation's code for the first language the environment asks for, as gettext reads it.
/// Returns `None` for English, C or one not built in.
fn from_env() -> Option<String> {
    let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
    // LANGUAGE is a list by preference, and counts only with a locale other than C.
    let locale = var("LC_ALL")
        .or_else(|| var("LC_MESSAGES"))
        .or_else(|| var("LANG"));
    let wanted = match (var("LANGUAGE"), &locale) {
        (Some(list), Some(l)) if !matches!(l.as_str(), "C" | "POSIX") => list,
        _ => locale?,
    };
    wanted.split(':').find_map(language)
}

/// The translation for one locale name, such as "nb_NO.UTF-8", "no_NO" or "nb".
fn language(locale: &str) -> Option<String> {
    let name = locale.split(['.', '@']).next()?;
    let lang = name.split('_').next()?.to_ascii_lowercase();
    if lang == "en" {
        return None;
    }
    LANGUAGES
        .iter()
        .find(|l| l.0 == name || l.2.contains(&lang.as_str()))
        .map(|l| l.0.to_owned())
}

/// `en` in the language in use, or itself when that has no translation of it.
pub fn tr(en: &'static str) -> &'static str {
    let current = CURRENT.read().unwrap_or_else(|e| e.into_inner());
    match *current {
        Some(table) => table.texts.get(en).map_or(en, |t| t.as_str()),
        None => en,
    }
}

/// Returns the group separator and the decimal mark of the language in use, for numbers such
/// as "1.200 Dateien" and "2,3 MB".
pub fn number_marks() -> (&'static str, &'static str) {
    let current = CURRENT.read().unwrap_or_else(|e| e.into_inner());
    marks_of((*current).map_or("en", |t| t.code))
}

/// Returns the group separator and decimal mark of the language `code`. Most languages here
/// group with a no-break space or a full stop and write a decimal comma.
fn marks_of(code: &str) -> (&'static str, &'static str) {
    match code {
        "en" => (",", "."),
        "da" | "de" | "es" | "is" | "it" | "nl" => (".", ","),
        _ => ("\u{a0}", ","),
    }
}

/// The form of `noun` for `n` of it: "image" or "images", or the translation's form.
pub fn plural(noun: &'static str, n: usize) -> std::borrow::Cow<'static, str> {
    let current = CURRENT.read().unwrap_or_else(|e| e.into_inner());
    if let Some(table) = *current
        && let Some(forms) = table.plurals.get(noun)
        && let Some(form) = forms.get(plural_form(table.code, n))
    {
        return form.as_str().into();
    }
    match n {
        1 => noun.into(),
        _ => format!("{noun}s").into(),
    }
}

/// `template` with each `{name}` replaced by its value from `values`; others stay as they are.
pub fn fill(template: &str, values: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = (after.find('}'))
            .and_then(|close| Some((close, values.iter().find(|v| v.0 == &after[..close])?)));
        match value {
            Some((close, (_, v))) => {
                out.push_str(&v.to_string());
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `.po` file's entries: msgid and msgstr, and msgid_plural with msgstr[0], [1] …; the
/// header, untranslated ones and those marked fuzzy left out, as gettext does.
fn parse(po: &str) -> Table {
    let mut table = Table::default();
    let (mut fuzzy, mut field, mut id, mut id_plural) = (false, "", String::new(), None);
    let mut strs: Vec<Option<String>> = vec![None; FORMS];
    let mut entries = Vec::new();
    let mut form = 0;
    let mut end = |fuzzy: &mut bool,
                   id: &mut String,
                   id_plural: &mut Option<String>,
                   strs: &mut Vec<Option<String>>| {
        if !*fuzzy && !id.is_empty() {
            entries.push((std::mem::take(id), id_plural.take(), std::mem::take(strs)));
        }
        id.clear();
        *id_plural = None;
        *strs = vec![None; FORMS];
        *fuzzy = false;
    };
    for line in po.lines().map(str::trim) {
        // An entry ends where the next begins: at its comments, a blank line or its msgid.
        let begins = line.is_empty() || line.starts_with('#') || line.starts_with("msgid ");
        if begins && strs[0].is_some() {
            end(&mut fuzzy, &mut id, &mut id_plural, &mut strs);
        }
        if line.starts_with("#,") && line.contains("fuzzy") {
            fuzzy = true;
        } else if line.is_empty() || line.starts_with('#') {
            continue;
        } else if let Some(text) = line.strip_prefix("msgid_plural ") {
            field = "plural";
            id_plural = Some(unquote(text));
        } else if let Some(text) = line.strip_prefix("msgid ") {
            field = "id";
            id = unquote(text);
        } else if let Some(text) = line.strip_prefix("msgstr ") {
            field = "str";
            form = 0;
            strs[0] = Some(unquote(text));
        } else if let Some((k, text)) = line
            .strip_prefix("msgstr[")
            .and_then(|rest| rest.split_once("] "))
            .and_then(|(k, text)| Some((k.parse::<usize>().ok().filter(|&k| k < FORMS)?, text)))
        {
            field = "str";
            form = k;
            strs[k] = Some(unquote(text));
        } else if line.starts_with('"') {
            let more = unquote(line);
            match field {
                "id" => id.push_str(&more),
                "plural" => id_plural.get_or_insert_default().push_str(&more),
                "str" => strs[form].get_or_insert_default().push_str(&more),
                _ => {}
            }
        }
    }
    end(&mut fuzzy, &mut id, &mut id_plural, &mut strs);
    for (id, id_plural, strs) in entries {
        // The forms given, up to the first missing one: at least two, each with its text.
        let forms: Vec<String> = strs
            .into_iter()
            .map_while(|s| s.filter(|s| !s.is_empty()))
            .collect();
        match id_plural {
            Some(_) if forms.len() >= 2 => {
                table.plurals.insert(id, forms);
            }
            None if !forms.is_empty() => {
                table
                    .texts
                    .insert(id, forms.into_iter().next().unwrap_or_default());
            }
            _ => {}
        }
    }
    table
}

/// The text of a quoted `.po` string, its escapes read: \" \\ \n \t.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// Every language has its own marks for numbers, and English keeps its comma and point.
    #[test]
    fn numbers_take_the_language_marks() {
        assert_eq!(super::marks_of("en"), (",", "."));
        assert_eq!(super::marks_of("de"), (".", ","));
        assert_eq!(super::marks_of("no_nb"), ("\u{a0}", ","));
        for l in super::LANGUAGES {
            assert_ne!(super::marks_of(l.0), (",", "."), "{}", l.0);
        }
    }

    /// Entries, plurals, continued strings and escapes read; the header, fuzzy and empty ones
    /// left out.
    #[test]
    fn reads_po_files() {
        let po = r#"# A comment
msgid ""
msgstr ""
"Language: nb\n"

msgid "Open File…"
msgstr "Åpne fil…"

#, fuzzy
msgid "Quit"
msgstr "Avslutt"

msgid "Not translated"
msgstr ""

msgid "Could not save {name}: "
"{e}"
msgstr "Kunne ikke lagre {name}: "
"{e}"

msgid "Say \"hi\"\n"
msgstr "Si \"hei\"\n"

msgid "image"
msgid_plural "images"
msgstr[0] "bilde"
msgstr[1] "bilder"
"#;
        let t = super::parse(po);
        assert_eq!(t.texts.get("Open File…").unwrap(), "Åpne fil…");
        assert!(!t.texts.contains_key("Quit") && !t.texts.contains_key("Not translated"));
        assert!(!t.texts.contains_key(""));
        assert_eq!(
            t.texts.get("Could not save {name}: {e}").unwrap(),
            "Kunne ikke lagre {name}: {e}"
        );
        assert_eq!(t.texts.get("Say \"hi\"\n").unwrap(), "Si \"hei\"\n");
        assert_eq!(t.plurals.get("image").unwrap(), &["bilde", "bilder"]);
        // Polish: one, few, many.
        let t = super::parse(
            "msgid \"file\"\nmsgid_plural \"files\"\nmsgstr[0] \"plik\"\nmsgstr[1] \"pliki\"\nmsgstr[2] \"plików\"\n",
        );
        assert_eq!(t.plurals.get("file").unwrap(), &["plik", "pliki", "plików"]);
    }

    /// `{name}`s filled in, others and a lone brace left as they are.
    #[test]
    fn fills_in_values() {
        let (n, e) = (3, "full disk");
        assert_eq!(
            super::fill("{n} left: {e}", &[("n", &n), ("e", &e)]),
            "3 left: full disk"
        );
        assert_eq!(super::fill("{x} {n} {", &[("n", &n)]), "{x} 3 {");
        let name = "a.jpg";
        assert_eq!(
            crate::tf!("Could not save {name}: {e}", name, e = e),
            "Could not save a.jpg: full disk"
        );
    }

    /// Locale names to the translation built in; English, C and others to none.
    #[test]
    fn finds_the_language() {
        for l in ["nb_NO.UTF-8", "nb", "no_NO", "nb_NO@euro", "no_nb"] {
            assert_eq!(super::language(l).as_deref(), Some("no_nb"), "{l}");
        }
        assert_eq!(super::language("de_DE.UTF-8").as_deref(), Some("de"));
        assert_eq!(super::language("pl_PL.UTF-8").as_deref(), Some("pl"));
        for l in ["en_US.UTF-8", "C", "ru_RU.UTF-8", ""] {
            assert_eq!(super::language(l), None, "{l}");
        }
    }

    /// The translation built in: every placeholder of a text kept in its translation, so a
    /// value is never lost or shown as "{name}".
    #[test]
    fn translations_keep_placeholders() {
        let names = |s: &str| {
            let mut v: Vec<String> = s
                .split('{')
                .skip(1)
                .filter_map(|p| Some(p[..p.find('}')?].to_owned()))
                .collect();
            v.sort();
            v
        };
        for (code, _, _, po) in super::LANGUAGES {
            let t = super::parse(po);
            for (en, text) in &t.texts {
                assert_eq!(names(en), names(text), "{code}: {en}");
            }
            println!("{code}: {} texts, {} nouns", t.texts.len(), t.plurals.len());
        }
    }

    /// Each language's form for a number: Polish's few and many, Icelandic's 21, French's 0.
    #[test]
    fn picks_the_plural_form() {
        let forms = |code, ns: &[usize]| {
            ns.iter()
                .map(|&n| super::plural_form(code, n))
                .collect::<Vec<_>>()
        };
        assert_eq!(forms("de", &[0, 1, 2, 21]), [1, 0, 1, 1]);
        assert_eq!(forms("fr", &[0, 1, 2]), [0, 0, 1]);
        assert_eq!(forms("is", &[1, 2, 11, 21, 111]), [0, 1, 1, 0, 1]);
        assert_eq!(
            forms("pl", &[1, 2, 4, 5, 12, 22, 25, 112]),
            [0, 1, 1, 2, 2, 1, 2, 2]
        );
    }

    /// The template for a new language, po/glimt.pot, has every text of no_nb.po, in its order,
    /// and no translation. After a text is added or changed, `python3 tools/pot.py` writes it.
    #[test]
    fn the_template_has_every_text() {
        let ids = |po: &'static str| -> Vec<&'static str> {
            (po.lines()).filter(|l| l.starts_with("msgid")).collect()
        };
        let pot = include_str!("../../po/glimt.pot");
        assert!(
            ids(pot) == ids(include_str!("../../po/no_nb.po")),
            "po/glimt.pot is behind po/no_nb.po: run python3 tools/pot.py"
        );
        let translated = (pot.lines().skip_while(|l| !l.is_empty()))
            .find(|l| l.starts_with("msgstr") && !l.ends_with(" \"\""));
        assert_eq!(translated, None, "po/glimt.pot has a translation");
    }

    /// Every translation has every text and noun of the Norwegian one, which each new text
    /// gets first: a language that lacks one would show it in English.
    #[test]
    fn every_language_has_every_text() {
        let nb = super::parse(include_str!("../../po/no_nb.po"));
        for (code, _, _, po) in super::LANGUAGES {
            let t = super::parse(po);
            let lacks: Vec<&String> = (nb.texts.keys())
                .filter(|k| !t.texts.contains_key(*k))
                .chain(nb.plurals.keys().filter(|k| !t.plurals.contains_key(*k)))
                .collect();
            assert!(lacks.is_empty(), "{code} lacks {lacks:?}");
            // As many forms as its rule picks from.
            let most = (0..200)
                .map(|n| super::plural_form(code, n))
                .max()
                .unwrap_or(0);
            for (noun, forms) in &t.plurals {
                assert!(
                    forms.len() > most,
                    "{code}: {noun} has {} forms",
                    forms.len()
                );
            }
        }
    }

    /// Every text the window shows goes through tr, tf! or plural: a literal handed straight to
    /// a widget, or one that starts with a capital and has two words, is translated where it
    /// stands, or is a text of no_nb.po in a table (a const or static) translated where it is
    /// shown, or stays English for a reason below. Each literal in tr, tf! or plural is a text
    /// of no_nb.po, so the translation test above finds it in every language.
    #[test]
    fn every_text_shown_is_translated() {
        // (file, the literals or the item they are in ("" for the whole file), why English).
        const ENGLISH: &[(&str, &[&str], &str)] = &[
            ("ui/help.rs", &[""], "the Help Manual stays English"),
            (
                "features/remote.rs",
                &[""],
                "--remote's commands and answers stay English",
            ),
            (
                "support/source_rules.rs",
                &[""],
                "tests of the source; their texts are for developers",
            ),
            (
                "cli.rs",
                &["pub(crate) const START_OPTIONS"],
                "--help stays English",
            ),
            (
                "ui/actions.rs",
                &["pub fn english("],
                "the actions' names, which label translates",
            ),
            (
                "images/colour.rs",
                &["Adobe RGB", "Display P3"],
                "names of colour profiles",
            ),
            (
                "support/lang.rs",
                &["Norsk bokmål"],
                "each language is named in itself",
            ),
            (
                "images/svg.rs",
                &["SVG "],
                "what is wrong in a file, as the system's errors",
            ),
            (
                "metadata/exif.rs",
                &["EXIF block too large"],
                "what is wrong in a file",
            ),
            (
                "metadata/exif.rs",
                &["Focal length", "Exposure bias"],
                "keys, translated where shown",
            ),
            (
                "ui/info_panel.rs",
                &["File size", "Colour profile"],
                "keys, translated where shown",
            ),
            (
                "features/print.rs",
                &["File size"],
                "the caption template's field, as in the panel",
            ),
            (
                "viewer/pan.rs",
                &["No date"],
                "a key, translated where shown",
            ),
            (
                "files/external.rs",
                &["Registered applications"],
                "read in gio's output",
            ),
            (
                "images/thumbs.rs",
                &[
                    "No such file",
                    "Permission denied",
                    "Input/output error",
                    "Transport endpoint",
                    "Host is down",
                ],
                "read in programs' errors",
            ),
            (
                "metadata/lightroom.rs",
                &["SELECT ", "FROM "],
                "SQL for sqlite3",
            ),
            ("features/print.rs", &["BT /F "], "PDF operators"),
            (
                "viewer/view.rs",
                &["Glimt Projector"],
                "the window title the release tests look for",
            ),
            (
                "ui/window.rs",
                &["Glimt full screen"],
                "the reason given to the screen saver",
            ),
        ];
        let shown_by = [
            ".label(",
            ".button(",
            ".small_button(",
            ".checkbox(",
            ".radio(",
            ".radio_value(",
            ".selectable_label(",
            ".selectable_value(",
            ".menu_button(",
            ".heading(",
            ".weak(",
            ".strong(",
            ".small(",
            "link(",
            "submenu(",
            ".on_hover_text(",
            ".on_disabled_hover_text(",
            ".hint_text(",
            ".selected_text(",
            ".text(",
            "Window::new(",
            "Button::new(",
            "button(",
            "selectable(",
            "choice(",
            "checkbox(",
            "radio(",
            "radio_choice(",
            "RichText::new(",
            "ProgressBar::new(",
        ];
        // Words, not a {value} or a %field% of a template.
        let words = |t: &str| {
            (t.split_whitespace())
                .filter(|w| w.chars().any(char::is_alphabetic) && !w.contains(['{', '%']))
                .count()
        };
        let called = |before: &str, calls: &[&str]| {
            calls.iter().any(|c| {
                // A method on anything, a function not as the end of a longer name.
                before.ends_with(c)
                    && (c.starts_with('.')
                        || !(before[..before.len() - c.len()])
                            .ends_with(|ch: char| ch.is_alphanumeric() || ch == '_'))
            })
        };
        /// Finds the call that the literal at `at` is an argument of, wherever it stands among
        /// the arguments. Quotes are skipped over, so parentheses in an earlier argument's text
        /// do not count. Returns the code up to the call's opening parenthesis, or "" when the
        /// literal is in no call of its statement.
        fn enclosing(code: &str, at: usize) -> &str {
            let (mut depth, mut quoted) = (0, false);
            let b = code.as_bytes();
            for i in (0..at).rev() {
                match b[i] {
                    b'"' if i == 0 || b[i - 1] != b'\\' => quoted = !quoted,
                    _ if quoted => {}
                    b')' | b']' => depth += 1,
                    b'(' | b'[' if depth > 0 => depth -= 1,
                    b'(' => return &code[..=i],
                    b';' | b'{' | b'}' if depth == 0 => return "",
                    _ => {}
                }
            }
            ""
        }
        // A one-word label after other arguments, one of them with a parenthesis in its text.
        let sample = r#"x.y(); choice(ui, &mut c, f("("), "Calendar")"#;
        let at = sample.find("\"Calendar").unwrap();
        assert!(called(enclosing(sample, at), &shown_by));
        // An item's lines: from its first line to the one that closes it, the next one no
        // deeper, or that line alone when it ends in ;.
        let item = |code: &str, start: usize| {
            let first = code[start..].find('\n').map_or(code.len(), |n| start + n);
            let depth = start - code[..start].rfind('\n').map_or(0, |n| n + 1);
            if code[start..first].ends_with(';') {
                return start..first;
            }
            let mut at = first + 1;
            while at < code.len() {
                let end = code[at..].find('\n').map_or(code.len(), |n| at + n);
                let line = &code[at..end];
                if !line.trim().is_empty() && line.len() - line.trim_start().len() <= depth {
                    return start..end;
                }
                at = end + 1;
            }
            start..code.len()
        };
        let nb = super::parse(include_str!("../../po/no_nb.po"));
        let msgid = |t: &str| nb.texts.contains_key(t) || nb.plurals.contains_key(t);
        let mut wrong = Vec::new();
        for (file, text) in crate::support::source_rules::rust_sources() {
            let code = text.split("\n#[cfg(test)]\nmod tests").next().unwrap();
            let mut tables = Vec::new();
            for (at, _) in code.match_indices('\n') {
                let line = code[at + 1..]
                    .trim_start_matches("pub(crate) ")
                    .trim_start_matches("pub ");
                if line.starts_with("const ") || line.starts_with("static ") {
                    tables.push(item(code, at + 1));
                }
            }
            let mut english = Vec::new();
            for (_, marks, _) in ENGLISH.iter().filter(|e| e.0 == file) {
                for mark in *marks {
                    let is_item = mark.starts_with("const ") || mark.starts_with("pub");
                    match code.find(mark).filter(|_| is_item) {
                        Some(start) => english.push(Err(item(code, start))),
                        None => english.push(Ok(*mark)),
                    }
                }
            }
            for (at, text) in crate::support::source_rules::literals(code) {
                let before = code[..at].trim_end();
                let line = code[..at].matches('\n').count() + 1;
                if called(before, &["tr(", "tf!(", "plural("]) {
                    if !msgid(&text) {
                        wrong.push(format!("{file}:{line}: not in no_nb.po: {text:?}"));
                    }
                    continue;
                }
                let shown = (text.starts_with(char::is_uppercase) && words(&text) >= 2)
                    || (called(enclosing(code, at), &shown_by) && words(&text) >= 1);
                let table = tables.iter().any(|r| r.contains(&at)) && msgid(&text);
                let terminal = called(
                    before,
                    &["println!(", "eprintln!(", "print!(", "eprint!(", "panic!("],
                );
                let kept = english.iter().any(|e| match e {
                    Ok(start) => text.starts_with(start),
                    Err(range) => range.contains(&at),
                });
                if shown && !table && !terminal && !kept {
                    wrong.push(format!("{file}:{line}: not translated: {text:?}"));
                }
            }
        }
        wrong.sort();
        assert!(
            wrong.is_empty(),
            "put these in tr(\"…\") or tf!(\"…\"):\n{}",
            wrong.join("\n")
        );
    }

    /// `.po` files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_po_files() {
        let seed = "msgid \"a\\\"b\"\nmsgstr \"c\\n\"\n\"d\"\nmsgid \"x\"\nmsgid_plural \"xs\"\nmsgstr[0] \"y\"\nmsgstr[1] \"ys\"\n";
        for bytes in crate::support::test_support::broken(seed.as_bytes()) {
            super::parse(&String::from_utf8_lossy(&bytes));
        }
    }
}
