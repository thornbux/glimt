//! Collections, as in Geeqie: named lists of images from any folders. Each is a Geeqie `.gqv`
//! file, one quoted path per line, kept in `$XDG_DATA_HOME/glimt/collections`; Geeqie's own
//! collections can be opened too.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Where new collections go.
pub fn dir() -> Option<PathBuf> {
    crate::config::xdg_dir("XDG_DATA_HOME", ".local/share", "glimt").map(|d| d.join("collections"))
}

pub fn is_collection(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gqv"))
        && path.is_file()
}

/// The saved collections: (name, file), by name.
pub fn list() -> Vec<(String, PathBuf)> {
    let entries = dir().and_then(|d| std::fs::read_dir(d).ok());
    let mut all: Vec<(String, PathBuf)> = (entries.into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "gqv"))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    all.sort();
    all
}

/// The file of a new collection called `name`, or why there can be none.
pub fn new_file(name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(format!("Not a valid collection name: \"{name}\""));
    }
    let file = dir()
        .ok_or("no data folder ($HOME is not set)")?
        .join(format!("{name}.gqv"));
    match file.exists() {
        true => Err(format!("There is a collection called \"{name}\" already")),
        false => Ok(file),
    }
}

pub fn load(file: &Path) -> std::io::Result<Vec<PathBuf>> {
    Ok(parse(&std::fs::read(file)?))
}

/// Write `paths` to `file`, through a temporary file so a crash never leaves half of it.
pub fn save(file: &Path, paths: &[PathBuf]) -> std::io::Result<()> {
    // This Glimt's name in it: Flatpak windows can share a process ID.
    let tmp = file.with_extension(format!("{}.tmp", crate::remote::me()));
    std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(&tmp, to_text(paths))?;
    std::fs::rename(&tmp, file)
}

/// Add `paths` to the collection in `file` (made if new), leaving out those it has.
pub fn append(file: &Path, paths: &[PathBuf]) -> std::io::Result<usize> {
    // Only a collection that is not there yet starts empty: one that cannot be read would be
    // written over.
    let mut all = match load(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        loaded => loaded?,
    };
    let before = all.len();
    for path in paths {
        if !all.contains(path) {
            all.push(path.clone());
        }
    }
    save(file, &all)?;
    Ok(all.len() - before)
}

/// Geeqie's format: comment lines start with #, a path is in double quotes.
fn parse(text: &[u8]) -> Vec<PathBuf> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let rest = line.strip_prefix(b"\"")?;
            let end = rest.iter().rposition(|&b| b == b'"')?;
            Some(PathBuf::from(OsStr::from_bytes(&rest[..end])))
        })
        .filter(|p| p.is_absolute())
        .collect()
}

fn to_text(paths: &[PathBuf]) -> Vec<u8> {
    // Geeqie still writes the marker of GQview, which it came from.
    let head = format!(
        "#GQview collection\n#created with Glimt version {}\n",
        env!("CARGO_PKG_VERSION")
    );
    let mut text = head.into_bytes();
    for path in paths {
        let bytes = path.as_os_str().as_bytes();
        if !bytes.contains(&b'\n') {
            text.push(b'"');
            text.extend(bytes);
            text.extend(b"\"\n");
        }
    }
    text.extend(b"#end\n");
    text
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[test]
    fn reads_and_writes_geeqie_collections() {
        let paths = [
            PathBuf::from("/a b/c.jpg"),
            PathBuf::from("/x/\"quoted\".png"),
        ];
        let text = super::to_text(&paths);
        assert!(text.starts_with(b"#GQview collection\n"));
        assert_eq!(super::parse(&text), paths);
        // As Geeqie writes it, with its geometry line; relative paths are left out.
        let geeqie =
            b"#GQview collection\n#geometry: 0 0 400 300\n\"/p/1.jpg\"\n\"rel.jpg\"\n#end\n";
        assert_eq!(super::parse(geeqie), [PathBuf::from("/p/1.jpg")]);
        // Added to: a new one is made, one that cannot be read is left as it is.
        let dir = std::env::temp_dir().join(format!("glimt-test-gqv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.gqv");
        assert_eq!(super::append(&file, &paths).unwrap(), 2);
        assert_eq!(super::append(&file, &paths[..1]).unwrap(), 0);
        // A link to itself: reading fails, though not as missing.
        let unreadable = dir.join("b.gqv");
        std::os::unix::fs::symlink(&unreadable, &unreadable).unwrap();
        assert!(super::append(&unreadable, &paths).is_err());
        assert!(unreadable.symlink_metadata().unwrap().is_symlink());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
