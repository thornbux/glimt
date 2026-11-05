//! Keywords and comment, as Geeqie's info sidebar edits them, which are dc:subject and
//! dc:description in XMP. It also handles IPTC's alt text and extended description, and the
//! keywords' tree paths (lr:hierarchicalSubject). They are written to the sidecar
//! `<file>.xmp`, which darktable and digiKam read too, and read from it, or else from the XMP
//! inside the image. `update_sidecar` writes the stars too, and `follow` takes sidecars along
//! when their files move.

use std::path::{Path, PathBuf};

const ALT: &str = "Iptc4xmpCore:AltTextAccessibility";

const EXTENDED: &str = "Iptc4xmpCore:ExtDescrAccessibility";

/// The 64 locks within this Glimt, by `lock_slot`.
static SLOTS: [std::sync::Mutex<()>; 64] = [const { std::sync::Mutex::new(()) }; 64];

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Meta {
    pub keywords: Vec<String>,
    pub comment: String,
}

impl Meta {
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

/// IPTC's fields for accessibility (Photo Metadata 2021), in XMP. They are a short text for
/// someone who cannot see the picture, and a longer description of it. Web pages, publishing
/// tools and screen readers use them. They are kept apart from the keywords and comment, which
/// Geeqie edits.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Access {
    pub alt: String,
    pub extended: String,
}

