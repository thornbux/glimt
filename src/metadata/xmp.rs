//! Keywords and comment, as Geeqie's info sidebar edits them, which are dc:subject and
//! dc:description in XMP. It also handles IPTC's alt text and extended description, and the
//! keywords' tree paths (lr:hierarchicalSubject). They are written to the sidecar
//! `<file>.xmp`, which darktable and digiKam read too, and read from it, or else from the XMP
//! inside the image. `update_sidecar` writes the stars too, and `follow` takes sidecars along
//! when their files move.

use crate::files::config::fits_on_a_line;
use crate::support::format::without_control_characters;
use crate::support::threads::lock_even_if_poisoned;
use std::path::{Path, PathBuf};

const ALT: &str = "Iptc4xmpCore:AltTextAccessibility";

const EXTENDED: &str = "Iptc4xmpCore:ExtDescrAccessibility";

/// The keyword tree lists that digiKam and Windows Photo Gallery keep beside
/// lr:hierarchicalSubject, with "/" between the levels, each with its container and namespace.
/// digiKam reads them before dc:subject. Glimt reads their paths with Lightroom's
/// (`tree_paths_in`) and writes them again from the paths it writes (`with_slashed_tree_lists`),
/// so they never hold a keyword taken out or lack one added. Media Pro's catalog sets and
/// ACDSee's categories are not keywords, and stay as they are.
const SLASHED_TREE_LISTS: [(&str, &str, &str); 2] = [
    ("digiKam:TagsList", "Seq", "http://www.digikam.org/ns/1.0/"),
    (
        "MicrosoftPhoto:LastKeywordXMP",
        "Bag",
        "http://ns.microsoft.com/photo/1.0/",
    ),
];

/// The namespaces whose properties Copy for Sharing names by the prefix here, whatever prefix a
/// packet binds them to (`properties`). A packet may write Exif's GPS as `ns1:GPSLatitude`,
/// and the old `xap:` is `xmp:`, so a name by the prefix as written would miss its group.
const KNOWN_NAMESPACES: [(&str, &str); 31] = [
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("xmp", "http://ns.adobe.com/xap/1.0/"),
    ("xmpMM", "http://ns.adobe.com/xap/1.0/mm/"),
    ("xmpRights", "http://ns.adobe.com/xap/1.0/rights/"),
    ("xmpDM", "http://ns.adobe.com/xmp/1.0/DynamicMedia/"),
    ("exif", "http://ns.adobe.com/exif/1.0/"),
    ("aux", "http://ns.adobe.com/exif/1.0/aux/"),
    ("exifEX", "http://cipa.jp/exif/1.0/"),
    ("tiff", "http://ns.adobe.com/tiff/1.0/"),
    ("photoshop", "http://ns.adobe.com/photoshop/1.0/"),
    ("crs", "http://ns.adobe.com/camera-raw-settings/1.0/"),
    ("lr", "http://ns.adobe.com/lightroom/1.0/"),
    (
        "Iptc4xmpCore",
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
    ),
    ("Iptc4xmpExt", "http://iptc.org/std/Iptc4xmpExt/2008-02-29/"),
    ("plus", "http://ns.useplus.org/ldf/xmp/1.0/"),
    ("digiKam", "http://www.digikam.org/ns/1.0/"),
    // The namespace that digiKam 4 wrote its keyword list and labels in.
    ("digiKam", "http://www.digikam.org/"),
    ("MicrosoftPhoto", "http://ns.microsoft.com/photo/1.0/"),
    (
        "mwg-rs",
        "http://www.metadataworkinggroup.com/schemas/regions/",
    ),
    ("mediapro", "http://ns.iview-multimedia.com/mediapro/1.0/"),
    ("drone-dji", "http://www.dji.com/drone-dji/1.0/"),
    ("GPano", "http://ns.google.com/photos/1.0/panorama/"),
    // Google's camera: its motion photo's flags, also written with the prefix Camera.
    ("GCamera", "http://ns.google.com/photos/1.0/camera/"),
    ("MP", "http://ns.microsoft.com/photo/1.2/"),
    ("acdsee", "http://ns.acdsee.com/iptc/1.0/"),
    ("darktable", "http://darktable.sf.net/"),
    ("hdrgm", "http://ns.adobe.com/hdr-gain-map/1.0/"),
    // Apple's marks of an iPhone's HDR gain map.
    ("HDRGainMap", "http://ns.apple.com/HDRGainMap/1.0/"),
    ("apdi", "http://ns.apple.com/pixeldatainfo/1.0/"),
    ("Container", "http://ns.google.com/photos/1.0/container/"),
    ("Item", "http://ns.google.com/photos/1.0/container/item/"),
];

/// The 64 locks within this Glimt, by `sidecar_lock_number`.
#[cfg(not(test))]
static SIDECAR_LOCKS: [std::sync::Mutex<()>; 64] = [const { std::sync::Mutex::new(()) }; 64];

#[derive(Clone, Default, PartialEq, Debug)]
pub struct KeywordsAndComment {
    pub keywords: Vec<String>,
    pub comment: String,
}

impl KeywordsAndComment {
    /// The keywords as typed, separated by commas.
    pub fn keywords_text(&self) -> String {
        self.keywords.join(", ")
    }

    /// Keywords from text separated by commas, without empty or repeated ones.
    pub fn parse_keywords(text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for k in text.split(',').map(str::trim).filter(|k| !k.is_empty()) {
            if !out.iter().any(|o| o == k) {
                out.push(k.to_owned());
            }
        }
        out
    }

    /// Keywords and comment as one line, for searching.
    pub fn text(&self) -> String {
        format!("{} {}", self.keywords.join(" "), self.comment)
    }
}

/// An edit of the keywords field: the keywords it added and those it took out. `write_changed`
/// applies it to the keywords on disk, so keywords that another window or a background job set
/// meanwhile stay, and so do keywords the edit did not touch, also one that holds a comma.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct KeywordChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl KeywordChange {
    /// The change from the keywords `saved` to the keywords `typed`, which the field gives split
    /// at commas. A saved keyword that holds a comma ("Smith, John") stays when each of its parts
    /// is still typed. Otherwise it is taken out whole, and its parts still typed are added as
    /// keywords of their own. A keyword written in another letter case counts as taken out and
    /// added, so the new spelling is written.
    pub fn between(saved: &[String], typed: &[String]) -> Self {
        let (mut kept_parts, mut removed) = (Vec::new(), Vec::new());
        for keyword in saved {
            let parts = KeywordsAndComment::parse_keywords(keyword);
            match parts.iter().all(|p| typed.contains(p)) {
                true => kept_parts.extend(parts),
                false => removed.push(keyword.clone()),
            }
        }
        KeywordChange {
            added: (typed.iter())
                .filter(|k| !kept_parts.contains(k))
                .cloned()
                .collect(),
            removed,
        }
    }

    /// Takes the removed keywords out of `keywords` and adds the added ones at its end. A removed
    /// keyword is taken out only as it is spelled, because `between` chose it by its spelling,
    /// and "Rose" stays when "rose" goes.
    pub fn apply(&self, keywords: &mut Vec<String>) {
        keywords.retain(|k| !self.removed.contains(k));
        add_to(keywords, &self.added);
    }
}

/// IPTC's fields for accessibility (Photo Metadata 2021), in XMP. They are a short text for
/// someone who cannot see the picture, and a longer description of it. Web pages, publishing
/// tools and screen readers use them. They are kept apart from the keywords and comment, which
/// Geeqie edits.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct AltText {
    pub text: String,
    pub extended_description: String,
}

/// The namespaces one XMP packet declares, each as (prefix, URI), shared by its properties.
pub(crate) type Namespaces = std::sync::Arc<Vec<(String, String)>>;

/// The top-level properties of XMP packets, as Copy for Sharing reads them to write a new packet
/// of those the user keeps (`packet`). Each property is held by its name ("dc:subject"), its
/// element as written (`<dc:subject>…</dc:subject>`), and the namespaces of the packet it came
/// from, which bind its element's prefixes. The packets of the image and its sidecar may bind
/// one prefix to two namespaces, so each property keeps its own. The name has the prefix of
/// its namespace in `KNOWN_NAMESPACES`, which may differ from the element's.
#[derive(Clone, Default, Debug)]
pub(crate) struct Properties {
    pub(crate) list: Vec<(String, String, Namespaces)>,
}

impl Properties {
    /// Adds the properties of `other`, each in place of one of the same name.
    pub(crate) fn add_over(&mut self, other: Properties) {
        for property in other.list {
            self.list.retain(|(name, ..)| *name != property.0);
            self.list.push(property);
        }
    }

    /// Adds the properties of `other` whose names it lacks.
    pub(crate) fn add_missing(&mut self, other: Properties) {
        for property in other.list {
            if !self.list.iter().any(|(name, ..)| *name == property.0) {
                self.list.push(property);
            }
        }
    }
}

/// The fields a write of `write_keywords` changes. The others stay as the sidecar has them.
#[derive(Clone, Copy, PartialEq)]
enum Edited {
    Keywords,
    Comment,
    Both,
}

