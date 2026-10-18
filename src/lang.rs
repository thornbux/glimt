//! The interface in the user's language. The English texts are the keys: `tr("Open File…")`,
//! `tf!("Could not save {name}: {e}", name, e)` for texts with values, and `plural` for
//! `count`'s nouns. A translation is a gettext `.po` file built in (`src/no_nb.po`); a text it
//! lacks shows in English, mixed with the translated ones, as in Geeqie. The language follows
//! `$LANGUAGE`, `$LC_ALL`, `$LC_MESSAGES` and `$LANG`, unless the `language` setting names one.
//! Command-line output, `--remote` and the Help Manual stay English.

use std::collections::HashMap;
use std::sync::RwLock;

/// The translations built in: code (the setting), name in its own language, the language codes
/// of `$LANG` it is for, and its `.po` file.
pub const LANGUAGES: &[(&str, &str, &[&str], &str)] = &[(
    "no_nb",
    "Norsk bokmål",
    &["nb", "no"],
    include_str!("no_nb.po"),
)];

/// A translation, parsed: each English text and its translation, and each noun of `count`
/// with its two forms.
#[derive(Default)]
struct Table {
    texts: HashMap<String, String>,
    plurals: HashMap<String, [String; 2]>,
}

/// The translation in use; `None` for English.
static CURRENT: RwLock<Option<&'static Table>> = RwLock::new(None);

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
    tables[k].get_or_insert_with(|| Box::leak(Box::new(parse(LANGUAGES[k].3))))
}

/// The translation's code for the first language the environment asks for, as gettext reads
/// it; `None` for English, C or one not built in.
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

/// The form of `noun` for `n` of it: "image" or "images", or the translation's two forms.
// shortcut: two forms, the one and the many, as in English and Norwegian; a language with
// more (Polish, Russian) gets its rule from its .po's Plural-Forms when one is added.
pub fn plural(noun: &'static str, n: usize) -> std::borrow::Cow<'static, str> {
    let current = CURRENT.read().unwrap_or_else(|e| e.into_inner());
    if let Some(forms) = current.and_then(|t| t.plurals.get(noun)) {
        return forms[(n != 1) as usize].as_str().into();
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

/// `tf!("Could not save {name}: {e}", name, e = err)`: the text in the language in use, its
/// `{name}`s filled in. A name alone is a variable of that name.
#[macro_export]
macro_rules! tf {
    (@value $name:ident = $value:expr) => { $value };
    (@value $name:ident) => { $name };
    ($text:literal $(, $name:ident $(= $value:expr)?)* $(,)?) => {
        $crate::lang::fill(
            $crate::lang::tr($text),
            &[$((stringify!($name), &$crate::tf!(@value $name $(= $value)?) as &dyn ::std::fmt::Display)),*],
        )
    };
}

/// The `.po` file's entries: msgid and msgstr, and msgid_plural with msgstr[0] and [1]; the
/// header, untranslated ones and those marked fuzzy left out, as gettext does.
fn parse(po: &str) -> Table {
    let mut table = Table::default();
    let (mut fuzzy, mut field, mut id, mut id_plural) = (false, "", String::new(), None);
    let mut strs: [Option<String>; 2] = [None, None];
    let mut entries = Vec::new();
    let mut end = |fuzzy: &mut bool,
                   id: &mut String,
                   id_plural: &mut Option<String>,
                   strs: &mut [Option<String>; 2]| {
        if !*fuzzy && !id.is_empty() {
            entries.push((std::mem::take(id), id_plural.take(), std::mem::take(strs)));
        }
        id.clear();
        *id_plural = None;
        *strs = [None, None];
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
        } else if let Some(text) = line
            .strip_prefix("msgstr[0] ")
            .or(line.strip_prefix("msgstr "))
        {
            field = "str0";
            strs[0] = Some(unquote(text));
        } else if let Some(text) = line.strip_prefix("msgstr[1] ") {
            field = "str1";
            strs[1] = Some(unquote(text));
        } else if line.starts_with('"') {
            let more = unquote(line);
            match field {
                "id" => id.push_str(&more),
                "plural" => id_plural.get_or_insert_default().push_str(&more),
                "str0" => strs[0].get_or_insert_default().push_str(&more),
                "str1" => strs[1].get_or_insert_default().push_str(&more),
                _ => {}
            }
        }
    }
    end(&mut fuzzy, &mut id, &mut id_plural, &mut strs);
    for (id, id_plural, [one, many]) in entries {
        match (id_plural, one, many) {
            (Some(_), Some(one), Some(many)) if !one.is_empty() && !many.is_empty() => {
                table.plurals.insert(id, [one, many]);
            }
            (None, Some(text), _) if !text.is_empty() => {
                table.texts.insert(id, text);
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
        assert_eq!(
            t.plurals.get("image").unwrap(),
            &["bilde".to_owned(), "bilder".to_owned()]
        );
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
        for l in ["en_US.UTF-8", "C", "de_DE.UTF-8", ""] {
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

    /// `.po` files broken in thousands of ways: read without a panic.
    #[test]
    fn survives_broken_po_files() {
        let seed = "msgid \"a\\\"b\"\nmsgstr \"c\\n\"\n\"d\"\nmsgid \"x\"\nmsgid_plural \"xs\"\nmsgstr[0] \"y\"\nmsgstr[1] \"ys\"\n";
        for bytes in crate::broken(seed.as_bytes()) {
            super::parse(&String::from_utf8_lossy(&bytes));
        }
    }
}
