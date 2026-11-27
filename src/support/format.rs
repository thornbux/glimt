//! Text for the user: sizes, counts, times, a text cut to fit, templates filled in, and names
//! made from a file's bytes.

use crate::support::lang::{self, tr};

/// Makes each number type a `Number` and a `Shown`, shown as `ShownText::number` writes it.
macro_rules! numbers {
    ($($t:ty),*) => { $(
        impl Number for $t {}
        impl Shown for $t {
            fn shown(&self) -> ShownText {
                ShownText::number(*self)
            }
        }
    )* };
}

/// Makes each error type a `Shown`, shown as `ShownText::of_error` escapes it.
macro_rules! errors {
    ($($t:ty),*) => { $(
        impl Shown for $t {
            fn shown(&self) -> ShownText {
                ShownText::of_error(self)
            }
        }
    )* };
}

/// The size that a sum of the files of a share gives (`listing::is_on_a_share`). Their sizes
/// are not summed there, because a look at each file is a round trip over the network, and a
/// tree of many files kept the share busy for a program the user started on it.
pub(crate) const NOT_COUNTED_ON_A_SHARE: u64 = u64::MAX;

/// A piece of a text with `%` codes, as `pieces` reads it.
#[derive(Debug, PartialEq)]
pub(crate) enum Piece<'a> {
    /// Text as it stands. A `%%` is the text "%".
    Text(&'a str),
    /// A code without its `%` signs, such as `name` of `%name%` or `f` of `%f`.
    Code(&'a str),
}

/// A text that reaches the user: a label, a button, a notice, a failure, a question, the window
/// title, a screen reader's words or a print's caption. Only these make one, so a text of
/// another kind, such as a path's `to_string_lossy` or a raw `String`, does not compile where
/// one is shown:
/// - `lang::tr` and `tf!`, a translated text (`tf!` takes its values as `Shown`);
/// - `escaped_name` and `escaped_file_name`, and through them `App::name` and
///   `file_io::shown_path`, a name or path;
/// - `thousands`, `count`, `human_size`, `size_text` and `ShownText::number`, a number;
/// - `ShownText::from_file_text_on_one_line` and `from_file_text`, a text read from a file;
/// - `ShownText::untranslated`, a text that has no translation, such as "…" or a key's name,
///   and `untranslated!`, an English cause of a failure with its values filled in;
/// - `ShownText::of_error` and `of_message`, a library's error or a program's message, the
///   cause of a failure;
/// - `ShownText::of`, a value as `tf!` fills a placeholder with it, and `keys_of`, keys;
/// - `join` and `push`, of texts that are shown texts already.
///
/// Before, every widget and notice took any `String`, and `source_rules` read the source to find
/// a name that reached the user unescaped, which a spelling it did not know slipped past (#320,
/// #323).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ShownText(std::borrow::Cow<'static, str>);

impl ShownText {
    /// Returns `text`, which has no translation: a symbol such as "…", a key's name, or a text
    /// that stays English, such as the About window's credits.
    pub(crate) const fn untranslated(text: &'static str) -> Self {
        ShownText(std::borrow::Cow::Borrowed(text))
    }

    /// Returns `text`, made by one of the ways in that the type's doc lists. Only this module
    /// and `lang` make a text from a `String`.
    pub(in crate::support) fn made(text: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        ShownText(text.into())
    }

    /// Returns the number `n` as a text, as it is written in code ("12", "2.5"). A count of
    /// things goes through `count` or `thousands` instead, which group the digits.
    pub(crate) fn number(n: impl Number) -> Self {
        ShownText(n.to_string().into())
    }

    /// Returns `text`, read from a file (an EXIF or XMP value, a template's field), for one
    /// line: the window title, the full screen info, a print's caption, a notice or a question.
    /// Its line breaks, other control characters and marks of direction are written as escapes,
    /// so it cannot add a line of its own. Its backslashes are kept, as in a comment
    /// "D:\Archive": only a name doubles them (`escaped_name`), so that the text "\n" in a name
    /// is not taken for a line break of the name. A file's or folder's name goes through
    /// `escaped_name` instead.
    pub(crate) fn from_file_text_on_one_line(text: &str) -> Self {
        ShownText::with_hidden_marks_escaped(text)
    }

    /// Returns `text`, read from a file, for a place that shows it whole, as the info panel
    /// shows a comment or a description: its own line breaks are kept.
    pub(crate) fn from_file_text(text: &str) -> Self {
        ShownText(text.to_owned().into())
    }

    /// Returns the keys of `shortcut` as the menus show them, such as "Ctrl+Shift+N".
    pub(crate) fn keys_of(
        ctx: &eframe::egui::Context,
        shortcut: &eframe::egui::KeyboardShortcut,
    ) -> Self {
        ShownText(ctx.format_shortcut(shortcut).into())
    }

    /// Returns `value` alone as a text, as `tf!` would fill a placeholder with it, such as an
    /// error's text that is drawn as a failure's cause.
    pub(crate) fn of(value: &impl Shown) -> Self {
        value.shown()
    }

    /// Returns the text of `error`, made by a library or the system (a decoder's, a codec's, a
    /// bus's or a regular expression's error), as the cause of a failure. Its line breaks,
    /// other control characters and marks of direction are written as escapes, as a library
    /// may put a name it was given into its text. Its backslashes are kept, so a text that was
    /// escaped once, such as a `ShownText` passed on inside an `std::io::Error`, is not
    /// escaped again.
    pub(crate) fn of_error(error: &impl std::error::Error) -> Self {
        ShownText::with_hidden_marks_escaped(&error.to_string())
    }

    /// Returns `text`, a message that a program or a library wrote (ffmpeg's last error line,
    /// the line of a regular expression's error that says what is wrong), as the cause of a
    /// failure. It is escaped as `of_error` escapes a library's error, as such a message may
    /// hold the names or the pattern it was given.
    pub(crate) fn of_message(text: &str) -> Self {
        ShownText::with_hidden_marks_escaped(text)
    }

    /// Returns `text` with its line breaks, other control characters and marks of direction
    /// written as escapes, and its backslashes kept.
    fn with_hidden_marks_escaped(text: &str) -> Self {
        let mut shown = String::new();
        for c in text.chars() {
            match c {
                c if is_hidden_mark(c) => shown.extend(c.escape_default()),
                c => shown.push(c),
            }
        }
        ShownText(shown.into())
    }

    /// Returns `parts` joined by `separator`.
    pub(crate) fn join<'a>(
        parts: impl IntoIterator<Item = &'a ShownText>,
        separator: &ShownText,
    ) -> Self {
        let parts: Vec<&str> = parts.into_iter().map(ShownText::as_str).collect();
        ShownText(parts.join(separator.as_str()).into())
    }

    /// Returns `parts` one after the other, such as a number and a symbol.
    pub(crate) fn concat<'a>(parts: impl IntoIterator<Item = &'a ShownText>) -> Self {
        ShownText::join(parts, &ShownText::default())
    }

    /// Adds `more` at the end of the text.
    pub(crate) fn push(&mut self, more: &ShownText) {
        self.0.to_mut().push_str(more.as_str());
    }

    /// Returns the text without the white space at its start and end.
    pub(crate) fn trimmed(&self) -> Self {
        ShownText(self.0.trim().to_owned().into())
    }

    /// Returns the text with each line break as `separator`, such as the lines of a template
    /// on a caption's one line.
    pub(crate) fn with_line_breaks_as(&self, separator: &ShownText) -> Self {
        ShownText(self.0.replace('\n', separator.as_str()).into())
    }

    /// Returns the text cut to `chars` characters, with "..." at the end when it is longer.
    pub(crate) fn cut_to(&self, chars: usize) -> Self {
        match self.0.chars().count() > chars {
            true => {
                let kept: String = self.0.chars().take(chars.saturating_sub(3)).collect();
                ShownText((kept + "...").into())
            }
            false => self.clone(),
        }
    }

    /// Returns the text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for ShownText {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::ops::AddAssign<&ShownText> for ShownText {
    fn add_assign(&mut self, more: &ShownText) {
        self.push(more);
    }
}