/// A sidecar's lock while it is held. It holds the lock file, if it could be made, and the
/// mutex in `SIDECAR_LOCKS` that its number picks.
pub(crate) type SidecarLock = (Option<std::fs::File>, std::sync::MutexGuard<'static, ()>);

/// The RAW files and Adobe sidecars of each folder that one operation looks at, by their name
/// stem (`adobe_stem`). A folder is read once for the whole operation, and only when a RAW file
/// in it is asked about, so a folder of JPEG files is never read. Before, every RAW extension
/// was looked up in two cases for each file, about 56 lookups a file on a network share.
#[derive(Default)]
pub(crate) struct AdobeNames {
    folders: std::collections::HashMap<PathBuf, std::collections::HashMap<String, StemNames>>,
    /// The RAW files that the operation brings into a folder, by folder and name stem.
    arriving: std::collections::HashMap<(PathBuf, String), Vec<PathBuf>>,
}

impl AdobeNames {
    /// Returns the names for an operation that brings files to the names `arriving` (an Undo, a
    /// move, a rename). A RAW file among them counts as another RAW file of its name stem for
    /// each of the others, because it will be there too.
    pub(crate) fn arriving(arriving: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut names = Self::default();
        for path in arriving.into_iter().filter(|p| is_adobe_raw(p)) {
            if let Some((dir, stem)) = adobe_stem(&path) {
                let key = (dir.to_owned(), stem);
                names.arriving.entry(key).or_default().push(path);
            }
        }
        names
    }

    /// Returns the RAW files and Adobe sidecars in the folder of `path` that have its name stem.
    /// Returns `None` when there are none, also when the folder cannot be read.
    fn of_stem(&mut self, path: &Path) -> Option<&StemNames> {
        let (dir, stem) = adobe_stem(path)?;
        let folder = (self.folders.entry(dir.to_owned())).or_insert_with(|| list_adobe_names(dir));
        folder.get(&stem)
    }
}

/// The RAW files and Adobe sidecars of one name stem in a folder, by their names there.
#[derive(Default)]
struct StemNames {
    raw_files: Vec<std::ffi::OsString>,
    sidecars: Vec<std::ffi::OsString>,
}

/// A sidecar that could not follow its file (`follow`).
#[derive(Debug)]
pub struct NotFollowed {
    /// The sidecar's new name, where it could not be put.
    pub at: PathBuf,
    pub error: std::io::Error,
    /// Whether every sidecar that had followed was taken back, so that the file can go back
    /// to them. When one could not be taken back, the file stays with it.
    pub taken_back: bool,
}

fn parse_alt_text(xmp: &str) -> AltText {
    AltText {
        text: language_text(xmp, ALT),
        extended_description: language_text(xmp, EXTENDED),
    }
}

/// `read` and `tree_paths` of `path`, and its alt text and extended description (from its
/// sidecar if that has them, and otherwise from the image), for the panel. Its sidecar and the
/// XMP inside it are each read once, because on a network share each reading of a large photo
/// costs.
pub fn read_all(path: &Path) -> (KeywordsAndComment, Vec<String>, AltText) {
    let side = own_sidecar_text(path);
    let inside = std::cell::OnceCell::new();
    let inside = || inside.get_or_init(|| embedded(path)).clone();
    let has_meta = |x: &String| x.contains("<dc:subject") || x.contains("<dc:description");
    let has_alt_text =
        |x: &String| x.contains(&format!("<{ALT}")) || x.contains(&format!("<{EXTENDED}"));
    let source = side.clone().filter(has_meta).or_else(inside);
    let alt_text = side.filter(has_alt_text).or_else(inside);
    (
        source
            .as_deref()
            .map_or_else(KeywordsAndComment::default, parse),
        source.map_or_else(Vec::new, |x| tree_paths_in(&x)),
        alt_text.map_or_else(AltText::default, |x| parse_alt_text(&x)),
    )
}

/// Writes the alt text and extended description of `path` to its sidecar, and everything else
/// in it stays. Fields the image holds itself and that were cleared are written empty, as the
/// comment is.
pub fn write_alt_text(path: &Path, alt_text: &AltText) -> Result<(), String> {
    let _lock = lock_sidecar(path);
    let own = embedded(path).is_some_and(|x| parse_alt_text(&x) != AltText::default());
    update(path, *alt_text == AltText::default() && !own, |x| {
        set_alt_text(x, alt_text, own)
    })
}

/// `xmp` with the alt text and extended description of `alt_text`, each in place of the
/// language shown, and their other languages kept (`language_alternative`). Returns `None` if
/// `xmp` has no rdf:Description.
fn set_alt_text(xmp: &str, alt_text: &AltText, say_empty: bool) -> Option<String> {
    let add = language_alternative(xmp, ALT, &alt_text.text, say_empty)
        + &language_alternative(xmp, EXTENDED, &alt_text.extended_description, say_empty);
    let xmp = without_property(&without_property(xmp, ALT), EXTENDED);
    insert(
        xmp,
        &add,
        "Iptc4xmpCore",
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
    )
}

/// Whether two keywords are the same one. Letter case does not count ("Oslo" and "oslo").
pub fn same_keyword(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The keywords without repeats, each as first written.
fn unique(keywords: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in keywords {
        if !out.iter().any(|o| same_keyword(o, &k)) {
            out.push(k);
        }
    }
    out
}

/// The keyword of a keyword tree path, which is its last level.
fn keyword_of_path(tag: &str) -> &str {
    tag.rsplit('|').next().unwrap_or(tag)
}

/// Whether an image with `keywords` and keyword tree `paths` has `tag`. It must have the
/// keyword, and the path too unless no path ends in that keyword (as when a program that
/// writes keywords only set it).
pub fn has_tag(keywords: &[String], paths: &[String], tag: &str) -> bool {
    let name = keyword_of_path(tag);
    keywords.iter().any(|k| same_keyword(k, name))
        && (paths.iter().any(|p| same_keyword(p, tag))
            || !paths.iter().any(|p| same_keyword(keyword_of_path(p), name)))
}

/// Set keyword tree path `tag` on or off in `keywords` and `paths`. Its keyword is added once,
/// and taken off only when no other path ends in it ("Events|Oslo" off keeps Oslo for
/// "Places|Norway|Oslo").
pub fn set_tag(keywords: &mut Vec<String>, paths: &mut Vec<String>, tag: &str, on: bool) {
    let name = keyword_of_path(tag);
    paths.retain(|p| !same_keyword(p, tag));
    if on {
        paths.push(tag.to_owned());
        if !keywords.iter().any(|k| same_keyword(k, name)) {
            keywords.push(name.to_owned());
        }
    } else if !paths.iter().any(|p| same_keyword(keyword_of_path(p), name)) {
        keywords.retain(|k| !same_keyword(k, name));
    }
}

/// The keyword tree paths (lr:hierarchicalSubject) of `path`, from where `read` takes the
/// keywords: the sidecar, or else the XMP inside the image. The writers read them with the
/// keywords (`read_with_tree_paths`), and the panel with everything else (`read_all`).
#[cfg(test)]
fn tree_paths(path: &Path) -> Vec<String> {
    read_with_tree_paths(path).1
}

/// `read` and `tree_paths` of `path`, from one reading of its sidecar or image.
fn read_with_tree_paths(path: &Path) -> (KeywordsAndComment, Vec<String>) {
    source(path).map_or_else(Default::default, |x| (parse(&x), tree_paths_in(&x)))
}

/// The keyword tree paths that the XMP packet `xmp` holds: those of lr:hierarchicalSubject and
/// of the lists with "/" between the levels (`SLASHED_TREE_LISTS`), together. A program may have
/// written one list and not the others, so a path that only one holds is kept, and the writers
/// write it into each of them.
fn tree_paths_in(xmp: &str) -> Vec<String> {
    let piped = list_items(xmp, "lr:hierarchicalSubject");
    // A slashed entry that is a path of lr:hierarchicalSubject with "/" for "|" is that path.
    // So a level that holds a "/" itself, as in Music|AC/DC, is not split into two.
    let slashed: Vec<String> = (SLASHED_TREE_LISTS.iter())
        .flat_map(|(list, ..)| list_items(xmp, list))
        .filter(|entry| !piped.iter().any(|p| p.replace('|', "/") == *entry))
        .map(|p| p.replace('/', "|"))
        .collect();
    unique(piped.into_iter().chain(slashed))
}

/// The XMP the keywords and comment of `path` come from. It is the sidecar if that has them,
/// and otherwise the image's own.
fn source(path: &Path) -> Option<String> {
    let has = |x: &String| x.contains("<dc:subject") || x.contains("<dc:description");
    own_sidecar_text(path)
        .filter(has)
        .or_else(|| embedded(path))
}

/// Reads the sidecar of `path` as text for the panel and the search. Returns `None` when there
/// is none, when it cannot be read or is not UTF-8, and when it is a link (`own_sidecar`).
fn own_sidecar_text(path: &Path) -> Option<String> {
    String::from_utf8(own_sidecar(path).ok()??).ok()
}

/// Reads the sidecar of `path`, the one way a sidecar is read. Returns `None` when there is
/// none or when it is a link, and an error when one is there that cannot be read (no
/// permission, a folder), which a writer must not write over. A sidecar that is a link is not
/// the photo's own, because a downloaded folder could point it at another file of the user's,
/// whose contents would then show for this photo or be copied with it. The link is refused by
/// the open itself, so one swapped in after a look at the name is not read through either.
pub(crate) fn own_sidecar(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match crate::images::decode::read_unlinked(&sidecar(path)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `<file>.xmp`, next to the file.
pub fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".xmp");
    path.with_file_name(name)
}

/// The keywords and comment of `path`, from its sidecar if that has them, and otherwise from
/// the image.
pub fn read(path: &Path) -> KeywordsAndComment {
    source(path).map_or_else(KeywordsAndComment::default, |x| parse(&x))
}

/// The XMP packet inside the image. A JPEG's is read from its first 256 KB, as its EXIF is.
fn embedded(path: &Path) -> Option<String> {
    const JPEG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    let bytes = match crate::metadata::exif::jpeg_app1(path, JPEG) {
        Some(found) => found?[JPEG.len()..].to_vec(),
        None => {
            use image::ImageDecoder;
            let reader = crate::images::decode::image_reader(path).ok()?;
            reader.into_decoder().ok()?.xmp_metadata().ok()??
        }
    };
    String::from_utf8(bytes).ok()
}

/// Writes the fields that were edited (`None` means as it is on disk). The keywords' change is
/// applied to the keywords on disk, read and written as one change under the sidecar's lock. So
/// keywords that another window or a background job set since the panel read the file stay,
/// and everything else in the sidecar stays too. Keywords or a comment the image holds itself
/// and that were cleared are written as empty elements, because if they were left out, the
/// image's own would be read again. Returns the keywords and comment as written.
pub fn write_changed(
    path: &Path,
    keywords: Option<KeywordChange>,
    comment: Option<String>,
) -> Result<KeywordsAndComment, String> {
    let _lock = lock_sidecar(path);
    let (mut meta, paths) = read_with_tree_paths(path);
    let edited = match (&keywords, &comment) {
        (_, None) => Edited::Keywords,
        (None, Some(_)) => Edited::Comment,
        (Some(_), Some(_)) => Edited::Both,
    };
    if let Some(change) = keywords {
        change.apply(&mut meta.keywords);
    }
    if let Some(c) = comment {
        meta.comment = c;
    }
    write_keywords(path, &meta, &paths, edited)?;
    Ok(meta)
}

/// Adds each of `keywords` that `have` lacks to it, at its end.
pub fn add_to(have: &mut Vec<String>, keywords: &[String]) {
    for k in keywords {
        if !have.iter().any(|m| same_keyword(m, k)) {
            have.push(k.clone());
        }
    }
}

/// Add `keywords` to those `path` has, read and written as one change.
pub fn add_keywords(path: &Path, keywords: &[String]) -> Result<(), String> {
    let _lock = lock_sidecar(path);
    let (mut meta, paths) = read_with_tree_paths(path);
    add_to(&mut meta.keywords, keywords);
    write_keywords(path, &meta, &paths, Edited::Keywords)
}

/// Writes the fields of `meta` that were `edited`, and the keyword tree `paths`, to the sidecar
/// of `path`, with its lock held by the caller. It is the one way keywords are written. An edit
/// of the comment alone leaves every keyword list as the sidecar has it. Every writer starts
/// from the paths of `read_with_tree_paths`, which come from where the keywords come from, so a
/// hierarchy that only the image holds is not flattened when its first sidecar is made. Keywords
/// or a comment that the image holds itself and that were cleared are written as empty elements,
/// because if they were left out, the image's own would be read again.
fn write_keywords(
    path: &Path,
    meta: &KeywordsAndComment,
    paths: &[String],
    edited: Edited,
) -> Result<(), String> {
    let inside = embedded(path);
    let own = inside
        .as_deref()
        .is_some_and(|x| parse(x) != KeywordsAndComment::default());
    // The image's own lists are countered in the sidecar, as its dc:subject is.
    let lists_in_image: Vec<&str> = (SLASHED_TREE_LISTS.iter())
        .map(|(list, ..)| *list)
        .filter(|list| {
            inside
                .as_deref()
                .is_some_and(|x| x.contains(&format!("<{list}")))
        })
        .collect();
    let only_if_there = *meta == KeywordsAndComment::default() && !own;
    update(path, only_if_there, |x| {
        // A sidecar without dc:subject takes the keywords that were read, so that a new one
        // does not hide the image's.
        if edited == Edited::Comment && x.contains("<dc:subject") {
            return set_comment(x, &meta.comment, own);
        }
        let x = with_paths(x.to_owned(), paths)?;
        let x = match edited {
            Edited::Keywords => set_keywords(&x, meta, own)?,
            Edited::Comment | Edited::Both => set_keywords_and_comment(&x, meta, own)?,
        };
        with_slashed_tree_lists(x, &lists_in_image)
    })
}

/// Holds the lock of the sidecar of `path` while it is read and written again, by this
/// window's threads and by every other Glimt window. So keywords added in the background, stars
/// saved and a comment edited in another window meanwhile each keep what the others wrote.
///
/// It is one of 64 locks, chosen by the sidecar's path. Within this Glimt it is a mutex, and
/// across windows a lock file in Glimt's runtime folder, not one beside the sidecar in the
/// user's folder. The mutex is always taken, so two threads of this Glimt wait for each other
/// also when the lock file cannot be made for one of them (a full runtime folder) and can for
/// the other. There are 64 locks, so a sidecar on a share that stopped answering holds up only
/// the sidecars of its own lock, not every write of the window. A move holds it too, so a write
/// that waited finds the photo gone, not its old name free for a sidecar.
pub(crate) fn lock_sidecar(path: &Path) -> SidecarLock {
    lock_sidecar_in(lock_dir().as_deref(), path)
}

fn lock_sidecar_in(dir: Option<&Path>, path: &Path) -> SidecarLock {
    let side = sidecar(path);
    let guard = lock_even_if_poisoned(lock_of(sidecar_lock_key(&side)));
    (dir.and_then(|d| lock_file(d, &side)), guard)
}

/// `lock_sidecar` for many files at once, as Rename Many and a delete move a photo and then its
/// sidecar. Each lock is taken once, because taking one twice would wait for itself. They are
/// taken in the order of their numbers, which every Glimt and every thread takes them in, so
/// two of these never wait for each other.
pub(crate) fn lock_sidecars<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Vec<SidecarLock> {
    lock_sidecars_in(lock_dir().as_deref(), paths)
}

fn lock_sidecars_in<'a>(
    dir: Option<&Path>,
    paths: impl IntoIterator<Item = &'a PathBuf>,
) -> Vec<SidecarLock> {
    let mut sides: Vec<(u64, PathBuf)> = (paths.into_iter())
        .map(|p| sidecar(p))
        .map(|s| (sidecar_lock_key(&s), s))
        .collect();
    sides.sort_by_key(|s| s.0);
    sides.dedup_by_key(|s| s.0);
    (sides.iter())
        .map(|(n, side)| {
            let guard = lock_even_if_poisoned(lock_of(*n));
            (dir.and_then(|d| lock_file(d, side)), guard)
        })
        .collect()
}

/// The folder of the lock files, where there is a runtime folder. Tests take only the mutexes,
/// because the runtime folder's lock files are shared with a Glimt that runs there, and two
/// tests' sidecars of one number would wait for each other at their lock file. The tests of the
/// lock files name a folder of their own.
fn lock_dir() -> Option<PathBuf> {
    #[cfg(test)]
    return None;
    #[cfg(not(test))]
    return crate::features::remote::runtime_dir().map(|d| d.join("sidecar-locks"));
}

fn lock_file(dir: &Path, side: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir);
    crate::files::file_io::open_and_lock(&dir.join(sidecar_lock_number(side).to_string())).ok()
}

/// Which of the 64 lock files a sidecar has. It is FNV-1a of its path's bytes, the same in
/// every Glimt, also one built with another Rust, whose own hasher may differ.
pub(crate) fn sidecar_lock_number(side: &Path) -> u64 {
    fnv(side) % 64
}

/// Returns FNV-1a of the bytes of the path `side`.
fn fnv(side: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    (side.as_os_str().as_bytes().iter()).fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// Returns the number of the mutex that the sidecar `side` takes: `sidecar_lock_number`. In
/// tests every sidecar has a mutex of its own, because the tests run side by side in one
/// process. A test that held a lock on purpose for seconds held up another test's sidecar of
/// the same number, and that test timed out.
fn sidecar_lock_key(side: &Path) -> u64 {
    #[cfg(test)]
    return fnv(side);
    #[cfg(not(test))]
    return sidecar_lock_number(side);
}

/// Returns the mutex of the lock number `key` (`sidecar_lock_key`).
fn lock_of(key: u64) -> &'static std::sync::Mutex<()> {
    #[cfg(not(test))]
    return &SIDECAR_LOCKS[key as usize];
    // Each lock is made when it is first taken, and kept for the rest of the tests.
    #[cfg(test)]
    {
        static LOCKS: std::sync::Mutex<
            std::collections::BTreeMap<u64, &'static std::sync::Mutex<()>>,
        > = std::sync::Mutex::new(std::collections::BTreeMap::new());
        let mut made = lock_even_if_poisoned(&LOCKS);
        made.entry(key).or_insert_with(|| Box::leak(Box::default()))
    }
}

/// Changes the sidecar of `path` with `change`, or a new, empty sidecar unless `only_if_there`,
/// in which case a file without a sidecar keeps having none. It is written to a temporary file
/// first, so a crash never leaves half a sidecar. The stars are written with it; a turn holds the
/// lock on after the change, so it takes the lock itself (`update_locked_sidecar`).
#[cfg(any(feature = "marks", test))]
pub fn update_sidecar(
    path: &Path,
    only_if_there: bool,
    change: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    let _lock = lock_sidecar(path);
    update(path, only_if_there, change)
}

/// Changes the sidecar of `path` as `update_sidecar` does, with its lock already taken by the
/// caller, who holds it on after the change (`save::replace_with_sidecar`).
pub(crate) fn update_locked_sidecar(
    _lock: &SidecarLock,
    path: &Path,
    only_if_there: bool,
    change: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    update(path, only_if_there, change)
}

fn update(
    path: &Path,
    only_if_there: bool,
    change: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    // When the image was moved, renamed or deleted while this waited, no sidecar of its old
    // name is made, and it does not count as done.
    if !crate::files::file_io::is_surely_there(path) {
        return Err(
            crate::support::lang::tr("the image was moved, renamed or deleted meanwhile").into(),
        );
    }
    let side = sidecar(path);
    // A link is not written through, because a downloaded folder may point IMG.jpg.xmp at
    // another file of the user's, which a keyword added here would then change.
    if is_link(&side) {
        return Err(crate::tf!(
            "{name} is a link: not written through",
            name = side.display()
        ));
    }
    // A sidecar that is there but cannot be read (no permission, not UTF-8) is not written
    // over, because it may hold another program's edits.
    let old = match own_sidecar(path) {
        Err(e) => return Err(format!("{}: {e}", side.display())),
        Ok(Some(bytes)) => {
            String::from_utf8(bytes).map_err(|e| format!("{}: {e}", side.display()))?
        }
        Ok(None) if only_if_there => return Ok(()),
        Ok(None) => "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
             <rdf:Description rdf:about=\"\"/>\n </rdf:RDF>\n</x:xmpmeta>\n\
             <?xpacket end=\"w\"?>\n"
            .to_owned(),
    };
    let text = change(&old).ok_or("unreadable XMP sidecar")?;
    // A link put there since the look above is replaced, and the file it points to is left
    // alone.
    let named = |e: std::io::Error| format!("{}: {e}", side.display());
    crate::files::file_io::replace_name(&side, text).map_err(named)
}

/// Returns the sidecar of `path` when there is one, as a name in the folder. A link counts, also
/// one that points nowhere, so that it goes with its file rather than stay behind under the old
/// name for the next file of that name. So does a sidecar that could not be looked at, so that
/// its move is tried and a failure told. Whether it may be read or copied is for `is_link` to say.
pub(crate) fn sidecar_of(path: &Path) -> Option<PathBuf> {
    let side = sidecar(path);
    (!crate::files::file_io::is_surely_gone(&side)).then_some(side)
}

/// Whether `path` is a camera RAW file that Lightroom, Camera Raw, Bridge and Capture One write
/// a sidecar beside: `IMG_1.xmp` for `IMG_1.CR2`. A DNG, like a JPEG, has Adobe's metadata
/// inside it, and so is no such file. Glimt never reads or writes Adobe's sidecar. It only
/// keeps it with its RAW file.
fn is_adobe_raw(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    ext.is_some_and(|ext| ext != "dng" && crate::images::raw::EXTS.contains(&ext.as_str()))
}

/// Whether `path` has the name of an Adobe sidecar: `NAME.xmp`, with the extension in any case,
/// because Capture One and Photo Mechanic write `NAME.XMP`.
fn is_adobe_sidecar_name(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

/// Returns the folder and the name stem, in lowercase, that tie a RAW file and its Adobe
/// sidecar together: `IMG_1` for both `IMG_1.CR2` and `IMG_1.XMP`. The case is left out, as
/// Lightroom's own disks ignore it. Returns `None` for a path that is neither.
pub(crate) fn adobe_stem(path: &Path) -> Option<(&Path, String)> {
    let is_either = is_adobe_raw(path) || is_adobe_sidecar_name(path);
    let stem = path.file_stem().filter(|_| is_either)?;
    Some((path.parent()?, stem.to_string_lossy().to_lowercase()))
}

/// Returns the RAW files and Adobe sidecars in the folder `dir`, by name stem. A folder that
/// cannot be read has none.
fn list_adobe_names(dir: &Path) -> std::collections::HashMap<String, StemNames> {
    let mut stems: std::collections::HashMap<String, StemNames> = Default::default();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some((_, stem)) = adobe_stem(Path::new(&name)) else {
            continue;
        };
        let names = stems.entry(stem).or_default();
        match is_adobe_sidecar_name(Path::new(&name)) {
            true => names.sidecars.push(name),
            false => names.raw_files.push(name),
        }
    }
    stems
}

/// Returns the Adobe sidecars that belong to the RAW file `path`, whether or not it is there
/// yet. This is the one rule of whose such a sidecar is. It is the file's when no other RAW file
/// of its name stem is in its folder. Another one (an `IMG_1.NEF` beside `IMG_1.CR2`, also as
/// `IMG_1.Nef`) could own it as well, and then it belongs to neither and stays where it is. A
/// RAW file whose path is in `ours` is leaving with the operation, and so does not count, while
/// one that `names` has arriving does. A JPEG beside the RAW owns none, because Lightroom writes
/// a JPEG's metadata into the JPEG.
pub(crate) fn adobe_sidecars_of(
    path: &Path,
    ours: &std::collections::HashSet<&PathBuf>,
    names: &mut AdobeNames,
) -> Vec<PathBuf> {
    let Some((dir, stem)) = adobe_stem(path).filter(|_| is_adobe_raw(path)) else {
        return Vec::new();
    };
    let another_arriving = (names.arriving.get(&(dir.to_owned(), stem)))
        .is_some_and(|arriving| arriving.iter().any(|p| p != path));
    let Some(stem) = names.of_stem(path).filter(|s| !s.sidecars.is_empty()) else {
        return Vec::new();
    };
    let own_name = path.file_name();
    let mut others = (stem.raw_files.iter())
        .filter(|n| Some(n.as_os_str()) != own_name)
        .map(|n| dir.join(n))
        .filter(|p| !ours.contains(p));
    let another_raw = match stem
        .raw_files
        .iter()
        .any(|n| Some(n.as_os_str()) == own_name)
    {
        true => others.next().is_some(),
        // The file is gone (a move or delete), or named in another case on a camera card,
        // where IMG_1.cr2 names IMG_1.CR2 too. The other names are then told by who they are.
        false => {
            let identity = |p: &Path| {
                let meta = std::fs::symlink_metadata(p).ok()?;
                Some(crate::images::save::file_id_from_metadata(p, &meta))
            };
            let own = identity(path);
            others.any(|other| identity(&other).is_some_and(|i| Some(i) != own))
        }
    };
    if another_raw || another_arriving {
        return Vec::new();
    }
    // The folder was read once for the whole operation, so each sidecar is looked at again.
    (stem.sidecars.iter())
        .map(|n| dir.join(n))
        .filter(|p| !crate::files::file_io::is_surely_gone(p))
        .collect()
}

