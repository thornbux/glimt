//! Tools, as Geeqie's plugins: programs in desktop files (`*.desktop`), in the Edit → Tools menu,
//! run on the selected files. They are read from `glimt/tools` in the XDG data folders
//! (`~/.local/share/glimt/tools` first), and from the folders' old names in `RENAMED_FOLDERS`.
//! Geeqie's own plugins are not read: many of them ask a running Geeqie for things with
//! `geeqie --remote`. The desktop files of added formats are read with the same `desktop_files`
//! and `desktop_entry_values`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The folders of desktop files that 0.75.0 renamed, by their old name and the new one, in
/// Glimt's folder of the user's data folder and of the system's. Glimt reads the old folder too,
/// forever, and opens and creates only the new one. A desktop file in both under the same name
/// counts once, from the new folder. The desktop files keep their format: no key names a plugin
/// or a decoder, and an added format's group header stays `[Glimt Decoder]`. The header is never
/// checked, so `[Glimt Format]` is read as well.
pub const RENAMED_FOLDERS: [(&str, &str); 2] = [("plugins", "tools"), ("decoders", "formats")];

/// The tools running, by name and process ID, for Stop in Edit → Tools: one may be an
/// editor that stays open for hours, or a script that never ends.
pub type RunningTools = std::sync::Arc<std::sync::Mutex<Vec<(String, u32)>>>;

#[derive(Clone)]
pub struct Tool {
    pub name: String,
    /// The command line, with %f, %F, %u or %U for the files.
    exec: String,
    /// The folder of the desktop file: a program there is found without being on the PATH.
    dir: PathBuf,
    /// The MIME types of the files it takes, from the desktop file's MimeType=, such as
    /// image/x-canon-cr2 or image/*. When the list is empty, the tool takes every file.
    types: Vec<String>,
}

impl Tool {
    /// Returns whether the tool accepts `path`: every file when its desktop file names no
    /// MimeType, otherwise a file whose extension has one of those types in the desktop's
    /// MIME database. A file of a type the database does not know is not accepted then.
    pub fn accepts(&self, path: &Path) -> bool {
        if self.types.is_empty() {
            return true;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
        let Some(mime) = ext.and_then(|e| mime_types().get(&e)) else {
            return false;
        };
        (self.types.iter()).any(|t| match t.strip_suffix("/*") {
            Some(major) => mime.split('/').next() == Some(major),
            None => t == mime,
        })
    }

    /// Run the tool on `files` and wait for it: once for each file with %f or %u, once for all
    /// with %F or %U, and once in `folder` without any. Each run is in `running` while it runs;
    /// one taken out of it there was stopped, and the runs after it are not made. The error
    /// output if it failed.
    pub fn run(
        &self,
        files: &[PathBuf],
        folder: &Path,
        running: &RunningTools,
    ) -> Result<(), String> {
        let words = command_words(&self.exec);
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
            // kept whole in memory; its errors are. In a process group of its own, so Stop ends
            // what it started too (in a Flatpak through INT, see `external::stop_process_group`).
            // Not `stoppable_command`: its --watch-bus would end an editor on the host when the
            // Flatpak's Glimt closes, with its unsaved work.
            use std::os::unix::process::CommandExt;
            let mut child =
                crate::files::external::host_command(program.to_string_lossy().as_ref())
                    .process_group(0)
                    .args(&args[1..])
                    .current_dir(folder)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("{}: {e}", program.to_string_lossy()))?;
            let pid = child.id();
            running.lock().unwrap().push((self.name.clone(), pid));
            // Its errors are read in a thread of their own, and only the last of them are kept.
            // A program the tool started in the background (an editor, `xdg-open`) may hold
            // them open for hours, so the tool is waited for, not the end of its errors.
            let errors = child.stderr.take().map(|e| {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || tx.send(crate::files::external::last_text(e, 64 << 10)));
                rx
            });
            #[allow(clippy::disallowed_methods)] // the user's own program, which may take long
            let status = child.wait();
            let mut now = running.lock().unwrap();
            match now.iter().position(|r| r.1 == pid) {
                Some(at) => drop(now.remove(at)),
                None => return Err(crate::support::lang::tr("stopped").into()),
            }
            drop(now);
            let status = status.map_err(|e| format!("{}: {e}", program.to_string_lossy()))?;
            if !status.success() {
                let wait = std::time::Duration::from_millis(500);
                let err = errors.and_then(|rx| rx.recv_timeout(wait).ok());
                let err = err.unwrap_or_default();
                return Err(err.lines().last().unwrap_or("failed").to_owned());
            }
        }
        Ok(())
    }
}

/// The tools, sorted by name. A desktop file in the user's folder hides one of the same name
/// in the system's.
pub fn find() -> Vec<Tool> {
    let mut tools: Vec<Tool> = (desktop_files("tools").into_iter())
        .filter_map(|(path, text)| parse(&text, path.parent()?))
        .collect();
    tools.sort_by_key(|t| t.name.to_lowercase());
    tools
}

