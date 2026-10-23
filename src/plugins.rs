//! Plugins, as Geeqie's: programs in desktop files (`*.desktop`), in the Edit → Plugins menu,
//! run on the selected files. They are read from `glimt/plugins` in the XDG data folders
//! (`~/.local/share/glimt/plugins` first). Geeqie's own plugins are not read: many of them ask
//! a running Geeqie for things with `geeqie --remote`. Decoder plugins' desktop files are read
//! with the same `desktop_files` and `values`.

use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Plugin {
    pub name: String,
    /// The command line, with %f, %F, %u or %U for the files.
    exec: String,
    /// The folder of the desktop file: a program there is found without being on the PATH.
    dir: PathBuf,
}

/// The plugins, sorted by name; a desktop file in the user's folder hides one of the same name
/// in the system's.
pub fn find() -> Vec<Plugin> {
    let mut plugins: Vec<Plugin> = (desktop_files("plugins").into_iter())
        .filter_map(|(path, text)| parse(&text, path.parent()?))
        .collect();
    plugins.sort_by_key(|p| p.name.to_lowercase());
    plugins
}

/// The desktop files in `kind` (plugins, decoders) of Glimt's folder in the user's data folder
/// and the system's, with their text: one in the user's hides one of the same name in the
/// system's.
pub fn desktop_files(kind: &str) -> Vec<(std::path::PathBuf, String)> {
    let home = crate::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt");
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or("/usr/local/share:/usr/share".into());
    let mut seen = std::collections::HashSet::new();
    (home.into_iter())
        .chain(dirs.split(':').map(|d| Path::new(d).join("glimt")))
        .map(|d| d.join(kind))
        .flat_map(|dir| std::fs::read_dir(&dir).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
        .filter(|p| seen.insert(p.file_name().map(|n| n.to_owned())))
        .filter_map(|p| Some((p.clone(), crate::decode::read_text(&p).ok()?)))
        .collect()
}

/// The values of a desktop file's first group (its [Desktop Entry] or [Glimt Decoder], not its
/// actions), by key.
pub fn values<'a>(text: &'a str) -> impl Fn(&str) -> Option<&'a str> + 'a {
    // Comments and blank lines may come before its header.
    let mut rest = text;
    while let Some(line) = rest.lines().next()
        && (line.trim().is_empty() || line.starts_with('#'))
    {
        rest = rest.split_once('\n').map_or("", |(_, r)| r);
    }
    let entry = rest.split("\n[").next().unwrap_or_default();
    move |key| {
        (entry.lines())
            .find_map(|l| l.strip_prefix(key)?.trim_start().strip_prefix('='))
            .map(str::trim)
    }
}

/// The plugin of a desktop file; `None` if it is hidden or has no command.
fn parse(text: &str, dir: &Path) -> Option<Plugin> {
    let value = values(text);
    if value("Hidden") == Some("true") || value("NoDisplay") == Some("true") {
        return None;
    }
    Some(Plugin {
        name: value("Name")?.to_owned(),
        exec: value("Exec")?.to_owned(),
        dir: dir.to_owned(),
    })
}

/// The words of a desktop file's Exec line or the video player's command: separated by spaces,
/// "quoted" ones may hold them.
pub fn words(exec: &str) -> Vec<String> {
    let (mut out, mut word, mut quoted, mut chars) =
        (Vec::new(), None::<String>, false, exec.chars());
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                word.get_or_insert_default();
            }
            '\\' if quoted => word.get_or_insert_default().extend(chars.next()),
            ' ' | '\t' if !quoted => out.extend(word.take()),
            c => word.get_or_insert_default().push(c),
        }
    }
    out.extend(word);
    out
}