impl std::ops::AddAssign<ShownText> for ShownText {
    fn add_assign(&mut self, more: ShownText) {
        self.push(&more);
    }
}

impl std::fmt::Display for ShownText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A shown text is an error's text too, as when a step words why it failed.
impl std::error::Error for ShownText {}

impl AsRef<ShownText> for ShownText {
    fn as_ref(&self) -> &ShownText {
        self
    }
}

/// A test gives a text as a literal, which the code itself makes only through `tr`.
#[cfg(test)]
impl From<&'static str> for ShownText {
    fn from(text: &'static str) -> Self {
        ShownText::untranslated(text)
    }
}

impl PartialEq<str> for ShownText {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<String> for ShownText {
    fn eq(&self, other: &String) -> bool {
        self.0 == other.as_str()
    }
}

impl PartialEq<ShownText> for &str {
    fn eq(&self, other: &ShownText) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<&str> for ShownText {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// A value that `tf!` may fill a placeholder with: a shown text, a number, or an error's text.
/// Each says how it is shown, so a value fills a placeholder only in the form it has as a text
/// of its own: an error escaped by `ShownText::of_error`. Before, an error filled a placeholder
/// with its raw text, while `of_error` escaped the same error (audit 0.102.0 r1, A-1).
pub(crate) trait Shown {
    /// Returns the value as a text for the user.
    fn shown(&self) -> ShownText;
}

impl Shown for ShownText {
    fn shown(&self) -> ShownText {
        self.clone()
    }
}

impl<T: Shown + ?Sized> Shown for &T {
    fn shown(&self) -> ShownText {
        (**self).shown()
    }
}

errors!(
    std::io::Error,
    crate::files::file_io::StepError,
    image::ImageError,
    arboard::Error
);

/// A number that `ShownText::number` writes, and that fills a placeholder of `tf!`.
pub(crate) trait Number: std::fmt::Display {}

numbers!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64
);

/// A size for the status bar, or "calculating…" while it is summed (`None`), or that it is not
/// counted on a share (`NOT_COUNTED_ON_A_SHARE`).
pub(crate) fn size_text(bytes: Option<u64>) -> ShownText {
    match bytes {
        None => tr("calculating…"),
        Some(NOT_COUNTED_ON_A_SHARE) => tr("not counted on a network folder"),
        Some(bytes) => human_size(bytes),
    }
}

/// Size as in the file manager: "850.0 kB", "2.3 MB", "1.2 GB" (decimal units), with the
/// decimal mark of the language in use.
pub(crate) fn human_size(bytes: u64) -> ShownText {
    // A unit starts where the one below would round to 1000.0, so 999,950 bytes are "1.0 MB".
    let (value, unit) = match bytes {
        b if b >= 999_950_000 => (b as f64 / 1e9, "GB"),
        b if b >= 999_950 => (b as f64 / 1e6, "MB"),
        b if b >= 1_000 => (b as f64 / 1e3, "kB"),
        b => return ShownText::made(format!("{b} B")),
    };
    ShownText::made(format!("{value:.1} {unit}").replacen('.', lang::number_separators().1, 1))
}

/// Integer with its thousands grouped as the language in use writes them: 300000 → "300,000"
/// in English, "300.000" in German.
pub(crate) fn thousands(n: usize) -> ShownText {
    ShownText::made(grouped(n, lang::number_separators().0))
}

/// Returns `n` with `separator` between each group of three digits.
fn grouped(n: usize, separator: &str) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(separator);
        }
        out.push(c);
    }
    out
}