/// Returns each sidecar of `path` that is there, Glimt's own and Adobe's, paired with its name
/// for the file at `to`. Every operation that moves, renames, copies or deletes a file asks
/// this, so a sidecar never stays behind for the next file of the old name. `names` holds the
/// folders that the operation has read already, and `ours` the RAW files leaving with it
/// (`adobe_sidecars_of`).
pub(crate) fn sidecars_moving_with(
    path: &Path,
    to: &Path,
    ours: &std::collections::HashSet<&PathBuf>,
    names: &mut AdobeNames,
) -> Vec<(PathBuf, PathBuf)> {
    let own = sidecar_of(path).map(|side| (side, sidecar(to)));
    // Adobe's sidecar keeps the case of its extension: IMG_1.XMP goes to IMG_2.XMP.
    let adobe = adobe_sidecars_of(path, ours, names).into_iter();
    let adobe = adobe.map(|side| {
        let to = to.with_extension(side.extension().unwrap_or_default());
        (side, to)
    });
    own.into_iter().chain(adobe).collect()
}

/// Returns each sidecar of `paths` that is there, Glimt's own and Adobe's, for a delete. Each
/// is named once, also when two RAW files of one name stem, both gone, each found Adobe's theirs.
pub(crate) fn sidecars_of(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut names = AdobeNames::default();
    let sides: std::collections::BTreeSet<PathBuf> = (paths.iter())
        .flat_map(|path| sidecars_moving_with(path, path, &Default::default(), &mut names))
        .map(|(side, _)| side)
        .collect();
    sides.into_iter().collect()
}

/// Whether `path` is a link. A sidecar that is one is never read or written through, because a
/// downloaded folder may point it at any file of the user's (a private key).
pub(crate) fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Copies or moves the sidecars of `from` (`sidecars_moving_with`) with the file that went to
/// `to`, all or none. When one cannot follow, those that followed are taken back, so the file's
/// keywords are never split between two places. Returns the first sidecar that could not follow,
/// by its new name, with the error and whether every sidecar is back where it was.
/// `names` holds the folders that the operation has read already. `moving` are all the files
/// that a move takes, so that two RAW files of one name stem moved together take Adobe's
/// sidecar with the first of them, whatever their order. A copy passes none, because both
/// files stay, and the sidecar then stays theirs alone.
pub fn follow(
    from: &Path,
    to: &Path,
    is_copy: bool,
    moving: &std::collections::HashSet<&PathBuf>,
    names: &mut AdobeNames,
) -> Result<(), NotFollowed> {
    // Each sidecar is found here, at the move, not picked before it, so it is the photo's
    // sidecar by what it is now. Each step returns whether the sidecar was copied, or `None`
    // when nothing was put at its new name.
    let follow_one = |side: &Path, to: &Path| -> std::io::Result<Option<bool>> {
        // A link is no sidecar to copy. Moved, it goes as itself on the same disk, or it stays.
        if is_link(side) {
            return match is_copy {
                true => Ok(None),
                false => Ok(crate::files::file_io::rename_never_over(side, to)
                    .ok()
                    .map(|()| false)),
            };
        }
        // The sidecar goes as the file went. It is on disk before the original goes, and never
        // over a sidecar there, which belongs to another file or is a copy kept on purpose. A
        // sidecar that cannot be moved (someone else's in a shared folder) is copied, so the
        // keywords go with the photo.
        match crate::files::file_io::put_file(side, to, is_copy) {
            Err(_) if !is_copy => {
                crate::files::file_io::put_file(side, to, true).map(|()| Some(true))
            }
            done => done.map(|()| Some(is_copy)),
        }
    };
    let mut followed = Vec::new();
    for (side, to) in sidecars_moving_with(from, to, moving, names) {
        match follow_one(&side, &to) {
            Ok(Some(was_copy)) => followed.push((side, to, was_copy)),
            Ok(None) => {}
            Err(error) => {
                let taken_back = (followed.into_iter().rev()).all(|(side, to, was_copy)| {
                    crate::files::file_io::put_file_back(&side, &to, was_copy).is_ok()
                });
                return Err(NotFollowed {
                    at: to,
                    error,
                    taken_back,
                });
            }
        }
    }
    Ok(())
}

fn parse(xmp: &str) -> KeywordsAndComment {
    KeywordsAndComment {
        keywords: list_items(xmp, "dc:subject"),
        comment: language_text(xmp, "dc:description"),
    }
}

/// The texts of the rdf:li items of element `tag`.
fn list_items(xmp: &str, tag: &str) -> Vec<String> {
    (items_with_languages(xmp, tag).into_iter())
        .map(|(_, text)| text)
        .collect()
}

/// Returns which of the items of a language alternative (an rdf:Alt) the panel shows and an
/// edit replaces: the x-default one, or else the first. Returns `None` when there are none.
fn shown_language(items: &[(String, String)]) -> Option<usize> {
    let default = items
        .iter()
        .position(|(language, _)| language == "x-default");
    default.or((!items.is_empty()).then_some(0))
}

/// The text of the language alternative `tag` in `xmp` that the panel shows (`shown_language`).
fn language_text(xmp: &str, tag: &str) -> String {
    let mut items = items_with_languages(xmp, tag);
    shown_language(&items).map_or_else(String::new, |i| items.swap_remove(i).1)
}

/// The element `tag` as a language alternative, to be put in place of the one in `xmp`, with
/// `text` in place of the item shown (`shown_language`), or as x-default when there is none.
/// The other languages stay as they were. An empty `text` clears every language, as the field
/// shows no other. Returns an empty text for no element, or an empty rdf:Alt when `say_empty`.
fn language_alternative(xmp: &str, tag: &str, text: &str, say_empty: bool) -> String {
    let mut items = items_with_languages(xmp, tag);
    match shown_language(&items) {
        _ if text.is_empty() => items.clear(),
        Some(i) => items[i].1 = text.to_owned(),
        None => items.push(("x-default".into(), text.to_owned())),
    }
    if items.is_empty() {
        return match say_empty {
            true => format!("\n   <{tag}>\n    <rdf:Alt/>\n   </{tag}>"),
            false => String::new(),
        };
    }
    let mut out = format!("\n   <{tag}>\n    <rdf:Alt>\n");
    for (language, text) in items {
        let language = if language.is_empty() {
            "x-default".into()
        } else {
            language
        };
        out += &format!(
            "     <rdf:li xml:lang=\"{}\">{}</rdf:li>\n",
            escape(&language),
            escape(&text)
        );
    }
    out + &format!("    </rdf:Alt>\n   </{tag}>")
}

/// The rdf:li items of element `tag`, each as the language its start tag names (xml:lang, or
/// empty for none) and its text.
fn items_with_languages(xmp: &str, tag: &str) -> Vec<(String, String)> {
    let Some((start, end)) = element_span(xmp, tag) else {
        return Vec::new();
    };
    let (mut rest, mut out) = (&xmp[start..end], Vec::new());
    while let Some(i) = rest.find("<rdf:li") {
        rest = &rest[i..];
        let Some(gt) = rest.find('>') else { break };
        let body = &rest[gt + 1..];
        if rest[..gt].ends_with('/') {
            rest = body;
            continue;
        }
        let Some(close) = body.find("</rdf:li>") else {
            break;
        };
        let language = (attributes(&rest["<rdf:li".len()..gt]).into_iter())
            .find(|(name, _)| *name == "xml:lang")
            .map_or_else(String::new, |(_, value)| unescape(value));
        out.push((language, unescape(&body[..close])));
        rest = &body[close..];
    }
    out
}

/// `set_keywords_and_comment` for a change of keywords only. A comment the sidecar has stays as
/// it is, in all its languages. A comment only inside the image is copied into a new sidecar,
/// which would hide it otherwise.
fn set_keywords(xmp: &str, meta: &KeywordsAndComment, say_empty: bool) -> Option<String> {
    let Some(original) = element(xmp, "dc:description") else {
        return set_keywords_and_comment(xmp, meta, say_empty);
    };
    let keep = KeywordsAndComment {
        keywords: meta.keywords.clone(),
        comment: language_text(xmp, "dc:description"),
    };
    let new = set_keywords_and_comment(xmp, &keep, say_empty)?;
    // The description element as it was, every language of it.
    Some(match element(&new, "dc:description") {
        Some(made) => new.replacen(&made, &original, 1),
        None => insert(
            new,
            &format!("\n   {original}"),
            "dc",
            "http://purl.org/dc/elements/1.1/",
        )?,
    })
}

/// The whole element `tag` in `xmp`, from its start tag to its end tag, or the empty element
/// (`<tag/>`) alone.
fn element(xmp: &str, tag: &str) -> Option<String> {
    let (start, end) = element_span(xmp, tag)?;
    Some(xmp[start..end].to_owned())
}

/// Returns where the first element `tag` of `xmp` starts and where it ends, as
/// `save::xmp_without_property` ends it (`save::element_end_after_start_tag`). Returns `None`
/// when there is none, or when its end tag is missing.
fn element_span(xmp: &str, tag: &str) -> Option<(usize, usize)> {
    let start = xmp.find(&format!("<{tag}"))?;
    let gt = start + xmp[start..].find('>')?;
    let close = format!("</{tag}>");
    let find_close = |from: usize| xmp[from..].find(&close).map(|e| from + e);
    let end = crate::images::save::element_end_after_start_tag(
        xmp.as_bytes(),
        gt,
        close.as_bytes(),
        find_close,
    )?;
    Some((start, end))
}

/// `xmp` with the keywords and comment of `meta`, in its first rdf:Description. The comment
/// goes in place of the language shown, and its other languages stay (`language_alternative`).
/// Empty ones are written as empty elements when `say_empty` is set. Returns `None` if `xmp`
/// has no rdf:Description.
fn set_keywords_and_comment(
    xmp: &str,
    meta: &KeywordsAndComment,
    say_empty: bool,
) -> Option<String> {
    let description = language_alternative(xmp, "dc:description", &meta.comment, say_empty);
    let xmp = without_property(&without_property(xmp, "dc:subject"), "dc:description");
    let mut add = String::new();
    if meta.keywords.is_empty() && say_empty {
        add += "\n   <dc:subject>\n    <rdf:Bag/>\n   </dc:subject>";
    }
    if !meta.keywords.is_empty() {
        add += "\n   <dc:subject>\n    <rdf:Bag>\n";
        for k in &meta.keywords {
            add += &format!("     <rdf:li>{}</rdf:li>\n", escape(k));
        }
        add += "    </rdf:Bag>\n   </dc:subject>";
    }
    add += &description;
    let xmp = insert(xmp, &add, "dc", "http://purl.org/dc/elements/1.1/")?;
    let paths = tree_paths_for(&meta.keywords, list_items(&xmp, "lr:hierarchicalSubject"));
    with_paths(xmp, &paths)
}

/// The keyword tree paths for `keywords`, as Lightroom writes them. They are those of `paths`
/// that end in one of them, and each keyword without one as a path of its own (Immich, which
/// shows tags, reads only these). A keyword's own path goes when a longer one ends in it.
fn tree_paths_for(keywords: &[String], paths: Vec<String>) -> Vec<String> {
    let is_keyword = |p: &str| keywords.iter().any(|k| same_keyword(k, keyword_of_path(p)));
    // A path whose keyword was taken out is cut back to its longest start that ends in a keyword
    // still there. So Places|Norway stays when Oslo goes from Places|Norway|Oslo, and Norway is
    // not written as a keyword at the top of the tree.
    let cut_back = |p: String| -> Option<String> {
        let mut levels: Vec<&str> = p.split('|').collect();
        while !levels.is_empty() && !is_keyword(&levels.join("|")) {
            levels.pop();
        }
        (!levels.is_empty()).then(|| levels.join("|"))
    };
    let kept: Vec<String> = unique(paths.into_iter().filter_map(cut_back));
    // Lightroom writes the levels above a keyword as keywords too, Places, Norway and Oslo for
    // Places|Norway|Oslo. They are in that path, not paths of their own.
    let in_path = |p: &str, name: &str| p.split('|').any(|level| same_keyword(level, name));
    let longer = |name: &str| kept.iter().any(|q| q.contains('|') && in_path(q, name));
    let mut out: Vec<String> = (kept.iter())
        .filter(|p| p.contains('|') || !longer(p))
        .cloned()
        .collect();
    for k in keywords {
        if !out.iter().any(|p| in_path(p, k)) {
            out.push(k.clone());
        }
    }
    out
}

/// `xmp` with `comment` in place of its comment's language shown, and its other languages kept
/// (`language_alternative`). An empty one is written as an empty element when `say_empty` is
/// set. Returns `None` if `xmp` has no rdf:Description.
fn set_comment(xmp: &str, comment: &str, say_empty: bool) -> Option<String> {
    let description = language_alternative(xmp, "dc:description", comment, say_empty);
    let xmp = without_property(xmp, "dc:description");
    insert(xmp, &description, "dc", "http://purl.org/dc/elements/1.1/")
}

/// `xmp` with each list of `SLASHED_TREE_LISTS` that it has, or that `lists_in_image` names,
/// written again from its lr:hierarchicalSubject with "/" between the levels. A path with a "/"
/// inside a level is left out of them. digiKam has no escape for it there and would read the
/// "/" as one more level. It reads lr:hierarchicalSubject itself with the "/" kept in the
/// level, as a backslash. A list the image has is written also when it is empty, because if it
/// were left out, the image's own would be read in its place. Returns `None` if `xmp` has no
/// rdf:Description.
fn with_slashed_tree_lists(xmp: String, lists_in_image: &[&str]) -> Option<String> {
    let paths = list_items(&xmp, "lr:hierarchicalSubject");
    let mut xmp = xmp;
    for (list, container, uri) in SLASHED_TREE_LISTS {
        if !xmp.contains(&format!("<{list}")) && !lists_in_image.contains(&list) {
            continue;
        }
        let mut add = format!("\n   <{list}>\n    <rdf:{container}>\n");
        for p in paths.iter().filter(|p| !p.contains('/')) {
            add += &format!("     <rdf:li>{}</rdf:li>\n", escape(&p.replace('|', "/")));
        }
        add += &format!("    </rdf:{container}>\n   </{list}>");
        let prefix = list.split(':').next().unwrap_or(list);
        xmp = insert(without_property(&xmp, list), &add, prefix, uri)?;
    }
    Some(xmp)
}

/// `xmp` with lr:hierarchicalSubject holding `paths`, or without it if there are none.
fn with_paths(xmp: String, paths: &[String]) -> Option<String> {
    let xmp = without_property(&xmp, "lr:hierarchicalSubject");
    if paths.is_empty() {
        return Some(xmp);
    }
    let mut add = "\n   <lr:hierarchicalSubject>\n    <rdf:Bag>\n".to_owned();
    for p in paths {
        add += &format!("     <rdf:li>{}</rdf:li>\n", escape(p));
    }
    add += "    </rdf:Bag>\n   </lr:hierarchicalSubject>";
    insert(xmp, &add, "lr", "http://ns.adobe.com/lightroom/1.0/")
}

/// `xmp` with the elements `add` put in its first rdf:Description, and namespace `prefix`
/// declared there unless it is already in force.
fn insert(xmp: String, add: &str, prefix: &str, uri: &str) -> Option<String> {
    let (end, declared) = first_description(&xmp)?;
    // The prefix must be declared on this Description or above it. exiftool gives each
    // namespace a Description of its own, and a prefix declared on a later one is not in force
    // here.
    let ns = if declared.iter().any(|(p, _)| *p == prefix) {
        String::new()
    } else {
        format!(" xmlns:{prefix}=\"{uri}\"")
    };
    // A Description without content (<rdf:Description .../>) is opened to hold the elements.
    Some(match xmp[..end].strip_suffix('/') {
        Some(head) if !add.is_empty() => {
            format!("{head}{ns}>{add}\n  </rdf:Description>{}", &xmp[end + 1..])
        }
        Some(_) => xmp,
        None => format!("{}{ns}>{add}{}", &xmp[..end], &xmp[end + 1..]),
    })
}

