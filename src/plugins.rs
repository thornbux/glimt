//! Plugins, as Geeqie's: programs in desktop files (`*.desktop`), in the Edit → Plugins menu,
//! run on the selected files. They are read from `glimt/plugins` in the XDG data folders
//! (`~/.local/share/glimt/plugins` first). Geeqie's own plugins are not read: many of them ask
//! a running Geeqie for things with `geeqie --remote`.

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
    let home = crate::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt");
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or("/usr/local/share:/usr/share".into());
    let mut seen = std::collections::HashSet::new();
    let mut plugins: Vec<Plugin> = (home.into_iter())
        .chain(dirs.split(':').map(|d| Path::new(d).join("glimt")))
        .map(|d| d.join("plugins"))
        .flat_map(|dir| std::fs::read_dir(&dir).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
        .filter(|p| seen.insert(p.file_name().map(|n| n.to_owned())))
        .filter_map(|p| parse(&std::fs::read_to_string(&p).ok()?, p.parent()?))
        .collect();
    plugins.sort_by_key(|p| p.name.to_lowercase());
    plugins
}

/// The plugin of a desktop file; `None` if it is hidden or has no command.
fn parse(text: &str, dir: &Path) -> Option<Plugin> {
    // Only the [Desktop Entry] group, not its actions.
    let entry = text.split("\n[").next()?;
    let value = |key: &str| {
        (entry.lines())
            .find_map(|l| l.strip_prefix(key)?.trim_start().strip_prefix('='))
            .map(str::trim)
    };
    if value("Hidden") == Some("true") || value("NoDisplay") == Some("true") {
        return None;
    }
    Some(Plugin {
        name: value("Name")?.to_owned(),
        exec: value("Exec")?.to_owned(),
        dir: dir.to_owned(),
    })
}

/// The words of a desktop file's Exec line: separated by spaces, "quoted" ones may hold them.
fn words(exec: &str) -> Vec<String> {
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

impl Plugin {
    /// Run the plugin on `files` and wait for it: once for each file with %f or %u, once for all
    /// with %F or %U, and once in `folder` without any. The error output if it failed.
    pub fn run(&self, files: &[PathBuf], folder: &Path) -> Result<(), String> {
        let words = words(&self.exec);
        let codes = |w: &String| matches!(w.as_str(), "%f" | "%u" | "%F" | "%U");
        let each = words.iter().any(|w| w == "%f" || w == "%u");
        let runs: Vec<&[PathBuf]> = if each {
            files.chunks(1).collect()
        } else {
            vec![files]
        };
        for run in runs {
            let mut args = Vec::new();
            for w in &words {
                if codes(w) {
                    args.extend(run.iter().map(|f| f.as_os_str().to_owned()));
                } else if !(w.starts_with('%') && w.len() == 2) {
                    // Other field codes (%i, %c, %k) are left out, as the specification says.
                    args.push(w.replace("%%", "%").into());
                }
            }
            let Some(program) = args.first().cloned() else {
                return Err("no command".into());
            };
            let local = self.dir.join(&program);
            let program = if local.is_file() {
                local.into()
            } else {
                program
            };
            let out = crate::external::command(program.to_string_lossy().as_ref())
                .args(&args[1..])
                .current_dir(folder)
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