/// `n` and `noun` in the singular or plural, with thousands separated: "1 image", "1,200 images".
pub(crate) fn count(n: usize, noun: &'static str) -> ShownText {
    ShownText::made(format!("{} {}", thousands(n), lang::plural(noun, n)))
}

/// `text` as it is if it `fits`, else its start and end around "…", as long as fits; found by
/// halving, so a row lays out a few texts, not one for each letter.
pub(crate) fn middle_cut(text: &ShownText, fits: impl Fn(&str) -> bool) -> ShownText {
    if fits(text) {
        return text.clone();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| -> String {
        let tail = keep / 2;
        let head: String = chars[..keep - tail].iter().collect();
        let end: String = chars[chars.len() - tail..].iter().collect();
        format!("{head}…{end}")
    };
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&cut(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    ShownText::made(cut(lo))
}

/// Returns the pieces of `text` from left to right: the text, and the codes in it. With
/// `named_codes`, as in the templates, a code is `%name%` for a name that `named_codes` takes.
/// A `%` before another name is text, and the `%` after that name starts the next code, so
/// "100% of %pages%" keeps its page count. Without it, as in a desktop file's Exec, a code is a
/// `%` with the one character after it. A `%%` is a `%` of its own in both, never the start
/// or the end of a code, so `%page%%pages%` is two codes and `%%f` is the text "%f". A `%`
/// that starts no code is text. Every template and Exec word is read here, so the rule for
/// `%%` is held in one place.
pub(crate) fn pieces(
    text: &str,
    named_codes: Option<fn(&str) -> bool>,
) -> impl Iterator<Item = Piece<'_>> {
    let mut rest = text;
    let closed = named_codes.is_some();
    std::iter::from_fn(move || {
        let at = rest.find('%');
        if at != Some(0) {
            let end = at.unwrap_or(rest.len());
            let (piece, after) = rest.split_at(end);
            rest = after;
            return (!piece.is_empty()).then_some(Piece::Text(piece));
        }
        let after = &rest[1..];
        let length = match closed {
            true => after.find('%'),
            false => after.chars().next().map(char::len_utf8),
        };
        let Some(length) = length else {
            return Some(Piece::Text(std::mem::take(&mut rest)));
        };
        let code = &after[..length];
        if named_codes.is_some_and(|is_code| !code.is_empty() && !is_code(code)) {
            let (piece, after) = rest.split_at(1 + length);
            rest = after;
            return Some(Piece::Text(piece));
        }
        rest = &after[length + closed as usize..];
        Some(match code {
            "" | "%" => Piece::Text("%"),
            code => Piece::Code(code),
        })
    })
}

/// `template` (see FULL_SCREEN_INFO_TEMPLATE) with each `%field%` replaced by `value(field)`.
/// A `%%` is a `%` of its own, not a field, so it never takes its part along.
pub(crate) fn fill_template(template: &str, value: impl Fn(&str) -> ShownText) -> ShownText {
    let lines = template.split("\\n").filter_map(|line| {
        let parts: Vec<String> = (line.split('|'))
            .filter_map(|part| {
                let (mut fields, mut filled) = (0, 0);
                // Any name is a field, as an EXIF label is one.
                let out: String = (pieces(part, Some(|_| true)))
                    .map(|piece| match piece {
                        Piece::Text(text) => text.to_owned(),
                        Piece::Code(field) => {
                            let v = value(field);
                            fields += 1;
                            filled += !v.is_empty() as usize;
                            v.to_string()
                        }
                    })
                    .collect();
                (fields == 0 || filled > 0).then_some(out)
            })
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    });
    ShownText::made(lines.collect::<Vec<_>>().join("\n"))
}

/// Seconds since 1970 as local "2026-10-11 00:34:12" (`local_time`), as a text for the user.
pub(crate) fn shown_time(secs: i64) -> ShownText {
    ShownText(local_time(secs).into())
}

/// Seconds since 1970 as local "2026-10-11 00:34:12". Undo compares this form, with a T for
/// the space, with the trash's DeletionDate: keep it so.
pub(crate) fn local_time(secs: i64) -> String {
    // A time that the system's time_t cannot hold (after 2038 with a 32-bit one) has no date.
    let Some(time) = libc::time_t::try_from(secs).ok() else {
        return String::new();
    };
    #[allow(unsafe_code)]
    // SAFETY: libc's tm is the system's struct tm, all of whose fields may be zero, and both
    // pointers are valid for the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return String::new();
        }
        tm
    };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Seconds since 1970 as UTC in RFC 3339, such as "2026-11-10T08:05:09Z", for scripts to read.
