//! Lightroom Classic catalogs (`.lrcat`, an SQLite database), read once through the `sqlite3`
//! program: the collections, the keyword tree, the stars and each image's keywords, for Glimt's
//! own files. Only
//! columns that have been the same for many Lightroom versions are read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One query, so the catalog is read in one go: a row for each root folder (r), image (i),
/// collection (c), image in a collection (m), keyword (k) and keyword of an image (w), with five
/// columns each.
const QUERY: &str = "\
SELECT 'r', id_local, absolutePath, '', '' FROM AgLibraryRootFolder
UNION ALL SELECT 'i', i.id_local, f.rootFolder,
    f.pathFromRoot || fi.baseName || CASE WHEN fi.extension = '' THEN '' ELSE '.' || fi.extension END,
    CAST(coalesce(i.rating, 0) AS INTEGER)
    FROM Adobe_images i JOIN AgLibraryFile fi ON fi.id_local = i.rootFile
    JOIN AgLibraryFolder f ON f.id_local = fi.folder
UNION ALL SELECT 'c', id_local, coalesce(parent, 0), coalesce(name, ''), coalesce(creationId, '')
    FROM AgLibraryCollection
UNION ALL SELECT 'm', collection, image, '', '' FROM AgLibraryCollectionImage
UNION ALL SELECT 'k', id_local, coalesce(parent, 0), coalesce(name, ''), '' FROM AgLibraryKeyword
UNION ALL SELECT 'w', image, tag, '', '' FROM AgLibraryKeywordImage;";

/// Between the columns and after the rows: neither is in a file name or keyword.
const COLUMN: u8 = 0x1f;
const ROW: u8 = 0x1e;

#[derive(Default)]
pub struct Catalog {
    /// The root folders as the catalog names them, with a slash at the end.
    pub roots: Vec<(i64, String)>,
    /// Image: (root folder, path below it, stars).
    images: HashMap<i64, (i64, String, u8)>,
    /// Collection: (parent, name, kind).
    collections: HashMap<i64, (i64, String, String)>,
    /// (collection, image)
    members: Vec<(i64, i64)>,
    /// Keyword: (parent, name).
    keywords: HashMap<i64, (i64, String)>,
    /// (image, keyword)
    tagged: Vec<(i64, i64)>,
}

/// Read `file`; an error says why in words for the user.
pub fn read(file: &Path) -> Result<Catalog, String> {
    if file.with_extension("lrcat.lock").exists() {
        return Err("Lightroom has the catalog open: close Lightroom first".into());
    }
    let out = crate::external::command("sqlite3")
        .args([
            "-readonly",
            "-batch",
            "-noheader",
            "-separator",
            "\x1f",
            "-newline",
            "\x1e",
        ])
        .arg(file)
        .arg(QUERY)
        .output()
        .map_err(|e| format!("{NEEDS}: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        if missing(out.status.code(), &why) {
            return Err(format!("{NEEDS}, which is not installed"));
        }
        return Err(format!(
            "Not a Lightroom Classic catalog Glimt can read: {}",
            why.trim()
        ));
    }
    Ok(parse(&out.stdout))
}

const NEEDS: &str = "Reading a Lightroom catalog needs the sqlite3 program";

/// Whether sqlite3 is not there: in a Flatpak, flatpak-spawn starts but cannot run it on the
/// host, and a shell says 127.
fn missing(code: Option<i32>, stderr: &str) -> bool {
    code == Some(127) || stderr.contains("Failed to execute child process")
}

fn parse(text: &[u8]) -> Catalog {
    let mut c = Catalog::default();
    for row in text.split(|&b| b == ROW) {
        let cols: Vec<String> = (row.split(|&b| b == COLUMN))
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let [kind, id, a, b, d] = &cols[..] else {
            continue;
        };
        let (Ok(id), n) = (id.parse::<i64>(), a.parse::<i64>()) else {
            continue;
        };
        match (kind.as_str(), n) {
            ("r", _) => c.roots.push((id, a.clone())),
            ("i", Ok(n)) => drop(
                c.images
                    .insert(id, (n, b.clone(), d.parse().unwrap_or(0).min(5))),
            ),
            ("c", Ok(n)) => drop(c.collections.insert(id, (n, b.clone(), d.clone()))),
            ("m", Ok(n)) => c.members.push((id, n)),
            ("k", Ok(n)) => drop(c.keywords.insert(id, (n, b.clone()))),
            ("w", Ok(n)) => c.tagged.push((id, n)),
            _ => {}
        }
    }
    c
}