/// Sets a keyword of the keyword tree, "Places|Norway|Oslo", on `path` or takes it off. Its
/// name (Oslo) goes among the keywords, and the whole path in lr:hierarchicalSubject, where
/// darktable, digiKam and Lightroom keep the tree's place of a keyword.
pub fn tag_file(path: &Path, tag: &str, on: bool) -> Result<(), String> {
    let _lock = lock_sidecar(path);
    let (mut meta, mut paths) = read_with_tree_paths(path);
    set_tag(&mut meta.keywords, &mut paths, tag, on);
    write_keywords(path, &meta, &paths, Edited::Keywords)
}

/// Sets the keyword tree paths `tags` on `path` in one write, as `tag` sets one. Each name goes
/// among its keywords and each path in lr:hierarchicalSubject, beside what it has, which stays.
/// Nothing is written when it has them all.
pub fn add_tags(path: &Path, tags: &[String]) -> Result<(), String> {
    let _lock = lock_sidecar(path);
    let (mut meta, mut paths) = read_with_tree_paths(path);
    let (keywords, known) = (meta.keywords.clone(), paths.clone());
    for t in tags {
        set_tag(&mut meta.keywords, &mut paths, t, true);
    }
    if meta.keywords == keywords && paths == known {
        return Ok(());
    }
    write_keywords(path, &meta, &paths, Edited::Keywords)
}

/// `xmp` without property `tag` in any form it is written in (`save::xmp_without_property`).
pub(crate) fn without_property(xmp: &str, tag: &str) -> String {
    let out = crate::images::save::xmp_without_property(xmp.as_bytes(), tag.as_bytes());
    // It cuts only at ASCII characters, so the text stays UTF-8.
    String::from_utf8(out).unwrap_or_else(|_| xmp.to_owned())
}

/// Reads the top-level properties of every rdf:Description of `xmp`, written as attributes or as
/// elements, and every namespace it declares. An attribute is given as the element it stands
/// for, so a packet written of them holds only elements. A Description is read up to what does
/// not read as a property (a property cut off, or text between them), and the rest of it is not
/// read. Each search goes on from where the last one ended, so a hostile packet takes linear
/// time. A property is named by its namespace (`property_name`).
pub(crate) fn properties(xmp: &str) -> Properties {
    let mut out = Properties::default();
    let mut namespaces: Vec<(String, String)> = Vec::new();
    for (prefix, uri) in namespace_declarations(xmp) {
        if !namespaces.iter().any(|(p, _)| p == prefix) {
            namespaces.push((prefix.to_owned(), uri.to_owned()));
        }
    }
    let namespaces = Namespaces::new(namespaces);
    let mut from = 0;
    while let Some(found) = xmp[from..].find("<rdf:Description") {
        let start = from + found;
        let Some(gt) = start_tag_end(xmp, start) else {
            break;
        };
        let head = &xmp[start + "<rdf:Description".len()..gt];
        for (name, value) in attributes(head.trim_end_matches('/')) {
            if name.contains(':')
                && !name.starts_with("xmlns:")
                && !name.starts_with("rdf:")
                && !name.starts_with("xml:")
            {
                let element = format!("<{name}>{value}</{name}>");
                let name = property_name(name, &namespaces);
                out.list.push((name, element, namespaces.clone()));
            }
        }
        from = gt + 1;
        if !head.ends_with('/') {
            from = child_properties(xmp, from, &namespaces, &mut out);
        }
    }
    out
}

/// The namespaces that `xmp` declares, each as (prefix, URI), in the order they are written.
/// It is the one reading of them, for `properties` and for the writers (`first_description`).
fn namespace_declarations(xmp: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    for (at, _) in xmp.match_indices("xmlns:") {
        // A namespace's name is short. Without a cap, a packet of a million unclosed quotes
        // would be searched to its end a million times. The cap is moved back to the start of
        // a letter, as a cut inside one would panic.
        let rest = &xmp[at + 6..xmp.floor_char_boundary(at + 1024)];
        // XML allows spaces around the "=", as in `xmlns:tiff = "…"`.
        let declared = rest.split_once('=');
        if let Some((prefix, uri)) =
            declared.and_then(|(p, v)| Some((p.trim_end(), quoted(v.trim_start())?)))
            && is_name(prefix)
            && !prefix.contains(':')
        {
            out.push((prefix, uri));
        }
    }
    out
}

/// Returns where the start tag of the first rdf:Description in `xmp` ends, at its '>' past any
/// '>' in its quoted values, and the namespaces declared before that, on it or above it. The
/// writers put what they add into this Description, and read it as `properties` does, so a
/// declaration written `xmlns:dc = "…"` is not declared twice. Returns `None` when `xmp` has no
/// Description or its start tag does not end.
pub(crate) fn first_description(xmp: &str) -> Option<(usize, Vec<(&str, &str)>)> {
    let end = start_tag_end(xmp, xmp.find("<rdf:Description")?)?;
    Some((end, namespace_declarations(&xmp[..end])))
}

/// Reads the elements in `xmp` from `from` on, as properties of the rdf:Description they are
/// in, into `read`, named by the packet's namespaces `namespaces` (`property_name`). It reads
/// up to the end of that Description or what does not read as an element. Returns where it
/// stopped.
fn child_properties(
    xmp: &str,
    mut from: usize,
    namespaces: &Namespaces,
    read: &mut Properties,
) -> usize {
    loop {
        let rest = &xmp[from..];
        let text = rest.trim_start();
        from += rest.len() - text.len();
        if let Some(comment) = text.strip_prefix("<!--") {
            match comment.find("-->") {
                Some(end) => from += 4 + end + 3,
                None => return xmp.len(),
            }
            continue;
        }
        if !text.starts_with('<') || text.starts_with("</") {
            return from;
        }
        let name_end = text[1..].find(|c: char| c.is_whitespace() || c == '>' || c == '/');
        let Some(name) = name_end.map(|e| &text[1..1 + e]).filter(|n| is_name(n)) else {
            return xmp.len();
        };
        let Some(gt) = start_tag_end(xmp, from) else {
            return xmp.len();
        };
        let end = if xmp[..gt].ends_with('/') {
            gt + 1
        } else {
            let close = format!("</{name}>");
            match xmp[gt..].find(&close) {
                Some(e) => gt + e + close.len(),
                None => return xmp.len(),
            }
        };
        if name.contains(':') {
            let name = property_name(name, namespaces);
            read.list
                .push((name, xmp[from..end].to_owned(), namespaces.clone()));
        }
        from = end;
    }
}

/// Returns the name of the property written `name` in a packet that binds its prefixes as
/// `namespaces` says. It has the prefix of its namespace in `KNOWN_NAMESPACES`, or else the
/// prefix as written. A known prefix bound to another namespace, or to none, is replaced by
/// that namespace in braces ("{urn:other}Make"), so the name cannot pass for a known one. The
/// namespace's control characters are replaced (`without_control_characters`), as the name is
/// saved in Copy for Sharing's choice, one name to a line.
fn property_name(name: &str, namespaces: &[(String, String)]) -> String {
    let Some((prefix, local)) = name.split_once(':') else {
        return name.to_owned();
    };
    let uri = namespaces
        .iter()
        .find(|(p, _)| p == prefix)
        .map(|(_, u)| u.as_str());
    name_in_namespace(prefix, local, uri)
}

/// Returns the name of the property `local` written with `prefix`, which is bound to the
/// namespace `uri`, or to none, as `property_name` says. An empty prefix is a default
/// namespace's, and a property in an unknown default namespace is named by it in braces too.
fn name_in_namespace(prefix: &str, local: &str, uri: Option<&str>) -> String {
    match uri.and_then(|u| KNOWN_NAMESPACES.iter().find(|(_, known)| *known == u)) {
        Some((known, _)) => format!("{known}:{local}"),
        None if prefix.is_empty() || KNOWN_NAMESPACES.iter().any(|(known, _)| *known == prefix) => {
            format!(
                "{{{}}}{local}",
                without_control_characters(uri.unwrap_or(""))
            )
        }
        None => format!("{prefix}:{local}"),
    }
}

/// Returns where the start tag that begins at `start` in `xmp` ends, at its '>', past any '>'
/// in its quoted values, or `None` when it does not end.
fn start_tag_end(xmp: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (i, c) in xmp[start..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '>') => return Some(start + i),
            _ => {}
        }
    }
    None
}

/// Returns the attributes of a start tag's text `head` after its name, as (name, value), the
/// value as written, up to what does not read as one.
fn attributes(head: &str) -> Vec<(&str, &str)> {
    let (mut out, mut rest) = (Vec::new(), head);
    while let Some((name, after)) = rest.split_once('=') {
        let after = after.trim_start();
        let (name, Some(value)) = (name.trim(), quoted(after)) else {
            break;
        };
        if !is_name(name) {
            break;
        }
        out.push((name, value));
        rest = &after[value.len() + 2..];
    }
    out
}

/// Whether `text` reads as an XML name, such as "dc:subject", so a packet written with it as an
/// element's name stays XML.
fn is_name(text: &str) -> bool {
    !text.is_empty()
        && (text.chars()).all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Returns the text between the quotes that `text` starts with, single or double, or `None`
/// when it starts with none or the quote is not closed.
fn quoted(text: &str) -> Option<&str> {
    let quote = text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let end = text[1..].find(quote)?;
    Some(&text[1..1 + end])
}

/// Writes a new XMP packet of the properties of `properties`. The properties of each packet
/// they came from go into an rdf:Description of their own, with the namespaces they use
/// declared on it, so a prefix that two packets bind to two namespaces keeps both.
pub(crate) fn packet(properties: &Properties) -> Vec<u8> {
    let mut out = String::from(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
    );
    let mut packets: Vec<&Namespaces> = Vec::new();
    for (.., namespaces) in &properties.list {
        if !packets.iter().any(|p| Namespaces::ptr_eq(p, namespaces)) {
            packets.push(namespaces);
        }
    }
    for namespaces in packets {
        let elements: Vec<&String> = (properties.list.iter())
            .filter(|(.., n)| Namespaces::ptr_eq(n, namespaces))
            .map(|(_, element, _)| element)
            .collect();
        out += "  <rdf:Description rdf:about=\"\"";
        for (prefix, uri) in namespaces.iter() {
            let used = (elements.iter()).any(|e| e.contains(&format!("{prefix}:")));
            if used && prefix != "x" && prefix != "rdf" {
                let uri = uri.replace('"', "&quot;").replace('<', "&lt;");
                out += &format!("\n    xmlns:{prefix}=\"{uri}\"");
            }
        }
        out += ">\n";
        for element in elements {
            out += &format!("   {element}\n");
        }
        out += "  </rdf:Description>\n";
    }
    out += " </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>";
    out.into_bytes()
}

/// Returns the property element `element` with only its own value. Every property written
/// inside it, as an element or as an attribute, is left out unless `keeps` keeps its name. A
/// name is read by the namespace its prefix is bound to there (`name_inside`): by a declaration
/// inside the element, by a default namespace, or else by the packet's namespaces `namespaces`,
/// as `property_name` reads the properties themselves. The RDF and XML names that give the
/// value its form (rdf:Seq, rdf:li, rdf:parseType, xml:lang) stay, and comments go. Without
/// it, a kept property could carry a value the user left out, such as a GPS position written
/// inside a gain map's property. Returns an empty text when the element does not read as
/// balanced XML (an element that never ends, a start tag without its '>'), so that a property
/// that cannot be read is left out whole rather than written into a packet broken.
pub(crate) fn with_own_value_only(
    element: &str,
    namespaces: &[(String, String)],
    keeps: &dyn Fn(&str) -> bool,
) -> String {
    // The elements open at this point, each with the namespaces that it declares.
    let mut open: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    let mut out = String::with_capacity(element.len());
    let mut from = 0;
    while let Some(found) = element[from..].find('<') {
        let start = from + found;
        out += &element[from..start];
        let rest = &element[start..];
        // A comment goes, and a CDATA section is text, which may hold a quote or a '<'.
        if let Some(end_mark) = [("<!--", "-->"), ("<![CDATA[", "]]>")]
            .into_iter()
            .find_map(|(begins, ends)| rest.starts_with(begins).then_some(ends))
        {
            let Some(end) = rest.find(end_mark) else {
                return String::new();
            };
            if end_mark == "]]>" {
                out += &rest[..end + end_mark.len()];
            }
            from = start + end + end_mark.len();
            continue;
        }
        let Some(gt) = start_tag_end(element, start) else {
            return String::new();
        };
        let is_empty_element = element[..gt].ends_with('/');
        let tag = element[start + 1..gt].trim_end_matches('/');
        if let Some(closed) = tag.strip_prefix('/') {
            if open.pop().is_none_or(|(name, _)| name != closed.trim()) {
                return String::new();
            }
            out += &element[start..=gt];
            from = gt + 1;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            out += &element[start..=gt];
            from = gt + 1;
            continue;
        }
        let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
        let (name, head) = tag.split_at(name_end);
        let found = attributes(head);
        let declared: Vec<(&str, &str)> = (found.iter())
            .filter_map(|&(n, uri)| match n.strip_prefix("xmlns")? {
                "" => Some(("", uri)),
                prefix => Some((prefix.strip_prefix(':')?, uri)),
            })
            .collect();
        let is_kept = |name: &str, is_attribute: bool| {
            name_inside(name, is_attribute, &declared, &open, namespaces).is_none_or(|n| keeps(&n))
        };
        if start > 0 && !is_kept(name, false) {
            from = match is_empty_element {
                true => gt + 1,
                false => element_end(element, gt + 1, name),
            };
            continue;
        }
        let kept: Vec<_> = found.iter().filter(|(n, _)| is_kept(n, true)).collect();
        // The attributes end where the last value's closing quote is.
        let read_to = found.last().map_or(0, |(_, value)| {
            value.as_ptr() as usize - head.as_ptr() as usize + value.len() + 1
        });
        if kept.len() == found.len() && head[read_to..].trim().is_empty() {
            out += &element[start..=gt];
        } else {
            out.push('<');
            out += name;
            for (n, value) in kept {
                let quote = if value.contains('"') { '\'' } else { '"' };
                out += &format!(" {n}={quote}{value}{quote}");
            }
            out += if is_empty_element { "/>" } else { ">" };
        }
        if !is_empty_element {
            open.push((name, declared));
        }
        from = gt + 1;
    }
    match open.is_empty() {
        true => out + &element[from..],
        false => String::new(),
    }
}

/// Returns the property name of the element or attribute `name` inside a property element, as
/// `property_name` gives it. Its prefix is looked up in `declared`, the declarations of its own
/// element, then in those of the elements `open` around it, innermost first, and last in the
/// packet's `namespaces`. An element without a prefix is in the default namespace, if one is
/// declared. Returns `None` for what is no property: a name in the RDF or XML namespace, a
/// namespace declaration, an attribute without a prefix, and an element without a namespace.
/// The rdf and xml prefixes bound to nothing count as theirs, as before the namespaces were read.
fn name_inside(
    name: &str,
    is_attribute: bool,
    declared: &[(&str, &str)],
    open: &[(&str, Vec<(&str, &str)>)],
    namespaces: &[(String, String)],
) -> Option<String> {
    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    const XML: &str = "http://www.w3.org/XML/1998/namespace";
    if name == "xmlns" || name.starts_with("xmlns:") {
        return None;
    }
    let (prefix, local) = name.split_once(':').unwrap_or(("", name));
    if prefix.is_empty() && is_attribute {
        return None;
    }
    let in_open = (open.iter().rev()).flat_map(|(_, d)| d.iter().rev());
    let uri = (declared.iter().rev().chain(in_open))
        .find(|(p, _)| *p == prefix)
        .map(|(_, uri)| *uri)
        .or_else(|| {
            (namespaces.iter())
                .find(|(p, _)| p == prefix)
                .map(|(_, u)| u.as_str())
        });
    match (prefix, uri) {
        ("xml", _) | (_, Some(RDF | XML)) | ("" | "rdf", None) => None,
        _ => Some(name_in_namespace(prefix, local, uri)),
    }
}

/// Returns where the element `name` whose start tag ends before `from` in `xmp` ends, past its
/// end tag, counting the elements of the same name inside it. Returns the end of `xmp` when it
/// does not end.
fn element_end(xmp: &str, mut from: usize, name: &str) -> usize {
    let (open, close) = (format!("<{name}"), format!("</{name}>"));
    let mut depth = 1;
    while let Some(found) = xmp[from..].find('<') {
        let at = from + found;
        let rest = &xmp[at..];
        if rest.starts_with(&close) {
            depth -= 1;
            from = at + close.len();
            if depth == 0 {
                return from;
            }
            continue;
        }
        let Some(gt) = start_tag_end(xmp, at) else {
            break;
        };
        let after_name = rest.strip_prefix(&open).and_then(|r| r.chars().next());
        let opens = after_name.is_some_and(|c| c.is_whitespace() || c == '>' || c == '/');
        if opens && !xmp[..gt].ends_with('/') {
            depth += 1;
        }
        from = gt + 1;
    }
    xmp.len()
}

/// `text` as XML character data or an attribute's value. The characters that XML 1.0 does not
/// allow (the control characters below U+0020 but tab, line feed and carriage return, and the
/// noncharacters U+FFFE and U+FFFF) are left out, as no entity can stand for them either. Left
/// in, they make the whole sidecar unreadable for exiv2, and so for darktable and digiKam. A
/// comment pasted from a PDF may hold a form feed.
pub(crate) fn escape(text: &str) -> String {
    let not_allowed = |c: char| {
        (c < ' ' && !matches!(c, '\t' | '\n' | '\r')) || matches!(c, '\u{fffe}' | '\u{ffff}')
    };
    text.replace(not_allowed, "")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out += &rest[..i];
        rest = &rest[i..];
        // The longest entity, "&#x10FFFF;", is 10 bytes, so a ';' further on ends none. Looking
        // to the next ';' from each '&' was quadratic (a million '&' and one ';' took hours).
        let Some(end) = rest.as_bytes().iter().take(11).position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .map_or_else(
                    || entity.strip_prefix('#').and_then(|n| n.parse().ok()),
                    |h| u32::from_str_radix(h, 16).ok(),
                )
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out + rest
}

/// Keyword tree paths cleaned up: trimmed around each level, without empty ones, sorted, and
/// each once. A path with a line break, which a sidecar can give as `&#10;`, is left out, because
/// the keyword tree file would read it back as two paths.
pub(crate) fn keyword_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = paths
        .map(|p| {
            p.split('|')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("|")
        })
        .filter(|p| !p.is_empty() && fits_on_a_line(Path::new(p)))
        .collect();
    // A path in another letter case is the same path, and the first spelling stays.
    out.sort_by_key(|p| p.to_lowercase());
    out.dedup_by(|a, b| same_keyword(a, b));
    out
}