/// Returns the desktop files in the folder `kind` ("tools" or "formats") of Glimt's folder in
/// the user's data folder and the system's, with their text. One in the user's hides one of the
/// same name in the system's.
pub fn desktop_files(kind: &str) -> Vec<(PathBuf, String)> {
    let home = crate::files::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt");
    let system = system_data_dirs().into_iter().map(|d| d.join("glimt"));
    desktop_files_in(home.into_iter().chain(system), kind)
}

/// Returns the desktop files in the folder `kind` of each of `glimt_dirs`, with their text, and
/// those in the folder's old name of `RENAMED_FOLDERS` after it. The first file of a name
/// counts, so an earlier folder of `glimt_dirs` hides a later one, and the new folder hides the
/// old one.
fn desktop_files_in(
    glimt_dirs: impl Iterator<Item = PathBuf>,
    kind: &str,
) -> Vec<(PathBuf, String)> {
    let old_name = RENAMED_FOLDERS.iter().find(|r| r.1 == kind).map(|r| r.0);
    let mut seen = std::collections::HashSet::new();
    glimt_dirs
        .flat_map(|d| std::iter::once(d.join(kind)).chain(old_name.map(|old| d.join(old))))
        .flat_map(|dir| std::fs::read_dir(&dir).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
        .filter(|p| seen.insert(p.file_name().map(|n| n.to_owned())))
        .filter_map(|p| Some((p.clone(), crate::images::decode::read_text(&p).ok()?)))
        .collect()
}

/// The system's data folders, from $XDG_DATA_DIRS, which tools, added formats and the MIME
/// database are read from after the user's own.
fn system_data_dirs() -> Vec<PathBuf> {
    data_dirs_of(std::env::var("XDG_DATA_DIRS").ok())
}

/// Returns the absolute folders of `value`, the text of $XDG_DATA_DIRS. When it is not set or
/// is empty, they are /usr/local/share and /usr/share, as the XDG spec says. An empty value
/// read as no folders lost the system's tools and MIME types.
fn data_dirs_of(value: Option<String>) -> Vec<PathBuf> {
    let value = value.filter(|v| !v.is_empty());
    let value = value.as_deref().unwrap_or("/usr/local/share:/usr/share");
    absolute_dirs(value).map(Path::to_path_buf).collect()
}

/// Returns the folders of a list such as $XDG_DATA_DIRS that are absolute paths. The XDG spec
/// says to ignore a relative one. An empty entry ("/usr/share:") would otherwise be the current
/// folder, and a downloaded folder there could hold an added format that runs by itself.
fn absolute_dirs(list: &str) -> impl Iterator<Item = &Path> {
    list.split(':').map(Path::new).filter(|d| d.is_absolute())
}

/// The values of a desktop file's first group (a tool's [Desktop Entry] or an added format's
/// [Glimt Decoder], not its actions), by key.
pub fn desktop_entry_values<'a>(text: &'a str) -> impl Fn(&str) -> Option<&'a str> + 'a {
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

/// Returns the tool of a desktop file, or `None` if it is hidden or has no command.
fn parse(text: &str, dir: &Path) -> Option<Tool> {
    let value = desktop_entry_values(text);
    if value("Hidden") == Some("true") || value("NoDisplay") == Some("true") {
        return None;
    }
    let types = value("MimeType")
        .unwrap_or_default()
        .split(';')
        .map(str::trim);
    Some(Tool {
        name: value("Name")?.to_owned(),
        exec: value("Exec")?.to_owned(),
        dir: dir.to_owned(),
        types: types.filter(|t| !t.is_empty()).map(str::to_owned).collect(),
    })
}

/// The MIME type of each file name extension, lowercase, from shared-mime-info's `mime/globs2`
/// in the XDG data folders, read once. The user's own folder comes first, as the spec says.
// shortcut: read whole in the window's thread at the first tool run, some 100 kB; in the
// background if it is ever slow.
fn mime_types() -> &'static HashMap<String, String> {
    static TYPES: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    TYPES.get_or_init(|| {
        let home = crate::files::config::xdg_dir("XDG_DATA_HOME", ".local/share", "mime");
        let system = system_data_dirs().into_iter().map(|d| d.join("mime"));
        let all = home.into_iter().chain(system);
        let mut types = HashMap::new();
        for dir in all {
            let Ok(text) = crate::images::decode::read_text(dir.join("globs2")) else {
                continue;
            };
            for (ext, mime) in globs(&text) {
                types.entry(ext).or_insert(mime);
            }
        }
        types
    })
}