impl Catalog {
    /// The path of each image, its root folder replaced by `now`, the folder where the user
    /// says it is now (in the order of `roots`).
    pub fn paths(&self, now: &[PathBuf]) -> HashMap<i64, PathBuf> {
        // A root folder the user gave no place is left out.
        let roots: HashMap<i64, &PathBuf> = (self.roots.iter().map(|r| r.0))
            .zip(now)
            .filter(|r| !r.1.as_os_str().is_empty())
            .collect();
        (self.images.iter())
            .filter_map(|(&id, (root, below, _))| Some((id, roots.get(root)?.join(below))))
            .collect()
    }

    /// The plain collections (not smart ones, sets or publish services), named with the sets
    /// they are in ("Travel - Norway"), and their images at `paths` (from `Catalog::paths`).
    pub fn collections(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(String, Vec<PathBuf>)> {
        let mut images: HashMap<i64, Vec<PathBuf>> = HashMap::new();
        for (coll, image) in &self.members {
            if let Some(p) = paths.get(image) {
                images.entry(*coll).or_default().push(p.clone());
            }
        }
        let mut all: Vec<(String, Vec<PathBuf>)> = (self.collections.iter())
            .filter(|(_, c)| c.2 == "com.adobe.ag.library.collection")
            .map(|(id, _)| {
                let names = chain(*id, |i| {
                    self.collections.get(&i).map(|c| (c.0, c.1.as_str()))
                });
                let name = names.join(" - ").replace('/', "-");
                let name = name.trim_start_matches('.').to_owned();
                (name, images.remove(id).unwrap_or_default())
            })
            .filter(|(name, images)| !name.trim().is_empty() && !images.is_empty())
            .collect();
        all.sort();
        all
    }

    /// The keywords as keyword tree paths ("Places|Norway|Oslo").
    pub fn keyword_paths(&self) -> Vec<String> {
        (self.keywords.keys())
            .filter_map(|&id| self.keyword_path(id))
            .collect()
    }

    fn keyword_path(&self, id: i64) -> Option<String> {
        let names = chain(id, |i| self.keywords.get(&i).map(|k| (k.0, k.1.as_str())));
        (!names.is_empty()).then(|| names.join("|"))
    }

    /// The keywords of each image, as keyword tree paths, at `paths` (from `Catalog::paths`).
    /// One list for each file: the keywords of its virtual copies too, each once.
    pub fn image_keywords(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(PathBuf, Vec<String>)> {
        let mut per_file: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for (image, keyword) in &self.tagged {
            let (Some(path), Some(tag)) = (paths.get(image), self.keyword_path(*keyword)) else {
                continue;
            };
            let tags = per_file.entry(path.clone()).or_default();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        let mut all: Vec<(PathBuf, Vec<String>)> = per_file.into_iter().collect();
        all.iter_mut().for_each(|(_, tags)| tags.sort());
        all.sort();
        all
    }

    /// The images with stars, at `paths` (from `Catalog::paths`). One value for each file: a
    /// virtual copy is an image of its own on the same file, and the most stars of them count.
    #[cfg_attr(not(feature = "marks"), allow(dead_code))]
    pub fn stars(&self, paths: &HashMap<i64, PathBuf>) -> Vec<(PathBuf, u8)> {
        let mut best: HashMap<PathBuf, u8> = HashMap::new();
        for (id, i) in self.images.iter().filter(|(_, i)| i.2 > 0) {
            if let Some(p) = paths.get(id) {
                let n = best.entry(p.clone()).or_default();
                *n = (*n).max(i.2);
            }
        }
        best.into_iter().collect()
    }
}

/// The names from the top down to `id`, through `parent_and_name`; the nameless root that
/// Lightroom keeps keywords under is left out. A loop in a damaged catalog ends after 32 steps.
fn chain<'a>(mut id: i64, parent_and_name: impl Fn(i64) -> Option<(i64, &'a str)>) -> Vec<String> {
    let mut names = Vec::new();
    for _ in 0..32 {
        let Some((parent, name)) = parent_and_name(id) else {
            break;
        };
        if !name.is_empty() {
            names.push(name.replace('|', "/"));
        }
        id = parent;
    }
    names.reverse();
    names
}

/// Where each root folder is now: the same place if it is there, else empty for the user to
/// fill in.
/// The folder typed for a root folder: `~/` is the home folder, and a relative path or nothing
/// is no folder (empty), as a collection keeps absolute paths only.
pub fn root_now(typed: &str) -> PathBuf {
    let typed = typed.trim();
    let path = match typed.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(typed),
    };
    if path.is_absolute() {
        path
    } else {
        PathBuf::new()
    }
}