/// The keyword tree that the keyword tree file's bytes `file` hold. A line that is not UTF-8 is
/// skipped, so the rest of the tree still shows.
pub(crate) fn keyword_tree_of(file: &[u8]) -> Vec<String> {
    let lines = file.split(|&b| b == b'\n');
    keyword_paths(lines.filter_map(|line| std::str::from_utf8(line).ok()))
}

/// The keyword tree file `file` with `add` added, or `remove` and the paths below it taken out
/// (`keyword_tree_changed`), and the tree it then holds. A line that is not UTF-8, which the user
/// typed in by hand, is kept as it is after the tree, not changed into another character.
pub(crate) fn keyword_tree_file_changed(
    file: &[u8],
    add: Vec<String>,
    remove: Option<String>,
) -> (Vec<u8>, Vec<String>) {
    let tree = keyword_tree_changed(&keyword_tree_of(file), add, remove);
    let mut out = (tree.join("\n") + "\n").into_bytes();
    for line in file.split(|&b| b == b'\n') {
        if std::str::from_utf8(line).is_err() {
            out.extend(line);
            out.push(b'\n');
        }
    }
    (out, tree)
}

/// The keyword tree `tree` with `add` added, or `remove` and the paths below it taken out (an
/// inner level is only there for the paths under it).
pub(crate) fn keyword_tree_changed(
    tree: &[String],
    add: Vec<String>,
    remove: Option<String>,
) -> Vec<String> {
    let under = remove.as_ref().map(|p| format!("{p}|"));
    let kept = (tree.iter()).filter(|t| {
        remove.as_ref().is_none_or(|p| *t != p)
            && under.as_ref().is_none_or(|u| !t.starts_with(u.as_str()))
    });
    let all: Vec<&str> = kept
        .map(String::as_str)
        .chain(add.iter().map(String::as_str))
        .collect();
    keyword_paths(all.into_iter())
}

#[cfg(test)]
mod tests {
    /// A keyword tree file damaged in any way is read without a panic.
    #[test]
    fn survives_broken_keyword_tree() {
        for bytes in crate::support::test_support::broken(b"Animals\nPlaces|Oslo\nPlaces|Bergen\n")
        {
            super::keyword_tree_of(&bytes);
        }
    }