/// A sidecar's lock while it is held. It holds the lock file, if it could be made, and the
/// mutex of its slot.
pub(crate) type Held = (Option<std::fs::File>, std::sync::MutexGuard<'static, ()>);

fn parse_access(xmp: &str) -> Access {
    let first = |tag| items(xmp, tag).into_iter().next().unwrap_or_default();
    Access {
        alt: first(ALT),
        extended: first(EXTENDED),
    }
}

/// `read` and `tree_paths` of `path`, and its alt text and extended description (from its
/// sidecar if that has them, and otherwise from the image), for the panel. Its sidecar and the
/// XMP inside it are each read once, because on a network share each reading of a large photo
/// costs.
pub fn read_all(path: &Path) -> (Meta, Vec<String>, Access) {
    let side = own_sidecar_text(path);
    let inside = std::cell::OnceCell::new();
    let inside = || inside.get_or_init(|| embedded(path)).clone();
    let has_meta = |x: &String| x.contains("<dc:subject") || x.contains("<dc:description");
    let has_access =
        |x: &String| x.contains(&format!("<{ALT}")) || x.contains(&format!("<{EXTENDED}"));
    let source = side.clone().filter(has_meta).or_else(inside);
    let access = side.filter(has_access).or_else(inside);
    (
        source.as_deref().map_or_else(Meta::default, parse),
        source.map_or_else(Vec::new, |x| unique(items(&x, "lr:hierarchicalSubject"))),
        access.map_or_else(Access::default, |x| parse_access(&x)),
    )
}

/// Writes the alt text and extended description of `path` to its sidecar, and everything else
/// in it stays. Fields the image holds itself and that were cleared are written empty, as the
/// comment is.
pub fn write_access(path: &Path, access: &Access) -> Result<(), String> {
    let _held = held(path);
    let own = embedded(path).is_some_and(|x| parse_access(&x) != Access::default());
    update(path, *access == Access::default() && !own, |x| {
        set_access(x, access, own)
    })
}

fn set_access(xmp: &str, access: &Access, say_empty: bool) -> Option<String> {
    let xmp = without(&without(xmp, ALT), EXTENDED);
    let mut add = String::new();
    for (tag, text) in [(ALT, &access.alt), (EXTENDED, &access.extended)] {
        if !text.is_empty() {
            add += &format!(
                "\n   <{tag}>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </{tag}>",
                escape(text)
            );
        } else if say_empty {
            add += &format!("\n   <{tag}>\n    <rdf:Alt/>\n   </{tag}>");
        }
    }
    insert(
        xmp,
        &add,
        "Iptc4xmpCore",
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
    )
}

/// Whether two keywords are the same one. Letter case does not count ("Oslo" and "oslo").
pub fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The keywords without repeats, each as first written.
fn unique(keywords: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in keywords {
        if !out.iter().any(|o| same(o, &k)) {
            out.push(k);
        }
    }
    out
}

/// The keyword of a keyword tree path, which is its last level.
fn leaf(tag: &str) -> &str {
    tag.rsplit('|').next().unwrap_or(tag)
}

/// Whether an image with `keywords` and keyword tree `paths` has `tag`. It must have the
/// keyword, and the path too unless no path ends in that keyword (as when a program that
/// writes keywords only set it).
pub fn has_tag(keywords: &[String], paths: &[String], tag: &str) -> bool {
    let name = leaf(tag);
    keywords.iter().any(|k| same(k, name))
        && (paths.iter().any(|p| same(p, tag)) || !paths.iter().any(|p| same(leaf(p), name)))
}

/// Set keyword tree path `tag` on or off in `keywords` and `paths`. Its keyword is added once,
/// and taken off only when no other path ends in it ("Events|Oslo" off keeps Oslo for
/// "Places|Norway|Oslo").
pub fn set_tag(keywords: &mut Vec<String>, paths: &mut Vec<String>, tag: &str, on: bool) {
    let name = leaf(tag);
    paths.retain(|p| !same(p, tag));
    if on {
        paths.push(tag.to_owned());
        if !keywords.iter().any(|k| same(k, name)) {
            keywords.push(name.to_owned());
        }
    } else if !paths.iter().any(|p| same(leaf(p), name)) {
        keywords.retain(|k| !same(k, name));
    }
}

/// The keyword tree paths (lr:hierarchicalSubject) in the sidecar of `path`.
pub fn tree_paths(path: &Path) -> Vec<String> {
    // The paths come from where `read` takes the keywords, the sidecar or else the XMP inside
    // the image.
    source(path).map_or_else(Vec::new, |x| unique(items(&x, "lr:hierarchicalSubject")))
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
/// whose contents would then show for this photo or be copied with it.
pub(crate) fn own_sidecar(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let side = sidecar(path);
    match linked(&side) {
        true => Ok(None),
        false => crate::files::file_io::read_or_none(&side),
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
pub fn read(path: &Path) -> Meta {
    source(path).map_or_else(Meta::default, |x| parse(&x))
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

/// Writes the fields that were edited (`None` means as it is on disk). They are read and
/// written as one change, so keywords set in the background meanwhile stay, and everything else
/// in the sidecar stays too. Keywords or a comment the image holds itself and that were cleared
/// are written as empty elements, because if they were left out, the image's own would be read
/// again. Returns the keywords and comment as written.
pub fn write_changed(
    path: &Path,
    keywords: Option<Vec<String>>,
    comment: Option<String>,
) -> Result<Meta, String> {
    let _held = held(path);
    let mut meta = read(path);
    let keywords_only = comment.is_none();
    if let Some(k) = keywords {
        meta.keywords = k;
    }
    if let Some(c) = comment {
        meta.comment = c;
    }
    let own = embedded(path).is_some_and(|x| parse(&x) != Meta::default());
    update(
        path,
        meta == Meta::default() && !own,
        |x| match keywords_only {
            true => set_keywords(x, &meta, own),
            false => set(x, &meta, own),
        },
    )?;
    Ok(meta)
}

/// Adds each of `keywords` that `have` lacks to it, at its end.
pub fn add_to(have: &mut Vec<String>, keywords: &[String]) {
    for k in keywords {
        if !have.iter().any(|m| same(m, k)) {
            have.push(k.clone());
        }
    }
}

/// Add `keywords` to those `path` has, read and written as one change.
pub fn add_keywords(path: &Path, keywords: &[String]) -> Result<(), String> {
    let _held = held(path);
    let mut meta = read(path);
    add_to(&mut meta.keywords, keywords);
    update(path, meta == Meta::default(), |x| {
        set_keywords(x, &meta, false)
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
pub(crate) fn held(path: &Path) -> Held {
    held_in(locks().as_deref(), path)
}

fn held_in(dir: Option<&Path>, path: &Path) -> Held {
    let side = sidecar(path);
    let guard = SLOTS[lock_slot(&side) as usize]
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    (dir.and_then(|d| lock_file(d, &side)), guard)
}

/// `held` for many files at once, as Rename Many and a delete move a photo and then its
/// sidecar. Each lock is taken once, because taking one twice would wait for itself. They are
/// taken in the order of their numbers, which every Glimt and every thread takes them in, so
/// two of these never wait for each other.
pub(crate) fn held_all<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Vec<Held> {
    held_all_in(locks().as_deref(), paths)
}

fn held_all_in<'a>(dir: Option<&Path>, paths: impl IntoIterator<Item = &'a PathBuf>) -> Vec<Held> {
    let mut sides: Vec<(u64, PathBuf)> = (paths.into_iter())
        .map(|p| sidecar(p))
        .map(|s| (lock_slot(&s), s))
        .collect();
    sides.sort_by_key(|s| s.0);
    sides.dedup_by_key(|s| s.0);
    (sides.iter())
        .map(|(n, side)| {
            let guard = SLOTS[*n as usize].lock().unwrap_or_else(|e| e.into_inner());
            (dir.and_then(|d| lock_file(d, side)), guard)
        })
        .collect()
}

/// The folder of the lock files, where there is a runtime folder.
fn locks() -> Option<PathBuf> {
    crate::features::remote::dir().map(|d| d.join("sidecar-locks"))
}

fn lock_file(dir: &Path, side: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir);
    #[allow(clippy::disallowed_methods)] // an empty lock file in the private runtime folder
    let file = std::fs::File::create(dir.join(lock_slot(side).to_string())).ok()?;
    file.lock().ok()?;
    Some(file)
}

/// Which of the 64 lock files a sidecar has. It is FNV-1a of its path's bytes, the same in
/// every Glimt, also one built with another Rust, whose own hasher may differ.
pub(crate) fn lock_slot(side: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let fnv = (side.as_os_str().as_bytes().iter()).fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    });
    fnv % 64
}

/// Changes the sidecar of `path` with `change`, or a new, empty sidecar unless `only_if_there`,
/// in which case a file without a sidecar keeps having none. It is written to a temporary file
/// first, so a crash never leaves half a sidecar.
#[cfg_attr(not(feature = "marks"), allow(dead_code))]
pub fn update_sidecar(
    path: &Path,
    only_if_there: bool,
    change: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    let _held = held(path);
    update(path, only_if_there, change)
}

fn update(
    path: &Path,
    only_if_there: bool,
    change: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    // When the image was moved, renamed or deleted while this waited, no sidecar of its old
    // name is made, and it does not count as done.
    if std::fs::symlink_metadata(path).is_err() {
        return Err(
            crate::support::lang::tr("the image was moved, renamed or deleted meanwhile").into(),
        );
    }
    let side = sidecar(path);
    // A link is not written through, because a downloaded folder may point IMG.jpg.xmp at
    // another file of the user's, which a keyword added here would then change.
    if linked(&side) {
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
    crate::files::file_io::replace_name(&side, text).map_err(|e| e.to_string())
}

/// Returns the sidecar of `path` when there is one, as a name in the folder. A link counts, also
/// one that points nowhere, so that it goes with its file rather than stay behind under the old
/// name for the next file of that name. Whether it may be read or copied is for `linked` to say.
/// Every operation that moves, renames or deletes a file asks this.
pub(crate) fn sidecar_of(path: &Path) -> Option<PathBuf> {
    let side = sidecar(path);
    std::fs::symlink_metadata(&side).is_ok().then_some(side)
}

/// Whether `path` is a link. A sidecar that is one is never read or written through, because a
/// downloaded folder may point it at any file of the user's (a private key).
pub(crate) fn linked(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Copy or move the sidecar of `from`, if it has one, with the file that went to `to`.
pub fn follow(from: &Path, to: &Path, copy: bool) -> std::io::Result<()> {
    let (side, to) = (sidecar(from), sidecar(to));
    // A link is no sidecar to copy. Moved, it goes as itself on the same disk, or it stays.
    if linked(&side) {
        return match copy {
            true => Ok(()),
            false => crate::files::file_io::rename_never_over(&side, &to).or(Ok(())),
        };
    }
    if sidecar_of(from).is_none() {
        return Ok(());
    }
    // The sidecar goes as the file went. It is on disk before the original goes, and never
    // over a sidecar there, which belongs to another file or is a copy kept on purpose. A
    // sidecar that cannot be moved (someone else's in a shared folder) is copied, so the
    // keywords go with the photo.
    match crate::files::file_io::put_file(&side, &to, copy) {
        Err(_) if !copy => crate::files::file_io::put_file(&side, &to, true),
        done => done,
    }
}

fn parse(xmp: &str) -> Meta {
    Meta {
        keywords: items(xmp, "dc:subject"),
        comment: items(xmp, "dc:description")
            .into_iter()
            .next()
            .unwrap_or_default(),
    }
}

/// The texts of the rdf:li items of element `tag`.
fn items(xmp: &str, tag: &str) -> Vec<String> {
    let Some(start) = xmp.find(&format!("<{tag}")) else {
        return Vec::new();
    };
    let end = xmp[start..]
        .find(&format!("</{tag}>"))
        .map_or(xmp.len(), |e| start + e);
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
        out.push(unescape(&body[..close]));
        rest = &body[close..];
    }
    out
}

/// `set` for a change of keywords only. A comment the sidecar has stays as it is, in all its
/// languages. A comment only inside the image is copied into a new sidecar, which would hide it
/// otherwise.
fn set_keywords(xmp: &str, meta: &Meta, say_empty: bool) -> Option<String> {
    let Some(original) = element(xmp, "dc:description") else {
        return set(xmp, meta, say_empty);
    };
    let comment = items(xmp, "dc:description")
        .into_iter()
        .next()
        .unwrap_or_default();
    let keep = Meta {
        keywords: meta.keywords.clone(),
        comment,
    };
    let new = set(xmp, &keep, say_empty)?;
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

/// The whole element `tag` in `xmp`, from its start tag to its end tag.
fn element(xmp: &str, tag: &str) -> Option<String> {
    let start = xmp.find(&format!("<{tag}"))?;
    let close = format!("</{tag}>");
    let end = start + xmp[start..].find(&close)? + close.len();
    Some(xmp[start..end].to_owned())
}

/// `xmp` with the keywords and comment of `meta`, in its first rdf:Description. Empty ones are
/// written as empty elements when `say_empty` is set. Returns `None` if `xmp` has no
/// rdf:Description.
fn set(xmp: &str, meta: &Meta, say_empty: bool) -> Option<String> {
    let xmp = without(&without(xmp, "dc:subject"), "dc:description");
    let mut add = String::new();
    if meta.keywords.is_empty() && say_empty {
        add += "\n   <dc:subject>\n    <rdf:Bag/>\n   </dc:subject>";
    }
    if meta.comment.is_empty() && say_empty {
        add += "\n   <dc:description>\n    <rdf:Alt/>\n   </dc:description>";
    }
    if !meta.keywords.is_empty() {
        add += "\n   <dc:subject>\n    <rdf:Bag>\n";
        for k in &meta.keywords {
            add += &format!("     <rdf:li>{}</rdf:li>\n", escape(k));
        }
        add += "    </rdf:Bag>\n   </dc:subject>";
    }
    if !meta.comment.is_empty() {
        add += &format!(
            "\n   <dc:description>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </dc:description>",
            escape(&meta.comment)
        );
    }
    let xmp = insert(xmp, &add, "dc", "http://purl.org/dc/elements/1.1/")?;
    let paths = tree_paths_for(&meta.keywords, items(&xmp, "lr:hierarchicalSubject"));
    with_paths(xmp, &paths)
}

/// The keyword tree paths for `keywords`, as Lightroom writes them. They are those of `paths`
/// that end in one of them, and each keyword without one as a path of its own (Immich, which
/// shows tags, reads only these). A keyword's own path goes when a longer one ends in it.
fn tree_paths_for(keywords: &[String], paths: Vec<String>) -> Vec<String> {
    let kept: Vec<String> = (unique(paths).into_iter())
        .filter(|p| keywords.iter().any(|k| same(k, leaf(p))))
        .collect();
    // Lightroom writes the levels above a keyword as keywords too, Places, Norway and Oslo for
    // Places|Norway|Oslo. They are in that path, not paths of their own.
    let in_path = |p: &str, name: &str| p.split('|').any(|level| same(level, name));
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

/// `xmp` with lr:hierarchicalSubject holding `paths`, or without it if there are none.
fn with_paths(xmp: String, paths: &[String]) -> Option<String> {
    let xmp = without(&xmp, "lr:hierarchicalSubject");
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
    let start = xmp.find("<rdf:Description")?;
    let end = start + xmp[start..].find('>')?;
    // The prefix must be declared on this Description or above it. exiftool gives each
    // namespace a Description of its own, and a prefix declared on a later one is not in force
    // here.
    let ns = if xmp[..end].contains(&format!("xmlns:{prefix}=")) {
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
    let _held = held(path);
    let mut meta = read(path);
    let mut paths = tree_paths(path);
    set_tag(&mut meta.keywords, &mut paths, tag, on);
    let own = embedded(path).is_some_and(|x| parse(&x) != Meta::default());
    update(path, !on && meta == Meta::default() && !own, |x| {
        set_keywords(&with_paths(x.to_owned(), &paths)?, &meta, own)
    })
}

/// Sets the keyword tree paths `tags` on `path` in one write, as `tag` sets one. Each name goes
/// among its keywords and each path in lr:hierarchicalSubject, beside what it has, which stays.
/// Nothing is written when it has them all.
pub fn add_tags(path: &Path, tags: &[String]) -> Result<(), String> {
    let _held = held(path);
    let mut meta = read(path);
    let mut paths = tree_paths(path);
    let (keywords, known) = (meta.keywords.clone(), paths.clone());
    for t in tags {
        set_tag(&mut meta.keywords, &mut paths, t, true);
    }
    if meta.keywords == keywords && paths == known {
        return Ok(());
    }
    let own = embedded(path).is_some_and(|x| parse(&x) != Meta::default());
    update(path, false, |x| {
        set_keywords(&with_paths(x.to_owned(), &paths)?, &meta, own)
    })
}

/// `xmp` without element `tag` and its contents.
fn without(xmp: &str, tag: &str) -> String {
    let close = format!("</{tag}>");
    match (xmp.find(&format!("<{tag}>")), xmp.find(&close)) {
        (Some(a), Some(b)) if b > a => {
            // With the white space before it, so the sidecar does not gather empty lines.
            let a = xmp[..a].trim_end().len();
            format!("{}{}", &xmp[..a], &xmp[b + close.len()..])
        }
        _ => xmp.to_owned(),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
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
/// each once.
pub(crate) fn keyword_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = paths
        .map(|p| {
            p.split('|')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("|")
        })
        .filter(|p| !p.is_empty())
        .collect();
    // A path in another letter case is the same path, and the first spelling stays.
    out.sort_by_key(|p| p.to_lowercase());
    out.dedup_by(|a, b| same(a, b));
    out
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
    /// A keyword written while the photo is moved, which waited for the move, lands on no old
    /// name. No sidecar is left there without its photo, for a later file of that name.
    #[test]
    fn a_write_that_waits_for_a_move_leaves_no_sidecar() {
        let dir = std::env::temp_dir().join(format!("glimt-test-meta-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("to")).unwrap();
        let (from, to) = (dir.join("IMG.jpg"), dir.join("to/IMG.jpg"));
        std::fs::write(&from, b"photo").unwrap();
        let move_held = super::held(&from);
        let writer = {
            let from = from.clone();
            std::thread::spawn(move || super::add_keywords(&from, &["Oslo".to_owned()]))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        crate::files::file_io::put_file(&from, &to, false).unwrap();
        super::follow(&from, &to, false).unwrap();
        drop(move_held);
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

    /// The lock file of a sidecar is the same in every Glimt, by FNV-1a, as known for these
    /// paths.
    #[test]
    fn lock_slots_stay() {
        use std::path::Path;
        assert_eq!(super::lock_slot(Path::new("")), 0xcbf2_9ce4_8422_2325 % 64);
        assert_eq!(super::lock_slot(Path::new("a")), 0xaf63_dc4c_8601_ec8c % 64);
    }

    /// Within one Glimt, a holder whose lock file could not be made waits for one whose could,
    /// and the other way round, because neither goes by another lock than the other.
    #[test]
    fn a_failed_lock_file_still_waits_in_this_glimt() {
        let dir = std::env::temp_dir().join(format!("glimt-test-slots-{}", std::process::id()));
        let photo = std::path::PathBuf::from("/photos/slot.jpg");
        for (first, then) in [(Some(dir.as_path()), None), (None, Some(dir.as_path()))] {
            let held = super::held_in(first, &photo);
            let (tx, rx) = std::sync::mpsc::channel();
            let (dir2, p) = (then.map(|d| d.to_path_buf()), photo.clone());
            let waiter = std::thread::spawn(move || {
                let all = super::held_all_in(dir2.as_deref(), [&p]);
                tx.send(()).unwrap();
                drop(all);
            });
            let waited = rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err();
            drop(held);
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
        let (meta, paths, access) = super::read_all(&img);
        assert_eq!(meta.keywords, super::read(&img).keywords);
        assert_eq!(paths, super::tree_paths(&img));
        assert_eq!(paths, ["Places|Oslo"]);
        assert_eq!(access, super::Access::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;

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
        let meta = Meta {
            keywords: vec!["x".into()],
            comment: String::new(),
        };
        assert!(
            write_changed(
                &file,
                Some(meta.keywords.clone()),
                Some(meta.comment.clone())
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(sidecar(&file)).unwrap(),
            b"\xff\xfe not UTF-8"
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
        let written = write_changed(&file, Some(vec!["x".into()]), None);
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
        assert!(write_changed(&file, Some(vec!["x".into()]), None).is_err());
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
        let meta = Meta {
            keywords: vec!["x".into()],
            comment: String::new(),
        };
        let written = set(exiftool, &meta, false).unwrap();
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
            items(
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
            Meta::parse_keywords("Oslo, beach, oslo,, beach"),
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
        let meta = Meta {
            keywords: vec!["beach".into()],
            comment: String::new(),
        };
        let xmp = set(r#"<rdf:Description rdf:about="">"#, &meta, false).unwrap();
        assert_eq!(items(&xmp, "lr:hierarchicalSubject"), ["beach"]);
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
        let own = Meta {
            keywords: vec!["beach".into()],
            comment: String::new(),
        };
        let empty = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                     rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let packet = set(empty, &own, false).unwrap();
        let mut app1 = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        app1.extend(packet.as_bytes());
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend(((app1.len() + 2) as u16).to_be_bytes());
        bytes.extend(app1);
        bytes.extend(&base[2..]);
        std::fs::write(&file, bytes).unwrap();
        assert_eq!(read(&file), own);
        write_changed(&file, Some(Vec::new()), Some(String::new())).unwrap();
        assert_eq!(read(&file), Meta::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keywords_and_comment_round_trip() {
        let meta = Meta {
            keywords: vec!["beach".into(), "Tom & Ann".into()],
            comment: "Summer <2024>\nsecond line".into(),
        };
        // A darktable sidecar has an attribute-only Description with a rating, which is kept as
        // it is.
        let dt = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description rdf:about=\"\" \
                  xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>";
        let written = set(dt, &meta, false).unwrap();
        assert!(written.contains("xmp:Rating=\"3\" xmlns:dc="));
        assert_eq!(parse(&written), meta);
        // Written again with other values, they are replaced, not added.
        let other = Meta {
            keywords: vec!["snow".into()],
            comment: String::new(),
        };
        let again = set(&written, &other, false).unwrap();
        assert_eq!(parse(&again), other);
        assert_eq!(again.matches("<dc:subject>").count(), 1);
        assert_eq!(again.matches("xmlns:dc=").count(), 1);
        // With everything removed, the sidecar is as it was, but for the name space.
        let none = set(&again, &Meta::default(), false).unwrap();
        assert_eq!(parse(&none), Meta::default());
        assert!(none.contains("xmp:Rating=\"3\""));
        assert_eq!(unescape("a&#x41;&#66;&bogus c&amp;"), "aAB&bogus c&");
        assert_eq!(
            Meta::parse_keywords(" a, b,,a ,c "),
            ["a", "b", "c"].map(String::from)
        );
    }

    /// The IPTC alt text and extended description are written, read back, replaced and
    /// cleared, and the keywords, comment and other fields are kept.
    #[test]
    fn alt_text_round_trip() {
        let meta = Meta {
            keywords: vec!["beach".into()],
            comment: "Summer".into(),
        };
        let empty = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                     rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let with_meta = set(empty, &meta, false).unwrap();
        let access = Access {
            alt: "A dog <on> a beach".into(),
            extended: "Long & detailed".into(),
        };
        let written = set_access(&with_meta, &access, false).unwrap();
        assert_eq!(parse_access(&written), access);
        assert_eq!(parse(&written), meta);
        assert_eq!(written.matches("xmlns:Iptc4xmpCore=").count(), 1);
        let other = Access {
            alt: "A cat".into(),
            extended: String::new(),
        };
        let again = set_access(&written, &other, false).unwrap();
        assert_eq!(parse_access(&again), other);
        assert_eq!(again.matches("AltTextAccessibility>").count(), 2);
        let none = set_access(&again, &Access::default(), false).unwrap();
        assert_eq!(parse_access(&none), Access::default());
        assert_eq!(parse(&none), meta);
        // The image's own fields that are cleared are written empty, so they are not read from
        // the image again.
        assert!(
            set_access(&none, &Access::default(), true)
                .unwrap()
                .contains("<rdf:Alt/>")
        );
        // Keywords written after the alt text leave it.
        assert_eq!(
            parse_access(&set(&written, &Meta::default(), false).unwrap()),
            access
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
        super::write_changed(&photo, Some(vec!["Mine".into()]), None).unwrap();
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
        write_changed(&file, Some(vec!["a".into()]), None).unwrap();
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
        assert_eq!(read(&a), Meta::default());
        let meta = Meta {
            keywords: vec!["x".into()],
            comment: "y".into(),
        };
        write_changed(&a, Some(meta.keywords.clone()), Some(meta.comment.clone())).unwrap();
        assert_eq!(read(&a), meta);
        std::fs::rename(&a, &b).unwrap();
        follow(&a, &b, false).unwrap();
        assert_eq!(read(&b), meta);
        std::fs::copy(&b, &c).unwrap();
        follow(&b, &c, true).unwrap();
        assert_eq!(read(&c), meta);
        // A sidecar at the target belongs to another file, so it is kept, not replaced.
        std::fs::write(&a, b"").unwrap();
        let other = Meta {
            keywords: vec!["other".into()],
            comment: String::new(),
        };
        write_changed(
            &a,
            Some(other.keywords.clone()),
            Some(other.comment.clone()),
        )
        .unwrap();
        assert!(follow(&b, &a, true).is_err());
        assert_eq!(read(&a), other);
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
        write_changed(&other, Some(vec!["secret".into()]), None).unwrap();
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
        follow(&a, &b, true).unwrap();
        assert!(
            std::fs::symlink_metadata(sidecar(&b)).is_err(),
            "copied through the link"
        );
        crate::images::save::land(&a, &c, b"x", false, false).unwrap();
        assert!(
            std::fs::symlink_metadata(sidecar(&c)).is_err(),
            "landed through the link"
        );
        std::fs::rename(&a, b.with_file_name("d.png")).unwrap();
        follow(&a, &dir.join("d.png"), false).unwrap();
        assert!(linked(&sidecar(&dir.join("d.png"))), "moved as itself");
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "PRIVATE KEY");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Sidecars broken in thousands of ways are read and written to without a panic.
    #[test]
    fn survives_broken_sidecars() {
        let meta = Meta {
            keywords: vec!["beach".into(), "Tom & Ann".into()],
            comment: "Summer".into(),
        };
        let empty = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                     rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let access = Access {
            alt: "A dog".into(),
            extended: "On a beach".into(),
        };
        let seed = set_access(&set(empty, &meta, false).unwrap(), &access, false).unwrap();
        for bytes in crate::support::test_support::broken(seed.as_bytes()) {
            let xmp = String::from_utf8_lossy(&bytes);
            parse(&xmp);
            set(&xmp, &meta, true);
            set(&xmp, &Meta::default(), false);
            parse_access(&xmp);
            set_access(&xmp, &Access::default(), true);
            set_keywords(&xmp, &meta, true);
            element(&xmp, "dc:subject");
        }
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
}