pub(crate) fn utc_time(secs: i64) -> String {
    let (days, second_of_day) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // The calendar date of a day count, by Howard Hinnant's civil_from_days. Its years start
    // in March, so that the leap day falls at their end.
    let from_year_0 = days + 719_468;
    let (era, day_of_era) = (
        from_year_0.div_euclid(146_097),
        from_year_0.rem_euclid(146_097),
    );
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day / 60 % 60,
        second_of_day % 60
    )
}

/// Returns `text` with each control character, such as a line break, turned into "?". A name
/// made from a file's bytes goes through it, so it shows on one line and cannot add a line, and
/// with it a word, to a file that holds one name to a line.
pub(crate) fn without_control_characters(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Returns `name`, a file's or folder's name or path, as a text for the user: a label, a
/// question, a notice or a line printed. Each control character, such as a line break, and each
/// character that turns the direction of the text around it, such as U+202E, is written as an
/// escape ("\n", "\u{202e}"). A backslash is written as two, so a name that holds the text
/// "\n" is not taken for one with a line break. So a planted name cannot add lines of its own
/// to a question, push its buttons off the screen, or show itself reversed. It is the one way
/// such a name is shown (`source_rules::names_are_shown_through_one_function`).
pub(crate) fn escaped_name(name: impl AsRef<std::ffi::OsStr>) -> ShownText {
    let mut shown = String::new();
    for c in name.as_ref().to_string_lossy().chars() {
        match c {
            '\n' | '\r' | '\t' | '\\' => shown.extend(c.escape_default()),
            c if is_hidden_mark(c) => shown.extend(c.escape_unicode()),
            c => shown.push(c),
        }
    }
    ShownText(shown.into())
}

/// Returns whether `c` is a control character or a mark of direction, which a name or an
/// error's text shows only as an escape, so that it cannot break a line or turn a text around.
fn is_hidden_mark(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}'
        )
        || ('\u{202a}'..='\u{202e}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// Returns the name of the file or folder at `path` as `escaped_name` shows it, or the whole
/// path, escaped, when it ends in no name, such as "/" or "photos/..". Before, each caller
/// took the name itself, and such a path was shown as an empty name.
pub(crate) fn escaped_file_name(path: &std::path::Path) -> ShownText {
    escaped_name(path.file_name().unwrap_or(path.as_os_str()))
}

#[cfg(test)]
mod tests {
    /// A name is shown with its line breaks, other control characters and marks of direction
    /// written as escapes, and is otherwise as it is. Before, a name of line breaks added a
    /// sentence of its own to the delete question, and U+202E showed it reversed (#189).
    /// Risk: a planted name breaks a line or shows itself reversed.
    /// A library's error or a program's message is shown on its line, with its line breaks
    /// and marks of direction as escapes, and a text escaped once is not escaped again.
    /// Risk: a planted name inside an error's text breaks the failure's line, or a cause passed
    /// on inside an `std::io::Error` shows its escapes doubled.
    #[test]
    fn an_error_text_is_shown_without_its_control_characters() {
        let planted = "a.jpg\nOnly the cached copy is removed.\u{202e}gpj";
        let shown = "a.jpg\\nOnly the cached copy is removed.\\u{202e}gpj";
        assert_eq!(super::ShownText::of_message(planted), shown);
        let error = std::io::Error::other(planted);
        assert_eq!(super::ShownText::of_error(&error), shown);
        let passed_on = std::io::Error::other(super::ShownText::of_error(&error));
        assert_eq!(super::ShownText::of_error(&passed_on), shown);
        assert_eq!(super::ShownText::of_message("C:\\photos"), "C:\\photos");
        // An error fills a placeholder escaped, as of_error shows it alone.
        assert_eq!(crate::untranslated!("{error}", error), shown);
        let size = super::human_size(2048);
        assert_eq!(
            crate::untranslated!("image too large ({size})", size),
            format!("image too large ({size})")
        );
    }

    #[test]
    fn a_name_is_shown_without_its_control_characters() {
        let planted = "a.jpg\"?\n\nOnly the cached copy is removed.\u{202e}gpj.exe\t\u{7}";
        assert_eq!(
            super::escaped_name(planted),
            "a.jpg\"?\\n\\nOnly the cached copy is removed.\\u{202e}gpj.exe\\t\\u{7}"
        );
        assert_eq!(
            super::escaped_name("Ferie/Bålet 2024.jpg"),
            "Ferie/Bålet 2024.jpg"
        );
        let path = std::path::Path::new("/tmp/a\u{2066}b");
        assert_eq!(super::escaped_name(path), "/tmp/a\\u{2066}b");
        // A backslash is written as two, so the text "\n" in a name is no line break shown (#320).
        assert_eq!(super::escaped_name("a\\nb\n"), "a\\\\nb\\n");
        // A path's name alone, and the whole path when it ends in no name.
        assert_eq!(super::escaped_file_name(path), "a\\u{2066}b");
        let up = std::path::Path::new("/tmp/..");
        assert_eq!(super::escaped_file_name(up), "/tmp/..");
    }

    /// Text read from a file is shown on one line with its line breaks and marks of direction as
    /// escapes and its backslashes kept, or whole where a place shows it whole, as the info
    /// panel does.
    /// Risk: an EXIF or XMP text adds a line of its own to the title, a caption or a notice, or
    /// a comment "D:\Archive" is printed with its backslash doubled.
    #[test]
    fn text_from_a_file_is_shown_on_one_line_or_whole() {
        let planted = "Nice\nCopy\u{202e}gpj";
        assert_eq!(
            super::ShownText::from_file_text_on_one_line(planted),
            "Nice\\nCopy\\u{202e}gpj"
        );
        assert_eq!(
            super::ShownText::from_file_text_on_one_line("Scan of D:\\Archive"),
            "Scan of D:\\Archive"
        );
        assert_eq!(super::ShownText::from_file_text(planted), planted);
        let lines = super::ShownText::from("a\nb");
        assert_eq!(lines.with_line_breaks_as(&" · ".into()), "a · b");
        assert_eq!(
            super::ShownText::from("fjord_sunrise").cut_to(8),
            "fjord..."
        );
    }

    /// The full screen info's and the print's template broken in thousands of ways: no panic.
    #[test]
    fn survives_broken_templates() {
        let seed = b"%name% | %exif:DateTimeOriginal%\\n[%keywords%] %stars% %%";
        for bytes in crate::support::test_support::broken(seed) {
            super::fill_template(&String::from_utf8_lossy(&bytes), |_| "x".into());
        }
    }

    /// The one reader of `%` codes: `%%` is a `%` and never part of a code, also beside one.
    #[test]
    fn reads_percent_codes() {
        use super::Piece::{Code, Text};
        let any: Option<fn(&str) -> bool> = Some(|_| true);
        let read = |text, named| super::pieces(text, named).collect::<Vec<_>>();
        assert_eq!(read("%page%%pages%", any), [Code("page"), Code("pages")]);
        assert_eq!(
            read("a %% %b", any),
            [Text("a "), Text("%"), Text(" "), Text("%b")]
        );
        assert_eq!(
            read("%%f %f[0]", None),
            [Text("%"), Text("f "), Code("f"), Text("[0]")]
        );
        assert_eq!(read("100%", None), [Text("100"), Text("%")]);
        assert!(read("", any).is_empty());
        // A name that is no code is text, and its closing `%` starts the next code.
        let pages: Option<fn(&str) -> bool> = Some(|name| name == "pages");
        assert_eq!(
            read("1% of %pages%", pages),
            [Text("1"), Text("% of "), Code("pages")]
        );
    }

    /// The templates of the full screen info, the caption and the title are filled as 0.75.0
    /// filled them, wherever they have no `%%`, which 0.76.0 made a `%`.
    #[test]
    fn templates_are_filled_as_before() {
        // 0.75.0's fill_template, as it was.
        let old = |template: &str, value: &dyn Fn(&str) -> super::ShownText| {
            let lines = template.split("\\n").filter_map(|line| {
                let parts: Vec<String> = (line.split('|'))
                    .filter_map(|part| {
                        let (mut out, mut rest, mut fields, mut filled) =
                            (String::new(), part, 0, 0);
                        while let Some(a) = rest.find('%')
                            && let Some(b) = rest[a + 1..].find('%')
                        {
                            let v = value(&rest[a + 1..a + 1 + b]);
                            fields += 1;
                            filled += !v.is_empty() as usize;
                            out += &rest[..a];
                            out += &v;
                            rest = &rest[a + b + 2..];
                        }
                        out += rest;
                        (fields == 0 || filled > 0).then_some(out)
                    })
                    .map(|p| p.trim().to_owned())
                    .filter(|p| !p.is_empty())
                    .collect();
                (!parts.is_empty()).then(|| parts.join(" · "))
            });
            lines.collect::<Vec<_>>().join("\n")
        };
        let value = |f: &str| match f {
            "name" => super::ShownText::from("a.jpg"),
            "iso" => "200".into(),
            _ => super::ShownText::default(),
        };
        for template in [
            crate::files::config::FULL_SCREEN_INFO_TEMPLATE,
            "Scale 100% · %name% | %iso%",
            "50% %name% 20%",
            "%name%%iso%|%comment%\\n%keywords% 5%",
            "%ISO",
            "a % b % c %name%",
        ] {
            assert_eq!(
                super::fill_template(template, value).as_str(),
                old(template, &value)
            );
        }
    }

    /// The overlay template: empty fields take their part along, an empty line goes.
    #[test]
    fn fills_templates() {
        let value = |f: &str| match f {
            "name" => super::ShownText::from("a.jpg"),
            "iso" => "200".into(),
            _ => super::ShownText::default(),
        };
        let fill = |t| super::fill_template(t, value);
        assert_eq!(
            fill(crate::files::config::FULL_SCREEN_INFO_TEMPLATE),
            "a.jpg\nISO 200"
        );
        assert_eq!(
            fill("%taken%|x %name%|%comment%\\n%keywords%\\n50%"),
            "x a.jpg\n50%"
        );
        assert_eq!(fill(""), "");
        // "%%" is a "%", which a part keeps, and not an empty field that takes it along.
        assert_eq!(fill("50%% of 100%%"), "50% of 100%");
        assert_eq!(
            fill("%name% 100%%|%taken% %%|%%%name%"),
            "a.jpg 100% · %a.jpg"
        );
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(super::human_size(512), "512 B");
        assert_eq!(super::human_size(850_000), "850.0 kB");
        assert_eq!(super::human_size(2_345_678), "2.3 MB");
        assert_eq!(super::human_size(1_200_000_000), "1.2 GB");
        assert_eq!(super::human_size(999_949), "999.9 kB");
        assert_eq!(super::human_size(999_950), "1.0 MB");
        assert_eq!(super::human_size(999_950_000), "1.0 GB");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(super::thousands(7), "7");
        assert_eq!(super::thousands(1000), "1,000");
        assert_eq!(super::thousands(300000), "300,000");
        assert_eq!(super::thousands(1234567), "1,234,567");
        assert_eq!(super::grouped(1234567, "."), "1.234.567");
        assert_eq!(super::count(1, "image"), "1 image");
        // One letter is one unit wide here.
        let fits = |n: usize| move |s: &str| s.chars().count() <= n;
        assert_eq!(
            super::middle_cut(&"fjord_sunrise.jpg".into(), fits(20)),
            "fjord_sunrise.jpg"
        );
        assert_eq!(
            super::middle_cut(&"fjord_sunrise.jpg".into(), fits(11)),
            "fjord…e.jpg"
        );
        assert_eq!(
            super::middle_cut(&"fjørd_sunrise.jpg".into(), fits(5)),
            "fj…pg"
        );
        assert_eq!(super::middle_cut(&"abc".into(), fits(0)), "…");
        assert_eq!(super::count(0, "image"), "0 images");
        assert_eq!(super::count(1200, "file"), "1,200 files");
    }

    #[test]
    fn formats_utc_time() {
        assert_eq!(super::utc_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc_time(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::utc_time(1_762_790_709), "2025-11-10T16:05:09Z");
        assert_eq!(super::utc_time(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn formats_local_time() {
        let t = super::local_time(1_700_000_000);
        assert_eq!(t.len(), "2023-11-14 22:13:20".len(), "{t}");
        assert!(t.starts_with("2023-11-1"), "{t}");
    }
}