    /// A keyword written while the photo is moved, which waited for the move, lands on no old
    /// name. No sidecar is left there without its photo, for a later file of that name.
    #[test]
    fn a_write_that_waits_for_a_move_leaves_no_sidecar() {
        let dir = std::env::temp_dir().join(format!("glimt-test-meta-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let (from, to) = (dir.join("IMG.jpg"), dir.join("to/IMG.jpg"));
        std::fs::write(&from, b"photo").unwrap();
        let move_lock = super::lock_sidecar(&from);
        let writer = {
            let from = from.clone();
            std::thread::spawn(move || super::add_keywords(&from, &["Oslo".to_owned()]))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        crate::files::file_io::put_file(&from, &to, false).unwrap();
        super::follow(
            &from,
            &to,
            false,
            &Default::default(),
            &mut Default::default(),
        )
        .unwrap();
        drop(move_lock);
        assert!(writer.join().unwrap().is_err());
        assert!(!super::sidecar(&from).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A million '&' and then one ';' (a crafted keyword) are read at once, not in hours.
    #[test]
    fn unescapes_in_one_pass() {
        let text = "&".repeat(1_000_000) + "; &amp; &#x41; &#66; &ampno;";
        let start = std::time::Instant::now();
        let out = super::unescape(&text);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert!(out.ends_with("; & A B &ampno;") && out.len() == text.len() - 13);
    }

    /// Two tests never wait for each other's sidecar locks. A sidecar whose lock number
    /// another test's sidecar shares is locked at once while that test holds its own, as
    /// `a_copy_waits_for_its_sidecar_lock` does for seconds. Before, file operations' contract
    /// timed out now and then on "save frame over b".
    #[test]
    fn tests_never_share_a_sidecar_lock() {
        let dir = std::env::temp_dir().join(format!("glimt-test-own-lock-{}", std::process::id()));
        let mine = dir.join("b.png");
        let number = super::sidecar_lock_number(&super::sidecar(&mine));
        let other = (0..)
            .map(|k| dir.join(format!("other-{k}.png")))
            .find(|p| super::sidecar_lock_number(&super::sidecar(p)) == number)
            .unwrap();
        let others_lock = super::lock_sidecar(&other);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _lock = super::lock_sidecar(&mine);
            tx.send(()).unwrap();
        });
        let taken = rx.recv_timeout(std::time::Duration::from_secs(2));
        drop(others_lock);
        assert_eq!(
            taken,
            Ok(()),
            "a test waited for another test's sidecar lock"
        );
    }

    /// The lock file of a sidecar is the same in every Glimt, by FNV-1a, as known for these
    /// paths.
    #[test]
    fn lock_slots_stay() {
        use std::path::Path;
        assert_eq!(
            super::sidecar_lock_number(Path::new("")),
            0xcbf2_9ce4_8422_2325 % 64
        );
        assert_eq!(
            super::sidecar_lock_number(Path::new("a")),
            0xaf63_dc4c_8601_ec8c % 64
        );
    }

    /// Within one Glimt, a holder whose lock file could not be made waits for one whose could,
    /// and the other way round, because neither goes by another lock than the other.
    #[test]
    fn a_failed_lock_file_still_waits_in_this_glimt() {
        let dir = std::env::temp_dir().join(format!("glimt-test-slots-{}", std::process::id()));
        let photo = std::path::PathBuf::from("/photos/slot.jpg");
        for (first, then) in [(Some(dir.as_path()), None), (None, Some(dir.as_path()))] {
            let lock = super::lock_sidecar_in(first, &photo);
            let (tx, rx) = std::sync::mpsc::channel();
            let (dir2, p) = (then.map(|d| d.to_path_buf()), photo.clone());
            let waiter = std::thread::spawn(move || {
                let all = super::lock_sidecars_in(dir2.as_deref(), [&p]);
                tx.send(()).unwrap();
                drop(all);
            });
            let waited = rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err();
            drop(lock);
            waiter.join().unwrap();
            assert!(waited, "{first:?} then {then:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The lock of a sidecar holds off another holder of it, as another window would be, until
    /// it is let go.
    #[test]
    fn sidecar_lock_waits() {
        let dir = std::env::temp_dir().join(format!("glimt-test-locks-{}", std::process::id()));
        let side = std::path::PathBuf::from("/photos/a.xmp");
        let first = super::lock_file(&dir, &side).unwrap();
        let (dir2, side2) = (dir.clone(), side.clone());
        let started = std::time::Instant::now();
        let other = std::thread::spawn(move || {
            drop(super::lock_file(&dir2, &side2).unwrap());
            started.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(first);
        assert!(other.join().unwrap() >= std::time::Duration::from_millis(300));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A link planted in the folder of the sidecar locks is not followed, so the file it points
    /// to is neither emptied nor locked. Before, the lock was opened with `File::create`, which
    /// emptied it.
    #[test]
    fn a_sidecar_lock_leaves_a_planted_link_alone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-lock-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let side = std::path::PathBuf::from("/photos/b.xmp");
        let target = dir.join("notes.txt");
        std::fs::write(&target, "keep").unwrap();
        let lock = dir.join(super::sidecar_lock_number(&side).to_string());
        std::os::unix::fs::symlink(&target, lock).unwrap();
        assert!(super::lock_file(&dir, &side).is_none());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Read together, the keywords, their tree paths and the alt text are those read one by
    /// one, the sidecar's where it has them and otherwise the image's own.
    #[test]
    fn reads_all_as_one_by_one() {
        let dir = std::env::temp_dir().join(format!("glimt-test-readall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("a.jpg");
        std::fs::write(&img, b"not a jpeg").unwrap();
        std::fs::write(
            super::sidecar(&img),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lr="http://ns.adobe.com/lightroom/1.0/"><dc:subject><rdf:Bag><rdf:li>Oslo</rdf:li></rdf:Bag></dc:subject><lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Oslo</rdf:li></rdf:Bag></lr:hierarchicalSubject></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        let (meta, paths, alt_text) = super::read_all(&img);
        assert_eq!(meta.keywords, super::read(&img).keywords);
        assert_eq!(paths, super::tree_paths(&img));
        assert_eq!(paths, ["Places|Oslo"]);
        assert_eq!(alt_text, super::AltText::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;

    /// The change that adds `keywords`, as an edit of the panel's empty field sends it.
    fn adding(keywords: &[String]) -> Option<KeywordChange> {
        Some(KeywordChange::between(&[], keywords))
    }

    /// Writes a JPEG at `path` with the XMP packet `packet` inside it.
    fn jpeg_with_xmp(path: &Path, packet: &str) {
        let mut base = Vec::new();
        let mut out = std::io::Cursor::new(&mut base);
        (image::RgbImage::new(4, 2).write_to(&mut out, image::ImageFormat::Jpeg)).unwrap();
        let mut app1 = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        app1.extend(packet.as_bytes());
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend(((app1.len() + 2) as u16).to_be_bytes());
        bytes.extend(app1);
        bytes.extend(&base[2..]);
        std::fs::write(path, bytes).unwrap();
    }

    /// A keyword hierarchy that only the image holds, as Lightroom exports it, goes into the
    /// first sidecar whole, whichever writer makes it. Before, adding a keyword wrote its paths
    /// flat, so the tree's box of Places|Norway|Oslo was no longer ticked.
    #[test]
    fn the_first_sidecar_keeps_the_images_hierarchy() {
        let dir = std::env::temp_dir().join(format!("glimt-test-own-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let packet = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description rdf:about=\"\" \
             xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:lr=\"http://ns.adobe.com/lightroom/1.0/\">\
             <dc:subject><rdf:Bag><rdf:li>Places</rdf:li><rdf:li>Norway</rdf:li><rdf:li>Oslo</rdf:li></rdf:Bag></dc:subject>\
             <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Norway|Oslo</rdf:li></rdf:Bag></lr:hierarchicalSubject>\
             </rdf:Description></rdf:RDF></x:xmpmeta>";
        let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
        jpeg_with_xmp(&a, packet);
        jpeg_with_xmp(&b, packet);
        add_keywords(&a, &["beach".into()]).unwrap();
        write_changed(&b, adding(&["beach".into()]), None).unwrap();
        for file in [&a, &b] {
            assert!(sidecar(file).exists());
            assert_eq!(tree_paths(file), ["Places|Norway|Oslo", "beach"]);
            let (keywords, paths) = (read(file).keywords, tree_paths(file));
            assert!(has_tag(&keywords, &paths, "Places|Norway|Oslo"));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// digiKam's lists follow a keyword added and one taken out. digiKam reads its own TagsList
    /// before dc:subject, so the writers write it again from the paths they write, as they write
    /// Windows' LastKeywordXMP. Before 0.76.0 the lists kept a keyword taken out.
    #[test]
    fn digikams_own_keyword_lists_follow_an_edit() {
        let dir = std::env::temp_dir().join(format!("glimt-test-digikam-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        std::fs::write(
            sidecar(&file),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:digiKam="http://www.digikam.org/ns/1.0/"
    xmlns:MicrosoftPhoto="http://ns.microsoft.com/photo/1.0/">
   <digiKam:TagsList><rdf:Seq><rdf:li>Places/Oslo</rdf:li><rdf:li>Private</rdf:li></rdf:Seq></digiKam:TagsList>
   <MicrosoftPhoto:LastKeywordXMP><rdf:Bag><rdf:li>Places/Oslo</rdf:li><rdf:li>Private</rdf:li></rdf:Bag></MicrosoftPhoto:LastKeywordXMP>
   <dc:subject><rdf:Bag><rdf:li>Oslo</rdf:li><rdf:li>Private</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        assert_eq!(tree_paths(&file), ["Places|Oslo", "Private"]);
        let change = KeywordChange::between(&["Private".into()], &["Beach".into()]);
        write_changed(&file, Some(change), None).unwrap();
        let written = std::fs::read_to_string(sidecar(&file)).unwrap();
        for list in ["digiKam:TagsList", "MicrosoftPhoto:LastKeywordXMP"] {
            assert_eq!(
                list_items(&written, list),
                ["Places/Oslo", "Beach"],
                "{list}"
            );
        }
        assert_eq!(read(&file).keywords, ["Oslo", "Beach"]);
        assert_eq!(tree_paths(&file), ["Places|Oslo", "Beach"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A keyword level with a "/" in it, as Lightroom allows, is kept out of digiKam's and
    /// Windows' lists, where the "/" would be one more level: Music|AC/DC would be the tag DC
    /// under AC. lr:hierarchicalSubject keeps it.
    #[test]
    fn a_slash_inside_a_level_stays_out_of_the_slashed_lists() {
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/" xmlns:digiKam="http://www.digikam.org/ns/1.0/">
   <digiKam:TagsList><rdf:Seq><rdf:li>Old</rdf:li></rdf:Seq></digiKam:TagsList>
   <lr:hierarchicalSubject><rdf:Bag><rdf:li>Music|AC/DC</rdf:li><rdf:li>Places|Oslo</rdf:li></rdf:Bag></lr:hierarchicalSubject>
  </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let written = with_slashed_tree_lists(xmp.to_owned(), &[]).unwrap();
        assert_eq!(list_items(&written, "digiKam:TagsList"), ["Places/Oslo"]);
        assert_eq!(
            list_items(&written, "lr:hierarchicalSubject"),
            ["Music|AC/DC", "Places|Oslo"]
        );
        // A slashed list that already holds the path as Music/AC/DC reads as that one path,
        // not as Music|AC|DC beside it.
        let both = xmp.replace("<rdf:li>Old</rdf:li>", "<rdf:li>Music/AC/DC</rdf:li>");
        assert_eq!(tree_paths_in(&both), ["Music|AC/DC", "Places|Oslo"]);
        // Without that path in lr:hierarchicalSubject, each "/" is a level.
        let alone = both.replace("<rdf:li>Music|AC/DC</rdf:li>", "");
        assert_eq!(tree_paths_in(&alone), ["Places|Oslo", "Music|AC|DC"]);
    }

    /// A path that only digiKam's TagsList holds stays after an edit of the comment and of the
    /// keywords, and goes into lr:hierarchicalSubject too. ACDSee's categories, which are not
    /// keywords, stay as they are. Before, any edit took TagsList out, and People/Ann became
    /// Ann at the top of the tree.
    #[test]
    fn a_hierarchy_that_only_one_list_holds_stays() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tagslist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        let categories = "<acdsee:categories>&lt;Categories&gt;People&lt;/Categories&gt;\
            </acdsee:categories>";
        std::fs::write(
            sidecar(&file),
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:digiKam="http://www.digikam.org/ns/1.0/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/" xmlns:acdsee="http://ns.acdsee.com/iptc/1.0/">
   <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Oslo</rdf:li></rdf:Bag></lr:hierarchicalSubject>
   <digiKam:TagsList><rdf:Seq><rdf:li>Places/Oslo</rdf:li><rdf:li>People/Ann</rdf:li></rdf:Seq></digiKam:TagsList>
   <dc:subject><rdf:Bag><rdf:li>Oslo</rdf:li><rdf:li>Ann</rdf:li></rdf:Bag></dc:subject>
   {categories}
  </rdf:Description></rdf:RDF></x:xmpmeta>"#
            ),
        )
        .unwrap();
        write_changed(&file, None, Some("Trip".into())).unwrap();
        let written = std::fs::read_to_string(sidecar(&file)).unwrap();
        assert_eq!(
            list_items(&written, "digiKam:TagsList"),
            ["Places/Oslo", "People/Ann"]
        );
        assert_eq!(
            list_items(&written, "lr:hierarchicalSubject"),
            ["Places|Oslo"]
        );
        assert!(written.contains(categories));
        assert_eq!(read(&file).comment, "Trip");
        write_changed(&file, adding(&["Beach".into()]), None).unwrap();
        let written = std::fs::read_to_string(sidecar(&file)).unwrap();
        let paths = ["Places|Oslo", "People|Ann", "Beach"];
        assert_eq!(list_items(&written, "lr:hierarchicalSubject"), paths);
        assert_eq!(
            list_items(&written, "digiKam:TagsList"),
            ["Places/Oslo", "People/Ann", "Beach"]
        );
        assert!(written.contains(categories));
        assert_eq!(read(&file).comment, "Trip");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A keyword taken out of a photo whose own XMP has digiKam's TagsList is taken out of that
    /// list in the sidecar too, which is written even when it is left empty. Otherwise the
    /// image's list would still hold the keyword for a program that reads both.
    #[test]
    fn the_images_own_tree_list_is_countered() {
        let dir = std::env::temp_dir().join(format!("glimt-test-own-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        jpeg_with_xmp(
            &file,
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description rdf:about=\"\" \
             xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:digiKam=\"http://www.digikam.org/ns/1.0/\">\
             <dc:subject><rdf:Bag><rdf:li>Private</rdf:li></rdf:Bag></dc:subject>\
             <digiKam:TagsList><rdf:Seq><rdf:li>Private</rdf:li></rdf:Seq></digiKam:TagsList>\
             </rdf:Description></rdf:RDF></x:xmpmeta>",
        );
        let taken_out = KeywordChange::between(&["Private".into()], &[]);
        write_changed(&file, Some(taken_out), None).unwrap();
        let written = std::fs::read_to_string(sidecar(&file)).unwrap();
        assert!(written.contains("<digiKam:TagsList>"));
        assert!(list_items(&written, "digiKam:TagsList").is_empty());
        assert!(read(&file).keywords.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The comment and the alt text show the x-default language, or else the first, and an
    /// edit replaces only that one. Before, the first was shown, and an edit wrote it alone as
    /// x-default, so the other languages were lost.
    #[test]
    fn an_edit_keeps_the_other_languages() {
        let xmp = r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description><rdf:Alt><rdf:li xml:lang="de">Hallo</rdf:li><rdf:li xml:lang="x-default">Helo</rdf:li></rdf:Alt></dc:description>
   <Iptc4xmpCore:AltTextAccessibility><rdf:Alt><rdf:li xml:lang="de">Ein Hund</rdf:li><rdf:li xml:lang="en-US">A dgo</rdf:li></rdf:Alt></Iptc4xmpCore:AltTextAccessibility>
  </rdf:Description>"#;
        assert_eq!(parse(xmp).comment, "Helo");
        assert_eq!(parse_alt_text(xmp).text, "Ein Hund");
        let meta = KeywordsAndComment {
            keywords: Vec::new(),
            comment: "Hello".into(),
        };
        let written = set_keywords_and_comment(xmp, &meta, false).unwrap();
        let alt_text = AltText {
            text: "Ein Hund am Strand".into(),
            extended_description: String::new(),
        };
        let written = set_alt_text(&written, &alt_text, false).unwrap();
        let languages = |tag| items_with_languages(&written, tag);
        let pair = |l: &str, t: &str| (l.to_owned(), t.to_owned());
        assert_eq!(
            languages("dc:description"),
            [pair("de", "Hallo"), pair("x-default", "Hello")]
        );
        assert_eq!(
            languages(ALT),
            [pair("de", "Ein Hund am Strand"), pair("en-US", "A dgo")]
        );
        assert_eq!(parse(&written).comment, "Hello");
    }

    /// The writers read the first Description's start tag as the reader does: a namespace
    /// declared with spaces around its equals sign is not declared again, and a '>' inside an
    /// attribute's value does not end the tag.
    #[test]
    fn writers_read_the_start_tag_as_the_reader_does() {
        let xmp = r#"<rdf:Description rdf:about="" xmlns:dc = "http://purl.org/dc/elements/1.1/"
    photoshop:Headline="a>b"/>"#;
        let meta = KeywordsAndComment {
            keywords: vec!["beach".into()],
            comment: String::new(),
        };
        let written = set_keywords_and_comment(xmp, &meta, false).unwrap();
        assert_eq!(written.matches("xmlns:dc").count(), 1, "{written}");
        assert!(written.contains(r#"photoshop:Headline="a>b""#), "{written}");
        assert_eq!(parse(&written).keywords, ["beach"]);
        let names: Vec<String> = (properties(&written).list.into_iter())
            .map(|(name, ..)| name)
            .collect();
        assert!(names.contains(&"dc:subject".to_owned()), "{names:?}");
    }

    /// A control character or noncharacter in a comment or keyword, typed, pasted or read as
    /// `&#1;`, is left out of the sidecar, which stays well-formed XML for exiv2.
    #[test]
    fn control_characters_are_not_written() {
        let meta = KeywordsAndComment {
            keywords: vec![unescape("Oslo&#1;&#xFFFF;")],
            comment: "page\u{c}break\ttab\nline\u{fffe}".into(),
        };
        let written = set_keywords_and_comment("<rdf:Description rdf:about=\"\">", &meta, false);
        let written = written.unwrap();
        let allowed = |c: char| {
            (c >= ' ' || matches!(c, '\t' | '\n' | '\r')) && !matches!(c, '\u{fffe}' | '\u{ffff}')
        };
        assert!(written.chars().all(allowed), "{written:?}");
        assert_eq!(parse(&written).keywords, ["Oslo"]);
        assert_eq!(parse(&written).comment, "pagebreak\ttab\nline");
    }

    /// A keyword taken out of a path cuts the path back to the level above it, when that level
    /// is still a keyword, instead of writing the levels above it as keywords at the top.
    #[test]
    fn a_path_without_its_keyword_keeps_its_levels_above() {
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let paths = k(&["Places|Norway|Oslo"]);
        assert_eq!(
            tree_paths_for(&k(&["Places", "Norway"]), paths.clone()),
            ["Places|Norway"]
        );
        assert_eq!(tree_paths_for(&k(&["Places"]), paths.clone()), ["Places"]);
        assert_eq!(tree_paths_for(&k(&["beach"]), paths), ["beach"]);
    }

    /// A keyword that holds a comma is taken out or changed whole in the field. Before, the
    /// change was made from the saved keywords split at commas, so taking "Smith, John" out took
    /// nothing out, and changing it to "Smith, Jon" kept it beside Jon.
    #[test]
    fn a_keyword_with_a_comma_is_changed_whole() {
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let saved = k(&["Smith, John", "x"]);
        let edited = |field: &str| {
            let mut keywords = saved.clone();
            let typed = KeywordsAndComment::parse_keywords(field);
            KeywordChange::between(&saved, &typed).apply(&mut keywords);
            keywords
        };
        assert_eq!(edited("Smith, John, x"), saved);
        assert_eq!(edited("x"), ["x"]);
        assert_eq!(edited("Smith, Jon, x"), ["x", "Smith", "Jon"]);
    }

    /// Taking out one of two keywords that differ only in letter case keeps the other. Before,
    /// `apply` took out every keyword the same but for letter case, so both went.
    #[test]
    fn taking_out_one_spelling_keeps_the_other() {
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let saved = k(&["Rose", "rose"]);
        let mut keywords = saved.clone();
        KeywordChange::between(&saved, &k(&["Rose"])).apply(&mut keywords);
        assert_eq!(keywords, ["Rose"]);
        let mut keywords = k(&["rose"]);
        KeywordChange::between(&keywords.clone(), &k(&["Rose"])).apply(&mut keywords);
        assert_eq!(
            keywords,
            ["Rose"],
            "a change of letter case is still written"
        );
    }

    /// An edit of the keywords field changes only the keywords it added or took out. A keyword
    /// that another window or a Lightroom import wrote after the panel read the file stays, and
    /// so does one that holds a comma, which the field would split in two.
    #[test]
    fn an_edit_keeps_keywords_written_after_the_panel_read() {
        let dir = std::env::temp_dir().join(format!("glimt-test-kw-change-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        write_changed(&file, adding(&["Smith, John".into(), "x".into()]), None).unwrap();
        // The panel shows what it read, and then another window adds Oslo.
        let shown = read(&file).keywords;
        add_keywords(&file, &["Oslo".into()]).unwrap();
        // The user adds beach and takes x out in the field.
        let edited = KeywordsAndComment::parse_keywords("Smith, John, beach");
        write_changed(&file, Some(KeywordChange::between(&shown, &edited)), None).unwrap();
        assert_eq!(read(&file).keywords, ["Smith, John", "Oslo", "beach"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A JPEG with an empty APP1 segment followed by the XMP name gives no keywords, and no
    /// panic.
    #[test]
    fn reads_an_empty_xmp_segment() {
        let file = std::env::temp_dir().join(format!("glimt-xmp-{}.jpg", std::process::id()));
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1, 0, 2];
        jpeg.extend(b"http://ns.adobe.com/xap/1.0/\0");
        std::fs::write(&file, jpeg).unwrap();
        assert!(read(&file).keywords.is_empty());
        std::fs::remove_file(file).unwrap();
    }

    /// A sidecar that cannot be read as text is left as it is, not replaced by a new one.
    #[test]
    fn keeps_a_sidecar_it_cannot_read() {
        let dir = std::env::temp_dir().join(format!("glimt-test-bad-xmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        std::fs::write(sidecar(&file), b"\xff\xfe not UTF-8").unwrap();
        let meta = KeywordsAndComment {
            keywords: vec!["x".into()],
            comment: String::new(),
        };
        assert!(write_changed(&file, adding(&meta.keywords), Some(meta.comment.clone())).is_err());
        assert_eq!(
            std::fs::read(sidecar(&file)).unwrap(),
            b"\xff\xfe not UTF-8"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A read-only sidecar is refused with its own name in the error. Before, the bare "the
    /// file is read-only" was shown against the image, which is not read-only.
    #[test]
    fn a_read_only_sidecar_is_named() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("glimt-test-ro-xmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        std::fs::write(sidecar(&file), "<x:xmpmeta/>").unwrap();
        let ro = std::fs::Permissions::from_mode(0o444);
        std::fs::set_permissions(sidecar(&file), ro).unwrap();
        let refused = update_sidecar(&file, true, |x| Some(format!("{x} "))).unwrap_err();
        assert!(
            refused.starts_with(&sidecar(&file).display().to_string()),
            "{refused}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sidecar that is a link, as a downloaded folder may hold, is not written through. The
    /// XMP file it points to, elsewhere among the user's files, keeps what it had.
    #[test]
    fn never_writes_through_a_linked_sidecar() {
        let dir = std::env::temp_dir().join(format!("glimt-test-xmp-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (file, elsewhere) = (dir.join("a.jpg"), dir.join("other.xmp"));
        std::fs::write(&file, b"").unwrap();
        let theirs = "<x:xmpmeta><rdf:RDF><rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        std::fs::write(&elsewhere, theirs).unwrap();
        std::os::unix::fs::symlink(&elsewhere, sidecar(&file)).unwrap();
        let written = write_changed(&file, adding(&["x".into()]), None);
        assert!(written.is_err_and(|e| e.contains("a link")));
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), theirs);
        let link = std::fs::symlink_metadata(sidecar(&file)).unwrap();
        assert!(link.file_type().is_symlink());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An image renamed with its folder while its keywords waited does not make a folder of the
    /// old name again, with a sidecar of nothing in it.
    #[test]
    fn no_sidecar_for_an_image_gone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-gone-{}", std::process::id()));
        let file = dir.join("trip").join("a.jpg");
        assert!(write_changed(&file, adding(&["x".into()]), None).is_err());
        assert!(!dir.exists());
    }

    /// In exiftool's layout, with a Description for each namespace, "dc" declared on the second
    /// one is declared again on the first, where the keywords go.
    #[test]
    fn declares_dc_where_it_writes() {
        let exiftool = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF>\
                        <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\
                        </rdf:Description><rdf:Description rdf:about=\"\" \
                        xmlns:dc=\"http://purl.org/dc/elements/1.1/\"></rdf:Description>\
                        </rdf:RDF></x:xmpmeta>";
        let meta = KeywordsAndComment {
            keywords: vec!["x".into()],
            comment: String::new(),
        };
        let written = set_keywords_and_comment(exiftool, &meta, false).unwrap();
        let first = &written[..written.find("<dc:subject").unwrap()];
        assert_eq!(first.matches("xmlns:dc=").count(), 1, "{written}");
    }

    /// A keyword of the tree is set on and off. Its name goes among the keywords and its path in
    /// lr:hierarchicalSubject, and the other keywords and paths are kept.
    #[test]
    fn tags_with_the_tree_path() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        let paths = |file: &Path| {
            list_items(
                &std::fs::read_to_string(sidecar(file)).unwrap(),
                "lr:hierarchicalSubject",
            )
        };
        tag_file(&file, "Places|Norway|Oslo", true).unwrap();
        tag_file(&file, "Places|Norway|Bergen", true).unwrap();
        assert_eq!(read(&file).keywords, ["Oslo", "Bergen"]);
        assert_eq!(paths(&file), ["Places|Norway|Oslo", "Places|Norway|Bergen"]);
        assert!(
            std::fs::read_to_string(sidecar(&file))
                .unwrap()
                .contains("xmlns:lr=")
        );
        tag_file(&file, "Places|Norway|Oslo", false).unwrap();
        assert_eq!(read(&file).keywords, ["Bergen"]);
        assert_eq!(paths(&file), ["Places|Norway|Bergen"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// One keyword for two paths that end in it, also in another letter case, is set once. It
    /// is kept while one of the paths is on, and only that path's box is ticked.
    #[test]
    fn keeps_a_keyword_another_path_ends_in() {
        let (mut keywords, mut paths) = (vec!["oslo".to_owned()], Vec::new());
        // When a program that writes keywords only set it, both boxes are ticked.
        assert!(has_tag(&keywords, &paths, "Events|Oslo"));
        set_tag(&mut keywords, &mut paths, "Places|Norway|Oslo", true);
        set_tag(&mut keywords, &mut paths, "Events|Oslo", true);
        assert_eq!(keywords, ["oslo"]);
        set_tag(&mut keywords, &mut paths, "events|oslo", false);
        assert_eq!(
            (&keywords[..], &paths[..]),
            (
                &["oslo".to_owned()][..],
                &["Places|Norway|Oslo".to_owned()][..]
            )
        );
        assert!(has_tag(&keywords, &paths, "Places|Norway|Oslo"));
        assert!(!has_tag(&keywords, &paths, "Events|Oslo"));
        set_tag(&mut keywords, &mut paths, "Places|Norway|Oslo", false);
        assert!(keywords.is_empty() && paths.is_empty());
        assert_eq!(
            KeywordsAndComment::parse_keywords("Oslo, beach, oslo,, beach"),
            ["Oslo", "beach", "oslo"]
        );
    }

    /// Keywords typed in the field get paths of their own, which Immich reads. A keyword taken
    /// away takes its paths along, and a longer path replaces a keyword's own.
    #[test]
    fn writes_a_path_for_every_keyword() {
        let paths = |k: &[&str], p: &[&str]| {
            let k: Vec<String> = k.iter().map(|s| s.to_string()).collect();
            tree_paths_for(&k, p.iter().map(|s| s.to_string()).collect())
        };
        assert_eq!(paths(&["beach", "Oslo"], &[]), ["beach", "Oslo"]);
        assert_eq!(paths(&["beach"], &["Places|Oslo", "beach"]), ["beach"]);
        assert_eq!(paths(&["oslo"], &["Oslo", "Places|Oslo"]), ["Places|Oslo"]);
        let meta = KeywordsAndComment {
            keywords: vec!["beach".into()],
            comment: String::new(),
        };
        let xmp =
            set_keywords_and_comment(r#"<rdf:Description rdf:about="">"#, &meta, false).unwrap();
        assert_eq!(list_items(&xmp, "lr:hierarchicalSubject"), ["beach"]);
    }

    /// Keywords inside a JPEG that are cleared stay cleared, and are not read from the image
    /// again.
    #[test]
    fn clears_the_image_own_keywords() {
        let dir = std::env::temp_dir().join(format!("glimt-test-own-xmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        let mut base = Vec::new();
        let jpeg = image::ImageFormat::Jpeg;
        let mut out = std::io::Cursor::new(&mut base);
        image::RgbImage::new(4, 2).write_to(&mut out, jpeg).unwrap();
        let own = KeywordsAndComment {
            keywords: vec!["beach".into()],
            comment: String::new(),
        };
        let empty = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                     rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let packet = set_keywords_and_comment(empty, &own, false).unwrap();
        let mut app1 = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        app1.extend(packet.as_bytes());
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend(((app1.len() + 2) as u16).to_be_bytes());
        bytes.extend(app1);
        bytes.extend(&base[2..]);
        std::fs::write(&file, bytes).unwrap();
        assert_eq!(read(&file), own);
        write_changed(
            &file,
            Some(KeywordChange::between(&own.keywords, &[])),
            Some(String::new()),
        )
        .unwrap();
        assert_eq!(read(&file), KeywordsAndComment::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keywords_and_comment_round_trip() {
        let meta = KeywordsAndComment {
            keywords: vec!["beach".into(), "Tom & Ann".into()],
            comment: "Summer <2024>\nsecond line".into(),
        };
        // A darktable sidecar has an attribute-only Description with a rating, which is kept as
        // it is.
        let dt = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description rdf:about=\"\" \
                  xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>";
        let written = set_keywords_and_comment(dt, &meta, false).unwrap();
        assert!(written.contains("xmp:Rating=\"3\" xmlns:dc="));
        assert_eq!(parse(&written), meta);
        // Written again with other values, they are replaced, not added.
        let other = KeywordsAndComment {
            keywords: vec!["snow".into()],
            comment: String::new(),
        };
        let again = set_keywords_and_comment(&written, &other, false).unwrap();
        assert_eq!(parse(&again), other);
        assert_eq!(again.matches("<dc:subject>").count(), 1);
        assert_eq!(again.matches("xmlns:dc=").count(), 1);
        // With everything removed, the sidecar is as it was, but for the name space.
        let none = set_keywords_and_comment(&again, &KeywordsAndComment::default(), false).unwrap();
        assert_eq!(parse(&none), KeywordsAndComment::default());
        assert!(none.contains("xmp:Rating=\"3\""));
        assert_eq!(unescape("a&#x41;&#66;&bogus c&amp;"), "aAB&bogus c&");
        assert_eq!(
            KeywordsAndComment::parse_keywords(" a, b,,a ,c "),
            ["a", "b", "c"].map(String::from)
        );
    }

    /// The IPTC alt text and extended description are written, read back, replaced and
    /// cleared, and the keywords, comment and other fields are kept.
    #[test]
    fn alt_text_round_trip() {
        let meta = KeywordsAndComment {
            keywords: vec!["beach".into()],
            comment: "Summer".into(),
        };
        let empty = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                     rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let with_meta = set_keywords_and_comment(empty, &meta, false).unwrap();
        let alt_text = AltText {
            text: "A dog <on> a beach".into(),
            extended_description: "Long & detailed".into(),
        };
        let written = set_alt_text(&with_meta, &alt_text, false).unwrap();
        assert_eq!(parse_alt_text(&written), alt_text);
        assert_eq!(parse(&written), meta);
        assert_eq!(written.matches("xmlns:Iptc4xmpCore=").count(), 1);
        let other = AltText {
            text: "A cat".into(),
            extended_description: String::new(),
        };
        let again = set_alt_text(&written, &other, false).unwrap();
        assert_eq!(parse_alt_text(&again), other);
        assert_eq!(again.matches("AltTextAccessibility>").count(), 2);
        let none = set_alt_text(&again, &AltText::default(), false).unwrap();
        assert_eq!(parse_alt_text(&none), AltText::default());
        assert_eq!(parse(&none), meta);
        // The image's own fields that are cleared are written empty, so they are not read from
        // the image again.
        assert!(
            set_alt_text(&none, &AltText::default(), true)
                .unwrap()
                .contains("<rdf:Alt/>")
        );
        // Keywords written after the alt text leave it.
        assert_eq!(
            parse_alt_text(
                &set_keywords_and_comment(&written, &KeywordsAndComment::default(), false).unwrap()
            ),
            alt_text
        );
    }

    /// Keyword tree paths from another program are added to an image in one write. Its own
    /// keyword stays, each name and path is there once, and nothing is written when all are
    /// there.
    #[test]
    fn adds_tags_of_another_program() {
        let dir = std::env::temp_dir().join(format!("glimt-test-addtags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("a.jpg");
        std::fs::write(&photo, b"").unwrap();
        super::write_changed(&photo, adding(&["Mine".into()]), None).unwrap();
        let tags = ["Places|Norway|Oslo".to_owned(), "People|Ann".to_owned()];
        super::add_tags(&photo, &tags).unwrap();
        super::add_tags(&photo, &tags).unwrap();
        let mut keywords = super::read(&photo).keywords;
        keywords.sort();
        assert_eq!(keywords, ["Ann", "Mine", "Oslo"]);
        let mut paths = super::tree_paths(&photo);
        paths.sort();
        assert_eq!(paths, ["Mine", "People|Ann", "Places|Norway|Oslo"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Keywords added from several threads at once are not lost.
    #[test]
    fn changes_at_once_are_all_kept() {
        let dir = std::env::temp_dir().join(format!("glimt-test-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::fs::write(&path, b"").unwrap();
        std::thread::scope(|s| {
            for k in 0..8 {
                let path = &path;
                s.spawn(move || {
                    for n in 0..10 {
                        add_keywords(path, &[format!("k{k}-{n}")]).unwrap();
                    }
                });
            }
        });
        assert_eq!(read(&path).keywords.len(), 80);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Keywords and a comment written again replace those a sidecar has in any form: an element
    /// with attributes, an empty element and an attribute. Before, only `<dc:subject>` was
    /// found, and the others stayed beside the new ones, a property twice.
    #[test]
    fn replaces_a_property_in_any_form() {
        let xmp = r#"<rdf:Description rdf:about="" dc:title='t'>
   <dc:subject xmlns:dc="http://purl.org/dc/elements/1.1/"><rdf:Bag><rdf:li>a</rdf:li></rdf:Bag></dc:subject>
   <dc:description/>
   <lr:hierarchicalSubject rdf:parseType="Resource"><rdf:Bag/></lr:hierarchicalSubject>
  </rdf:Description>"#;
        let meta = KeywordsAndComment {
            keywords: vec!["b".into()],
            comment: "c".into(),
        };
        let new = set_keywords_and_comment(xmp, &meta, false).unwrap();
        for tag in ["<dc:subject", "<dc:description", "<lr:hierarchicalSubject"] {
            assert_eq!(new.matches(tag).count(), 1, "{tag} in {new}");
        }
        assert_eq!(parse(&new), meta);
        assert!(new.contains("dc:title='t'") && !new.contains("\n\n"));
    }

    /// An empty `<dc:subject/>` or `<dc:description/>` ends where it stands. Before, the
    /// readers looked for its end tag, read the items of the lists after it as keywords and
    /// comment, and a keyword edit wrote them into the sidecar.
    #[test]
    fn an_empty_element_holds_nothing() {
        let xmp = r#"<rdf:Description rdf:about="">
   <dc:subject/>
   <dc:description/>
   <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Oslo</rdf:li></rdf:Bag></lr:hierarchicalSubject>
  </rdf:Description>"#;
        assert_eq!(parse(xmp), KeywordsAndComment::default());
        let meta = KeywordsAndComment {
            keywords: vec!["Beach".into()],
            comment: String::new(),
        };
        let new = set_keywords(xmp, &meta, false).unwrap();
        assert_eq!(parse(&new), meta, "{new}");
    }

    #[test]
    fn keeps_what_other_programs_wrote() {
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Lightroom's levels above a keyword are in its path, also when 0.21.0 wrote one bare.
        let lr = k(&["Places", "Norway", "Oslo"]);
        assert_eq!(
            tree_paths_for(&lr, k(&["Places|Norway|Oslo"])),
            ["Places|Norway|Oslo"]
        );
        assert_eq!(
            tree_paths_for(&lr, k(&["Places|Norway|Oslo", "Norway"])),
            ["Places|Norway|Oslo"]
        );
        let dir = std::env::temp_dir().join(format!("glimt-test-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        std::fs::write(
            sidecar(&file),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject><rdf:Bag><rdf:li>Rose</rdf:li><rdf:li>rose</rdf:li></rdf:Bag></dc:subject>
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">Hello</rdf:li><rdf:li xml:lang="de">Hallo</rdf:li></rdf:Alt></dc:description>
  </rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        // The person and the flower stay two, and a tag and an added keyword leave both
        // comments.
        assert_eq!(read(&file).keywords, ["Rose", "rose"]);
        tag_file(&file, "Places|Oslo", true).unwrap();
        add_keywords(&file, &k(&["ROSE"])).unwrap();
        let x = std::fs::read_to_string(sidecar(&file)).unwrap();
        assert_eq!(read(&file).keywords, ["Rose", "rose", "Oslo"]);
        assert!(x.contains(">Hello<") && x.contains(">Hallo<"), "{x}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Saving an edited comment keeps keywords set in the background since it was read, and the
    /// tree paths inside an image count as the keywords there do.
    #[test]
    fn edits_keep_what_others_set() {
        let dir = std::env::temp_dir().join(format!("glimt-test-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"").unwrap();
        write_changed(&file, adding(&["a".into()]), None).unwrap();
        tag_file(&file, "b", true).unwrap();
        let meta = write_changed(&file, None, Some("c".into())).unwrap();
        assert_eq!(
            (meta.keywords, meta.comment),
            (vec!["a".into(), "b".into()], "c".into())
        );
        // A JPEG exported with its paths inside has them count until a sidecar has its own.
        let jpeg = dir.join("b.jpg");
        let mut base = Vec::new();
        let mut out = std::io::Cursor::new(&mut base);
        image::RgbImage::new(4, 2)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        let packet = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description rdf:about=\"\" \
             xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:lr=\"http://ns.adobe.com/lightroom/1.0/\">\
             <dc:subject><rdf:Bag><rdf:li>Oslo</rdf:li></rdf:Bag></dc:subject>\
             <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Oslo</rdf:li></rdf:Bag></lr:hierarchicalSubject>\
             </rdf:Description></rdf:RDF></x:xmpmeta>";
        let mut app1 = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        app1.extend(packet.as_bytes());
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend(((app1.len() + 2) as u16).to_be_bytes());
        bytes.extend(app1);
        bytes.extend(&base[2..]);
        std::fs::write(&jpeg, bytes).unwrap();
        assert_eq!(tree_paths(&jpeg), ["Places|Oslo"]);
        tag_file(&jpeg, "Events|Party", true).unwrap();
        assert_eq!(tree_paths(&jpeg), ["Places|Oslo", "Events|Party"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_follows_the_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.png"), dir.join("b.png"), dir.join("c.png"));
        image::RgbaImage::new(2, 2).save(&a).unwrap();
        assert_eq!(read(&a), KeywordsAndComment::default());
        let meta = KeywordsAndComment {
            keywords: vec!["x".into()],
            comment: "y".into(),
        };
        write_changed(&a, adding(&meta.keywords), Some(meta.comment.clone())).unwrap();
        assert_eq!(read(&a), meta);
        std::fs::rename(&a, &b).unwrap();
        follow(&a, &b, false, &Default::default(), &mut Default::default()).unwrap();
        assert_eq!(read(&b), meta);
        std::fs::copy(&b, &c).unwrap();
        follow(&b, &c, true, &Default::default(), &mut Default::default()).unwrap();
        assert_eq!(read(&c), meta);
        // A sidecar at the target belongs to another file, so it is kept, not replaced.
        std::fs::write(&a, b"").unwrap();
        let other = KeywordsAndComment {
            keywords: vec!["other".into()],
            comment: String::new(),
        };
        write_changed(&a, adding(&other.keywords), Some(other.comment.clone())).unwrap();
        assert!(follow(&b, &a, true, &Default::default(), &mut Default::default()).is_err());
        assert_eq!(read(&a), other);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Lightroom's `IMG_1.xmp` belongs to the RAW file `IMG_1.CR2`, also with an `IMG_1.JPG`
    /// beside it, but to neither of two RAW files of that name, nor to a DNG or a JPEG.
    #[test]
    fn adobe_sidecars_belong_to_their_raw_file_alone() {
        let dir = std::env::temp_dir().join(format!("glimt-test-adobe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = |name: &str| dir.join(name);
        let ours = std::collections::HashSet::new();
        for name in ["IMG_1.CR2", "IMG_1.JPG", "IMG_1.xmp", "IMG_1.CR2.xmp"] {
            std::fs::write(p(name), name).unwrap();
        }
        assert_eq!(
            sidecars_moving_with(
                &p("IMG_1.CR2"),
                &p("IMG_2.CR2"),
                &ours,
                &mut Default::default()
            ),
            [
                (p("IMG_1.CR2.xmp"), p("IMG_2.CR2.xmp")),
                (p("IMG_1.xmp"), p("IMG_2.xmp"))
            ]
        );
        assert!(sidecars_of(&[p("IMG_1.JPG")]).is_empty());
        std::fs::write(p("IMG_1.NEF"), "").unwrap();
        assert_eq!(sidecars_of(&[p("IMG_1.CR2")]), [p("IMG_1.CR2.xmp")]);
        for name in ["IMG_1.NEF", "IMG_1.CR2"] {
            std::fs::remove_file(p(name)).unwrap();
        }
        std::fs::write(p("IMG_1.dng"), "").unwrap();
        assert!(sidecars_of(&[p("IMG_1.dng")]).is_empty());
        // A file gone (a delete, a move) still takes it, as nothing else of its name is RAW.
        assert_eq!(
            sidecars_of(&[p("IMG_1.CR2")]),
            [p("IMG_1.CR2.xmp"), p("IMG_1.xmp")]
        );
        // `IMG_3.XMP` is Adobe's too and keeps its case, and a RAW file of the stem in mixed
        // case (`IMG_4.Nef`) is another RAW file.
        for name in [
            "IMG_3.CR2",
            "IMG_3.XMP",
            "IMG_4.CR2",
            "IMG_4.Nef",
            "IMG_4.xmp",
        ] {
            std::fs::write(p(name), name).unwrap();
        }
        assert_eq!(
            sidecars_moving_with(
                &p("IMG_3.CR2"),
                &p("IMG_5.CR2"),
                &ours,
                &mut Default::default()
            ),
            [(p("IMG_3.XMP"), p("IMG_5.XMP"))]
        );
        assert!(sidecars_of(&[p("IMG_4.CR2")]).is_empty());
        // A RAW file that an operation brings to the stem counts as another one.
        let mut arriving = AdobeNames::arriving([p("IMG_3.NEF"), p("IMG_3.CR2")]);
        assert!(adobe_sidecars_of(&p("IMG_3.CR2"), &ours, &mut arriving).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A sidecar that is a link is not read, because a downloaded folder could point it at
    /// another file of the user's. Neither the search nor the panel reads through it.
    #[test]
    fn a_linked_sidecar_is_not_read() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-linked-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (other, a) = (dir.join("other.png"), dir.join("a.png"));
        image::RgbaImage::new(2, 2).save(&other).unwrap();
        image::RgbaImage::new(2, 2).save(&a).unwrap();
        write_changed(&other, adding(&["secret".into()]), None).unwrap();
        std::os::unix::fs::symlink(sidecar(&other), sidecar(&a)).unwrap();
        assert!(read(&a).keywords.is_empty(), "read through the link");
        assert!(
            read_all(&a).0.keywords.is_empty(),
            "the panel read through the link"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A sidecar that is a link (a downloaded folder pointing IMG.jpg.xmp at a private key) is
    /// not copied through, so a copy and a crop get none. Moved, the link goes as itself.
    #[test]
    fn a_linked_sidecar_is_not_copied_through() {
        let dir = std::env::temp_dir().join(format!("glimt-test-linked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (secret, a, b, c) = (
            dir.join("key"),
            dir.join("a.png"),
            dir.join("b.png"),
            dir.join("c.png"),
        );
        std::fs::write(&secret, "PRIVATE KEY").unwrap();
        image::RgbaImage::new(2, 2).save(&a).unwrap();
        std::os::unix::fs::symlink(&secret, sidecar(&a)).unwrap();
        std::fs::copy(&a, &b).unwrap();
        follow(&a, &b, true, &Default::default(), &mut Default::default()).unwrap();
        assert!(
            std::fs::symlink_metadata(sidecar(&b)).is_err(),
            "copied through the link"
        );
        crate::images::save::save_copy_with_sidecar(&a, &c, b"x", None, false).unwrap();
        assert!(
            std::fs::symlink_metadata(sidecar(&c)).is_err(),
            "landed through the link"
        );
        std::fs::rename(&a, b.with_file_name("d.png")).unwrap();
        follow(
            &a,
            &dir.join("d.png"),
            false,
            &Default::default(),
            &mut Default::default(),
        )
        .unwrap();
        assert!(is_link(&sidecar(&dir.join("d.png"))), "moved as itself");
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "PRIVATE KEY");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A link swapped in at a sidecar's name, again and again while it is read, is never read
    /// through. The old look at the name and the read after it were two steps, so a link put
    /// there between them showed another file of the user's as the photo's keywords.
    #[test]
    fn a_link_swapped_in_is_not_read_through() {
        let dir = std::env::temp_dir().join(format!("glimt-test-swapped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (secret, photo) = (dir.join("key"), dir.join("a.jpg"));
        let (file, link) = (dir.join("file"), dir.join("link"));
        std::fs::write(&secret, "PRIVATE KEY").unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let read_through = std::thread::scope(|s| {
            s.spawn(|| {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::fs::write(&file, "own").unwrap();
                    std::fs::rename(&file, sidecar(&photo)).unwrap();
                    std::os::unix::fs::symlink(&secret, &link).unwrap();
                    std::fs::rename(&link, sidecar(&photo)).unwrap();
                }
            });
            let start = std::time::Instant::now();
            let mut found = false;
            while !found && start.elapsed() < std::time::Duration::from_secs(2) {
                found = own_sidecar(&photo)
                    .unwrap()
                    .is_some_and(|b| b == b"PRIVATE KEY");
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            found
        });
        assert!(!read_through, "read through a link swapped in");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Sidecars broken in thousands of ways are read and written to without a panic.
    #[test]
    fn survives_broken_sidecars() {
        let meta = KeywordsAndComment {
            keywords: vec!["beach".into(), "Tom & Ann".into()],
            comment: "Summer".into(),
        };
        let empty = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                     rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let alt_text = AltText {
            text: "A dog".into(),
            extended_description: "On a beach".into(),
        };
        let seed = set_alt_text(
            &set_keywords_and_comment(empty, &meta, false).unwrap(),
            &alt_text,
            false,
        )
        .unwrap();
        for bytes in crate::support::test_support::broken(seed.as_bytes()) {
            let xmp = String::from_utf8_lossy(&bytes);
            parse(&xmp);
            set_keywords_and_comment(&xmp, &meta, true);
            set_keywords_and_comment(&xmp, &KeywordsAndComment::default(), false);
            parse_alt_text(&xmp);
            set_alt_text(&xmp, &AltText::default(), true);
            set_keywords(&xmp, &meta, true);
            element(&xmp, "dc:subject");
        }
    }

    /// Copy for Sharing reads a packet's properties, written as attributes or as elements, past
    /// a comment, with their namespaces, and writes a packet of them that reads back the same.
    #[test]
    fn reads_and_writes_properties() {
        let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                   rdf:about=\"\" xml:lang=\"en\" xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\" \
                   xmlns:dc='http://purl.org/dc/elements/1.1/' tiff:Make=\"Canon\">\
                   <!-- <dc:title>x</dc:title> --><dc:subject><rdf:Bag><rdf:li>a</rdf:li>\
                   </rdf:Bag></dc:subject><dc:rights/></rdf:Description></rdf:RDF></x:xmpmeta>";
        let read = super::properties(xmp);
        let names: Vec<&str> = read.list.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(names, ["tiff:Make", "dc:subject", "dc:rights"]);
        assert!(
            read.list[0]
                .2
                .iter()
                .any(|(p, u)| p == "dc" && u.contains("purl"))
        );
        let written = String::from_utf8(super::packet(&read)).unwrap();
        let again = super::properties(&written);
        let elements = |p: &super::Properties| -> Vec<(String, String)> {
            (p.list.iter())
                .map(|(n, e, _)| (n.clone(), e.clone()))
                .collect()
        };
        assert_eq!(elements(&again), elements(&read));
        assert!(written.contains("xmlns:tiff=") && !written.contains("xml:lang"));
    }

    /// A property is named by its namespace, not by the prefix the packet gave it, so Exif's GPS
    /// under "ns1:" is "exif:" and the old "xap:" is "xmp:".
    #[test]
    fn names_properties_by_their_namespace() {
        let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                   rdf:about=\"\" xmlns:ns1=\"http://ns.adobe.com/exif/1.0/\" \
                   xmlns:xap=\"http://ns.adobe.com/xap/1.0/\" xmlns:my=\"urn:my\" \
                   ns1:GPSLatitude=\"59,55N\" my:Thing=\"x\"><xap:CreateDate>2023\
                   </xap:CreateDate></rdf:Description></rdf:RDF></x:xmpmeta>";
        let read = super::properties(xmp);
        let names: Vec<&str> = read.list.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(names, ["exif:GPSLatitude", "my:Thing", "xmp:CreateDate"]);
        let written = String::from_utf8(super::packet(&read)).unwrap();
        assert!(written.contains("xmlns:ns1=\"http://ns.adobe.com/exif/1.0/\""));
        assert!(written.contains("<ns1:GPSLatitude>59,55N</ns1:GPSLatitude>"));
    }

    /// A known prefix bound to another namespace, or to none, names its property by that
    /// namespace, so Copy for Sharing does not take it for the known one and leaves it out with
    /// any group.
    #[test]
    fn a_known_prefix_of_another_namespace_is_not_known() {
        let xmp = "<rdf:Description rdf:about=\"\" xmlns:aux=\"http://example.com/x\" \
                   xmlns:exif=\"urn:other\" aux:SerialNumber=\"1\" exif:Make=\"x\" \
                   tiff:Model=\"y\"/>";
        let read = super::properties(xmp);
        let names: Vec<&str> = read.list.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "{http://example.com/x}SerialNumber",
                "{urn:other}Make",
                "{}Model"
            ]
        );
        for name in names {
            let groups = crate::images::shared_copy::Group::groups_of(name);
            assert_eq!(groups.len(), crate::images::shared_copy::Group::ALL.len());
        }
    }

    /// digiKam 4's keyword list, in the namespace that version wrote, is named as digiKam's, so
    /// Copy for Sharing leaves it out with Keywords only and keeps it with Place.
    #[test]
    fn digikam_4s_namespace_is_known() {
        let xmp = "<rdf:Description rdf:about=\"\" xmlns:digiKam=\"http://www.digikam.org/\">\
                   <digiKam:TagsList><rdf:Seq><rdf:li>Adrien</rdf:li></rdf:Seq>\
                   </digiKam:TagsList></rdf:Description>";
        let read = super::properties(xmp);
        assert_eq!(read.list[0].0, "digiKam:TagsList");
        let groups = crate::images::shared_copy::Group::groups_of(&read.list[0].0);
        assert_eq!(groups, [crate::images::shared_copy::Group::Keywords]);
    }

    /// A namespace declared with spaces around its "=", as XML allows, is read, so the
    /// orientation and an HDR gain map's properties keep the names a copy for sharing always
    /// keeps them by, and the copy's packet declares their namespaces.
    #[test]
    fn reads_a_namespace_with_spaces_around_its_equals_sign() {
        let xmp = "<rdf:Description rdf:about=\"\" \
                   xmlns:tiff = \"http://ns.adobe.com/tiff/1.0/\"\n\
                   xmlns:hdrgm\t=\t'http://ns.adobe.com/hdr-gain-map/1.0/' \
                   xmlns:Container =\"http://ns.google.com/photos/1.0/container/\" \
                   tiff:Orientation=\"6\" hdrgm:Version=\"1.0\"><Container:Directory/>\
                   </rdf:Description>";
        let read = super::properties(xmp);
        let names: Vec<&str> = read.list.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(
            names,
            ["tiff:Orientation", "hdrgm:Version", "Container:Directory"]
        );
        let written = String::from_utf8(super::packet(&read)).unwrap();
        assert!(written.contains("xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\""));
    }

    /// A name made from a namespace holds no control character, so it cannot add a line, and
    /// with it a word, to Copy for Sharing's saved choice.
    #[test]
    fn a_property_name_holds_no_control_character() {
        let xmp = "<rdf:Description xmlns:aux=\"u\nkeep:GPSLatitude\n\" aux:A=\"1\"/>";
        let read = super::properties(xmp);
        let names: Vec<&str> = read.list.iter().map(|(n, ..)| n.as_str()).collect();
        assert_eq!(names.len(), 1);
        assert!(!names[0].chars().any(char::is_control), "{names:?}");
    }

    /// Packets broken in thousands of ways: their properties read, and a packet written of them,
    /// without a panic.
    #[test]
    fn survives_broken_xmp_properties() {
        let seed = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                    rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
                    dc:format='image/jpeg'><!--c--><dc:subject><rdf:Bag><rdf:li>a</rdf:li>\
                    <rdf:li><rdf:Description dc:x='1' dc:y=\"2\"/></rdf:li><dc:z><dc:z>3\
                    </dc:z></dc:z></rdf:Bag></dc:subject><dc:rights/></rdf:Description>\
                    <rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        // A text of letters of two, three and four bytes, longer than the 1 KB that a namespace
        // is read in, so a cut lands inside a letter.
        let text = "Ålesund bæt 東京 \u{1f30a} ".repeat(80);
        let long = seed.replace(
            "<!--c-->",
            &format!("<dc:description>{text}</dc:description>"),
        );
        assert!(long.len() > 2048);
        for bytes in crate::support::test_support::broken(seed.as_bytes())
            .chain(crate::support::test_support::broken(long.as_bytes()))
        {
            let read = super::properties(&String::from_utf8_lossy(&bytes));
            super::packet(&read);
            for (name, element, namespaces) in &read.list {
                super::with_own_value_only(element, namespaces, &|n| n != name);
            }
        }
    }

    /// A GPS field inside a kept keyword list is known by its namespace, not by its prefix's
    /// text: also in a default namespace, and with the prefix dc bound to Exif's namespace
    /// inside the list, while the packet binds dc to Dublin Core.
    #[test]
    fn a_name_inside_a_property_is_read_by_its_namespace() {
        let namespaces = [
            ("dc", "http://purl.org/dc/elements/1.1/"),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ]
        .map(|(p, u)| (p.to_owned(), u.to_owned()));
        let keeps = |name: &str| name != "exif:GPSLatitude";
        for inside in [
            "<GPSLatitude xmlns=\"http://ns.adobe.com/exif/1.0/\">59N</GPSLatitude>",
            "<rdf:Description xmlns:dc=\"http://ns.adobe.com/exif/1.0/\" dc:GPSLatitude=\"59N\"/>",
        ] {
            let element = format!(
                "<dc:subject><rdf:Bag><rdf:li>Oslo</rdf:li><rdf:li>{inside}</rdf:li>\
                 </rdf:Bag></dc:subject>"
            );
            let kept = super::with_own_value_only(&element, &namespaces, &keeps);
            assert!(!kept.contains("59N"), "{kept}");
            assert!(kept.contains("<rdf:li>Oslo</rdf:li>"), "{kept}");
            assert!(kept.ends_with("</dc:subject>"), "{kept}");
        }
    }

    /// A property whose value does not read as balanced XML is left out whole, so the copy's
    /// packet stays XML, and a CDATA section with a quote is kept as the text it is.
    #[test]
    fn a_property_that_is_not_balanced_is_left_out_whole() {
        let namespaces = [
            ("dc", "http://purl.org/dc/elements/1.1/"),
            ("exif", "http://ns.adobe.com/exif/1.0/"),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ]
        .map(|(p, u)| (p.to_owned(), u.to_owned()));
        let keeps = |name: &str| name != "exif:GPSLatitude";
        for broken in [
            "<dc:subject><rdf:Bag><rdf:li><exif:GPSLatitude>59N</rdf:li></rdf:Bag></dc:subject>",
            "<dc:subject><rdf:Bag><rdf:li>Oslo</rdf:Bag></dc:subject>",
            "<dc:subject><rdf:Bag><rdf:li>Oslo</rdf:li><!-- not closed",
            "<dc:subject><rdf:Bag><rdf:li xml:lang='x",
        ] {
            assert_eq!(super::with_own_value_only(broken, &namespaces, &keeps), "");
        }
        let cdata = "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">\
                     <![CDATA[It's <b>bold</b>]]></rdf:li></rdf:Alt></dc:description>";
        assert_eq!(
            super::with_own_value_only(cdata, &namespaces, &keeps),
            cdata
        );
    }

    /// A packet whose text puts a letter of two bytes across the 1 KB after a namespace is read
    /// without a panic, also with the cut one byte later.
    #[test]
    fn reads_a_namespace_before_long_text() {
        for pad in ["", "a"] {
            let xmp = format!(
                "<rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
                 <dc:title>{pad}{}</dc:title></rdf:Description>",
                "ø".repeat(700)
            );
            let read = super::properties(&xmp);
            assert_eq!(read.list.len(), 1, "{pad:?}");
        }
    }

    /// The embedded packet and the sidecar may bind one prefix to two namespaces. Each property
    /// is written with the namespace of the packet it came from.
    #[test]
    fn keeps_each_packets_own_namespaces() {
        let packet = |uri: &str, name: &str| {
            format!("<rdf:Description rdf:about=\"\" xmlns:my=\"{uri}\" my:{name}=\"x\"/>")
        };
        let mut merged = super::properties(&packet("urn:a", "One"));
        merged.add_over(super::properties(&packet("urn:b", "Two")));
        let written = String::from_utf8(super::packet(&merged)).unwrap();
        let declares = |name: &str, uri: &str| {
            (written.split("<rdf:Description"))
                .find(|d| d.contains(&format!("<my:{name}>")))
                .is_some_and(|d| d.contains(&format!("xmlns:my=\"{uri}\"")))
        };
        assert!(
            declares("One", "urn:a") && declares("Two", "urn:b"),
            "{written}"
        );
    }

    /// Keyword tree paths are trimmed around each level, sorted and kept once.
    #[test]
    fn cleans_keyword_paths() {
        let paths = [
            " places | Norway|Oslo ",
            "Animals",
            "",
            "Places||x|",
            "Animals",
        ];
        let got = super::keyword_paths(paths.into_iter());
        assert_eq!(got, ["Animals", "places|Norway|Oslo", "Places|x"]);
    }

    /// The keyword tree changed by one window keeps what another added. Removing a level takes
    /// the levels below it.
    #[test]
    fn changes_the_keyword_tree_alone() {
        let on_disk = [
            "Animals".to_string(),
            "Places|Oslo".into(),
            "People|Kari".into(),
        ];
        let added = super::keyword_tree_changed(&on_disk, vec!["Places|Bergen".into()], None);
        assert_eq!(
            added,
            ["Animals", "People|Kari", "Places|Bergen", "Places|Oslo"]
        );
        let removed = super::keyword_tree_changed(&added, Vec::new(), Some("Places".into()));
        assert_eq!(removed, ["Animals", "People|Kari"]);
    }

    /// A line of the keyword tree file that is not UTF-8 keeps its bytes when the tree is
    /// changed, and the other lines still show. Before, it became U+FFFD at the next change,
    /// and at the start the whole tree showed empty.
    #[test]
    fn the_keyword_tree_file_keeps_bytes_that_are_not_utf8() {
        let file = b"Places|Oslo\nCaf\xe9\nAnimals\n";
        assert_eq!(super::keyword_tree_of(file), ["Animals", "Places|Oslo"]);
        let (written, tree) = super::keyword_tree_file_changed(file, vec!["People".into()], None);
        assert_eq!(tree, ["Animals", "People", "Places|Oslo"]);
        assert_eq!(written, b"Animals\nPeople\nPlaces|Oslo\nCaf\xe9\n");
    }

    /// A keyword with a line break is not put into the keyword tree, whose file would hold it
    /// as two lines and so as two paths.
    #[test]
    fn a_keyword_with_a_line_break_stays_out_of_the_tree() {
        let added = vec!["Places|Os\nlo".into(), "Animals".into()];
        let (written, tree) = super::keyword_tree_file_changed(b"", added, None);
        assert_eq!(tree, ["Animals"]);
        assert_eq!(written, b"Animals\n");
    }
}