pub fn roots_now(catalog: &Catalog) -> Vec<String> {
    (catalog.roots.iter())
        .map(|(_, r)| {
            if Path::new(r).is_dir() {
                r.clone()
            } else {
                String::new()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// A catalog as sqlite3 writes it: a root folder on a Mac, two images, a collection in a
    /// set, a smart collection, and keywords under Lightroom's nameless root.
    fn sample() -> Vec<u8> {
        let rows = [
            "r\x1f1\x1f/Volumes/Photos/\x1f\x1f",
            "i\x1f10\x1f1\x1f2024/Oslo/a.jpg\x1f4",
            "i\x1f11\x1f1\x1f2024/b.CR3\x1f0",
            "c\x1f20\x1f0\x1fTravel\x1fcom.adobe.ag.library.group",
            "c\x1f21\x1f20\x1fNorway\x1fcom.adobe.ag.library.collection",
            "c\x1f22\x1f0\x1fFive stars\x1fcom.adobe.ag.library.smart_collection",
            "m\x1f21\x1f10\x1f\x1f",
            "m\x1f21\x1f11\x1f\x1f",
            "m\x1f22\x1f10\x1f\x1f",
            "k\x1f30\x1f0\x1f\x1f",
            "k\x1f31\x1f30\x1fPlaces\x1f",
            "k\x1f32\x1f31\x1fOslo\x1f",
            "w\x1f10\x1f32\x1f\x1f",
            "w\x1f11\x1f31\x1f\x1f",
            "w\x1f12\x1f32\x1f\x1f",
            "w\x1f99\x1f32\x1f\x1f",
        ];
        rows.join("\x1e").into_bytes()
    }

    #[test]
    fn reads_collections_keywords_and_stars() {
        let c = super::parse(&sample());
        assert_eq!(c.roots, [(1, "/Volumes/Photos/".to_owned())]);
        let now = [PathBuf::from("/media/disk")];
        let a = PathBuf::from("/media/disk/2024/Oslo/a.jpg");
        let b = PathBuf::from("/media/disk/2024/b.CR3");
        let mut colls = c.collections(&c.paths(&now));
        colls[0].1.sort();
        assert_eq!(colls, [("Travel - Norway".to_owned(), vec![a.clone(), b])]);
        let mut keywords = c.keyword_paths();
        keywords.sort();
        assert_eq!(keywords, ["Places", "Places|Oslo"]);
        assert_eq!(c.stars(&c.paths(&now)), [(a.clone(), 4)]);
        // Each image's keywords as tree paths; one of an image not in the catalog left out.
        let tags = |c: &super::Catalog| c.image_keywords(&c.paths(&now));
        let tag = |p: &PathBuf, t: &[&str]| (p.clone(), t.iter().map(|s| s.to_string()).collect());
        let b = PathBuf::from("/media/disk/2024/b.CR3");
        assert_eq!(tags(&c), [tag(&a, &["Places|Oslo"]), tag(&b, &["Places"])]);
        // A virtual copy with more stars: one value for the file, the most.
        let mut copy = sample();
        copy.extend(b"\x1ei\x1f12\x1f1\x1f2024/Oslo/a.jpg\x1f5");
        let c = super::parse(&copy);
        assert_eq!(c.stars(&c.paths(&now)), [(a.clone(), 5)]);
        // Its keyword too, once for the file.
        assert_eq!(tags(&c)[0], tag(&a, &["Places|Oslo"]));
        assert_eq!(
            super::root_now(" /media/disk "),
            PathBuf::from("/media/disk")
        );
        assert_eq!(super::root_now("Pictures"), PathBuf::new());
    }

    /// sqlite3 not on the host of a Flatpak, or not in PATH, is told from a catalog it cannot read.
    #[test]
    fn tells_sqlite3_missing() {
        let spawn = "Portal call failed: Failed to start command: Failed to execute child \
                     process \u{201c}sqlite3\u{201d} (No such file or directory)";
        assert!(super::missing(Some(255), spawn));
        assert!(super::missing(Some(127), ""));
        assert!(!super::missing(Some(1), "Error: file is not a database"));
    }

    /// sqlite3 output broken in thousands of ways: read without a panic or an endless loop.
    #[test]
    fn survives_broken_output() {
        let now = [PathBuf::from("/x")];
        for text in crate::broken(&sample()) {
            let c = super::parse(&text);
            let paths = c.paths(&now);
            c.collections(&paths);
            c.keyword_paths();
            c.stars(&paths);
            c.image_keywords(&paths);
        }
    }
}