/// The extensions and their MIME types in the text of a globs2 file, whose lines are
/// `weight:type:glob[:flags]`, the heaviest first, as `*.cr2`. A glob that is more than an
/// extension, a line that is not of that form, and a case-sensitive glob (flag `cs`) with
/// capitals, such as C++'s `*.C`, which would take the place of `*.c`, are passed over. Returns
/// each extension once, lowercase, with the type of its heaviest line.
fn globs(text: &str) -> HashMap<String, String> {
    let mut found: HashMap<String, (u32, String)> = HashMap::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let mut parts = line.split(':');
        let (Some(weight), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(weight), Some(ext)) = (weight.parse::<u32>(), glob.strip_prefix("*.")) else {
            continue;
        };
        let flags = parts.next().unwrap_or_default();
        if flags.split(',').any(|f| f == "cs") && ext.chars().any(char::is_uppercase) {
            continue;
        }
        if ext.is_empty() || ext.contains(['*', '?', '[', '.']) || !mime.contains('/') {
            continue;
        }
        let ext = ext.to_lowercase();
        if found.get(&ext).is_none_or(|(w, _)| weight > *w) {
            found.insert(ext, (weight, mime.to_owned()));
        }
    }
    found
        .into_iter()
        .map(|(ext, (_, mime))| (ext, mime))
        .collect()
}