/// The command line of `words` (a desktop file's Exec) for `files`: %f, %u, %F and %U are the
/// files, %% is a %, and other field codes (%i, %c, %k) are left out, as the specification says.
fn arguments(words: &[String], files: &[PathBuf]) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    for w in words {
        match w.as_str() {
            "%f" | "%u" | "%F" | "%U" => {
                args.extend(files.iter().map(|f| f.as_os_str().to_owned()))
            }
            code if code.len() == 2 && code.starts_with('%') && code != "%%" => {}
            _ => args.push(w.replace("%%", "%").into()),
        }
    }
    args
}

impl Plugin {
    /// Run the plugin on `files` and wait for it: once for each file with %f or %u, once for all
    /// with %F or %U, and once in `folder` without any. The error output if it failed.
    pub fn run(&self, files: &[PathBuf], folder: &Path) -> Result<(), String> {
        let words = words(&self.exec);
        let each = words.iter().any(|w| w == "%f" || w == "%u");
        let runs: Vec<&[PathBuf]> = if each {
            files.chunks(1).collect()
        } else {
            vec![files]
        };
        for run in runs {
            let args = arguments(&words, run);
            let Some(program) = args.first().cloned() else {
                return Err("no command".into());
            };
            let local = self.dir.join(&program);
            let program = if local.is_file() {
                local.into()
            } else {
                program
            };
            // Its output is not read: a converter writing the image to stdout would else be
            // kept whole in memory; its errors are.
            let out = crate::external::command(program.to_string_lossy().as_ref())
                .args(&args[1..])
                .current_dir(folder)
                .stdout(std::process::Stdio::null())
                .output()
                .map_err(|e| format!("{}: {e}", program.to_string_lossy()))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(err.lines().last().unwrap_or("failed").to_owned());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// A plugin's desktop file broken in thousands of ways: no panic.
    #[test]
    fn survives_broken_desktop_files() {
        let seed = b"[Desktop Entry]\nName=Stamp\nExec=stamp %f \"a b\" %%\nType=Application\n";
        for bytes in crate::broken(seed) {
            super::parse(
                &String::from_utf8_lossy(&bytes),
                std::path::Path::new("/tmp"),
            );
        }
    }

    /// The files where %F stands, %% as a %, other field codes left out.
    #[test]
    fn builds_the_command_line() {
        let words: Vec<String> = ["printf", "%%", "%i", "100%%", "%F"]
            .map(String::from)
            .into();
        let files = [
            std::path::PathBuf::from("/a.jpg"),
            std::path::PathBuf::from("/b c.jpg"),
        ];
        let args = super::arguments(&words, &files);
        assert_eq!(args, ["printf", "%", "100%", "/a.jpg", "/b c.jpg"]);
    }

    use super::*;

    #[test]
    fn reads_and_runs_a_plugin() {
        assert_eq!(
            words(r#"sh -c "echo \"a b\" >> log" x %F"#),
            ["sh", "-c", "echo \"a b\" >> log", "x", "%F"]
        );
        let dir = std::env::temp_dir().join(format!("glimt-test-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = "[Desktop Entry]\nName=Mark\nExec=sh -c \"echo \\$0 >> log\" %f\n\
                    [Desktop Action x]\nName=Other\n";
        let plugin = parse(text, &dir).unwrap();
        assert_eq!(plugin.name, "Mark");
        assert!(parse("[Desktop Entry]\nName=A\nExec=a\nNoDisplay=true\n", &dir).is_none());
        // Written by hand, with a comment first.
        let commented = parse("# mine\n\n[Desktop Entry]\nName=C\nExec=c\n", &dir);
        assert_eq!(commented.unwrap().name, "C");
        // Once for each file, in the folder given.
        let files = [dir.join("a b.jpg"), dir.join("c.jpg")];
        plugin.run(&files, &dir).unwrap();
        let log = std::fs::read_to_string(dir.join("log")).unwrap();
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            files.clone().map(|f| f.display().to_string())
        );
        let failing = parse(
            "[Desktop Entry]\nName=F\nExec=sh -c \"echo no >&2; exit 1\"\n",
            &dir,
        );
        assert_eq!(failing.unwrap().run(&files, &dir), Err("no".into()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