/// The words of a desktop file's Exec line or the video player's command: separated by spaces,
/// "quoted" ones may hold them.
pub fn command_words(exec: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    /// A tool's desktop file broken in thousands of ways: no panic.
    #[test]
    fn survives_broken_desktop_files() {
        let seed = b"[Desktop Entry]\nName=Stamp\nExec=stamp %f \"a b\" %%\nType=Application\n";
        for bytes in crate::support::test_support::broken(seed) {
            super::parse(
                &String::from_utf8_lossy(&bytes),
                std::path::Path::new("/tmp"),
            );
        }
    }

    /// A globs2 file broken in thousands of ways does not make the parser panic.
    #[test]
    fn survives_broken_globs() {
        let seed = b"# comment\n50:image/jpeg:*.jpg\n50:image/x-canon-cr2:*.cr2:cs\n10:a/b:*.x\n";
        for bytes in crate::support::test_support::broken(seed) {
            super::globs(&String::from_utf8_lossy(&bytes));
        }
    }

    /// A tool with MimeType= takes the files of those types, a type such as image/* all of
    /// its kind, and one of an unknown type none; without MimeType it takes every file.
    #[test]
    fn takes_the_types_it_names() {
        let text = "60:image/x-canon-cr2:*.cr2\n50:image/jpeg:*.jpg\n50:image/jpeg:*.jpeg\n\
                    40:text/plain:*.jpg\n50:video/mp4:*.mp4\n50:image/x:*.tar.gz\n10:bad\n\
                    60:text/x-c++src:*.C:cs\n50:text/x-csrc:*.c\n";
        let types = super::globs(text);
        assert_eq!(types.get("jpg").map(String::as_str), Some("image/jpeg"));
        assert_eq!(
            types.get("cr2").map(String::as_str),
            Some("image/x-canon-cr2")
        );
        assert_eq!(types.get("tar.gz"), None);
        assert_eq!(types.get("c").map(String::as_str), Some("text/x-csrc"));
        assert_eq!(types.len(), 5);
        // Spaces around the types are not part of them.
        let spaced = "[Desktop Entry]\nName=S\nExec=s\nMimeType=image/png; image/jpeg ;\n";
        let spaced = super::parse(spaced, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(spaced.types, ["image/png", "image/jpeg"]);
        let dir = std::path::Path::new("/tmp");
        let raw = super::parse(
            "[Desktop Entry]\nName=R\nExec=r\nMimeType=image/x-canon-cr2;\n",
            dir,
        );
        let raw = raw.unwrap();
        assert_eq!(raw.types, ["image/x-canon-cr2"]);
        let any = super::parse("[Desktop Entry]\nName=A\nExec=a\n", dir).unwrap();
        assert!(any.accepts(std::path::Path::new("/x/notes.weird")));
        let images = super::parse("[Desktop Entry]\nName=I\nExec=i\nMimeType=image/*\n", dir);
        let images = images.unwrap();
        // The database of this computer: a JPEG is an image everywhere it is installed, and
        // no type is known for a made-up extension.
        if super::mime_types().contains_key("jpg") {
            assert!(images.accepts(std::path::Path::new("/x/a.JPG")));
            assert!(!raw.accepts(std::path::Path::new("/x/a.jpg")));
        }
        assert!(!images.accepts(std::path::Path::new("/x/a.no-such-type")));
        assert!(!images.accepts(std::path::Path::new("/x/no-extension")));
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
    fn reads_only_absolute_data_dirs() {
        let dirs: Vec<&Path> = absolute_dirs("/usr/share::glimt:/opt/x/").collect();
        assert_eq!(dirs, [Path::new("/usr/share"), Path::new("/opt/x/")]);
        assert_eq!(absolute_dirs("").count(), 0);
        // Not set and set but empty are both the spec's default.
        let default = [
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ];
        assert_eq!(data_dirs_of(None), default);
        assert_eq!(data_dirs_of(Some(String::new())), default);
        assert_eq!(
            data_dirs_of(Some("/opt/x".into())),
            [PathBuf::from("/opt/x")]
        );
    }

    /// The folders' old names in `RENAMED_FOLDERS` are still read. A desktop file in both under
    /// the same name counts once, from the new folder, and the user's folder hides the system's.
    #[test]
    fn reads_the_old_folders_and_the_new_one_wins() {
        let dir = std::env::temp_dir().join(format!("glimt-test-folders-{}", std::process::id()));
        let (user, system) = (dir.join("user"), dir.join("system"));
        let write = |folder: &Path, name: &str, text: &str| {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join(name), text).unwrap();
        };
        write(&user.join("tools"), "both.desktop", "new");
        write(&user.join("plugins"), "both.desktop", "old");
        write(&user.join("plugins"), "old.desktop", "old only");
        write(&system.join("tools"), "old.desktop", "the system's");
        write(
            &system.join("plugins"),
            "system.desktop",
            "the system's old",
        );
        write(&user.join("plugins"), "notes.txt", "not a desktop file");
        write(&user.join("decoders"), "format.desktop", "an added format");
        let mut found: Vec<(String, String)> =
            desktop_files_in([user.clone(), system.clone()].into_iter(), "tools")
                .into_iter()
                .map(|(path, text)| (path.strip_prefix(&dir).unwrap().display().to_string(), text))
                .collect();
        found.sort();
        let expected = [
            ("system/plugins/system.desktop", "the system's old"),
            ("user/plugins/old.desktop", "old only"),
            ("user/tools/both.desktop", "new"),
        ];
        assert_eq!(found, expected.map(|(p, t)| (p.to_owned(), t.to_owned())));
        let formats = desktop_files_in([user, system].into_iter(), "formats");
        assert_eq!(formats.len(), 1);
        assert!(formats[0].0.ends_with("decoders/format.desktop"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_and_runs_a_tool() {
        assert_eq!(
            command_words(r#"sh -c "echo \"a b\" >> log" x %F"#),
            ["sh", "-c", "echo \"a b\" >> log", "x", "%F"]
        );
        let dir = std::env::temp_dir().join(format!("glimt-test-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = "[Desktop Entry]\nName=Mark\nExec=sh -c \"echo \\$0 >> log\" %f\n\
                    [Desktop Action x]\nName=Other\n";
        let tool = parse(text, &dir).unwrap();
        assert_eq!(tool.name, "Mark");
        assert!(parse("[Desktop Entry]\nName=A\nExec=a\nNoDisplay=true\n", &dir).is_none());
        // Written by hand, with a comment first.
        let commented = parse("# mine\n\n[Desktop Entry]\nName=C\nExec=c\n", &dir);
        assert_eq!(commented.unwrap().name, "C");
        // Once for each file, in the folder given.
        let files = [dir.join("a b.jpg"), dir.join("c.jpg")];
        tool.run(&files, &dir, &RunningTools::default()).unwrap();
        let log = std::fs::read_to_string(dir.join("log")).unwrap();
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            files.clone().map(|f| f.display().to_string())
        );
        let failing = parse(
            "[Desktop Entry]\nName=F\nExec=sh -c \"echo no >&2; exit 1\"\n",
            &dir,
        );
        assert_eq!(
            failing.unwrap().run(&files, &dir, &RunningTools::default()),
            Err("no".into())
        );
        // One that leaves a program running in the background with its errors open: done once
        // the tool itself has ended.
        let helper = "[Desktop Entry]\nName=Helper\nExec=sh -c \"sleep 5 & exit 0\"\n";
        let start = std::time::Instant::now();
        let ran = parse(helper, &dir)
            .unwrap()
            .run(&files, &dir, &RunningTools::default());
        assert_eq!(ran, Ok(()));
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        // One that does not end, stopped from the menu: ended with what it started, and told.
        let endless = "[Desktop Entry]\nName=Endless\nExec=sh -c \"sleep 30; sleep 30\"\n";
        let endless = parse(endless, &dir).unwrap();
        let running = RunningTools::default();
        let start = std::time::Instant::now();
        let stopping = {
            let running = running.clone();
            std::thread::spawn(move || {
                while running.lock().unwrap().is_empty() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                let (_, pid) = running.lock().unwrap().remove(0);
                crate::files::external::stop_process_group(pid);
            })
        };
        assert_eq!(endless.run(&files, &dir, &running), Err("stopped".into()));
        stopping.join().unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
