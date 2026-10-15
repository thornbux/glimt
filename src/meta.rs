//! Keywords and comment, as Geeqie's info sidebar edits them: dc:subject and dc:description in
//! XMP. They are written to the sidecar `<file>.xmp`, which darktable and digiKam read too, and
//! read from it, or else from the XMP inside the image.

use std::path::{Path, PathBuf};

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Meta {
    pub keywords: Vec<String>,
    pub comment: String,
}

impl Meta {
    /// The keywords as typed: separated by commas.
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

/// Whether two keywords are the same one: letter case does not count ("Oslo" and "oslo").
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

/// The keyword of a keyword tree path: its last level.
fn leaf(tag: &str) -> &str {
    tag.rsplit('|').next().unwrap_or(tag)
}

/// Whether an image with `keywords` and keyword tree `paths` has `tag`: the keyword, and the path
/// too unless no path ends in that keyword (a program that writes keywords only set it).
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
    (std::fs::read_to_string(sidecar(path)))
        .map(|x| unique(items(&x, "lr:hierarchicalSubject")))
        .unwrap_or_default()
}

/// `<file>.xmp`, next to the file.
pub fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".xmp");
    path.with_file_name(name)
}

/// The keywords and comment of `path`: from its sidecar if that has them, else from the image.
pub fn read(path: &Path) -> Meta {
    let has = |x: &String| x.contains("<dc:subject") || x.contains("<dc:description");
    let side = std::fs::read_to_string(sidecar(path)).ok().filter(has);
    side.or_else(|| embedded(path))
        .map_or_else(Meta::default, |x| parse(&x))
}

/// The XMP packet inside the image. A JPEG's is read from its first 256 KB, as its EXIF is.
fn embedded(path: &Path) -> Option<String> {
    const JPEG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    let bytes = match crate::exif::jpeg_app1(path, JPEG) {
        Some(found) => found?[JPEG.len()..].to_vec(),
        None => {
            use image::ImageDecoder;
            let reader = image::ImageReader::open(path)
                .ok()?
                .with_guessed_format()
                .ok()?;
            reader.into_decoder().ok()?.xmp_metadata().ok()??
        }
    };
    String::from_utf8(bytes).ok()
}

/// Write the keywords and comment into the sidecar; everything else in it stays. Keywords or a
/// comment the image holds itself and that were cleared are written as empty elements: left
/// out, the image's own would show through again.
pub fn write(path: &Path, meta: &Meta) -> Result<(), String> {
    let own = embedded(path).is_some_and(|x| parse(&x) != Meta::default());
    update_sidecar(path, *meta == Meta::default() && !own, |x| {
        set(x, meta, own)
    })
}

/// Add `keywords` to those `path` has, read and written as one change.
pub fn add(path: &Path, keywords: &[String]) -> Result<(), String> {
    let _held = held();
    let mut meta = read(path);
    for k in keywords {
        if !meta.keywords.iter().any(|m| same(m, k)) {
            meta.keywords.push(k.clone());
        }
    }
    update(path, meta == Meta::default(), |x| {
        set_keywords(x, &meta, false)
    })
}

/// Held while a sidecar is read and written again: keywords are added in the background while
/// the stars or the comment of the same file may be saved, each through the same temporary file.
/// shortcut: within this Glimt only; two windows changing one sidecar at the same moment can
/// lose one change (not the file, which is replaced whole). A file lock (File::lock, Rust 1.89)
/// if that is ever seen.
fn held() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Change the sidecar of `path` with `change`, or a new, empty one unless `only_if_there`: then
/// a file without a sidecar keeps having none. Written to a temporary file first, so a crash never leaves half a sidecar.
pub fn update_sidecar(
    path: &Path,
    only_if_there: bool,
    change: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    let _held = held();
    update(path, only_if_there, change)
}

fn update(
    path: &Path,
    only_if_there: bool,
    change: impl FnOnce(&str) -> Option<String>,
) -> Result<(), String> {
    let side = sidecar(path);
    let old = match std::fs::read_to_string(&side) {
        Ok(text) => text,
        // One that is there but cannot be read (not UTF-8, no permission) is not written over:
        // it may hold another program's edits.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("{}: {e}", side.display()));
        }
        Err(_) if only_if_there => return Ok(()),
        Err(_) => "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
             <rdf:Description rdf:about=\"\"/>\n </rdf:RDF>\n</x:xmpmeta>\n\
             <?xpacket end=\"w\"?>\n"
            .to_owned(),
    };
    let text = change(&old).ok_or("unreadable XMP sidecar")?;
    // A temporary file with this Glimt's name: two windows may write the same sidecar.
    crate::config::replace_file(&side, text).map_err(|e| e.to_string())
}

/// Copy or move the sidecar of `from`, if it has one, with the file that went to `to`.
pub fn follow(from: &Path, to: &Path, copy: bool) {
    let (side, to) = (sidecar(from), sidecar(to));
    // One there already belongs to another file, or is a copy kept on purpose: never replaced.
    if !side.exists() || to.exists() {
        return;
    }
    let copied = || std::fs::copy(&side, &to).map(drop);
    let _ = if copy {
        copied()
    } else {
        // To another file system as the file went: copied, then removed.
        std::fs::rename(&side, &to).or_else(|_| copied().and_then(|_| std::fs::remove_file(&side)))
    };
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

/// `xmp` with its keywords and comment replaced, in its first rdf:Description; `None` if it has
/// none.
/// `xmp` with the keywords and comment of `meta`; empty ones as empty elements with `say_empty`.
/// `set` for a change of keywords only: a comment the sidecar has stays as it is, in all its
/// languages; one only inside the image is copied into a new sidecar, which would hide it else.
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

/// The keyword tree paths for `keywords`, as Lightroom writes them: those of `paths` that end in
/// one of them, and each keyword without one as a path of its own (Immich, which shows tags,
/// reads only these). A keyword's own path goes when a longer one ends in it.
fn tree_paths_for(keywords: &[String], paths: Vec<String>) -> Vec<String> {
    let kept: Vec<String> = (unique(paths).into_iter())
        .filter(|p| keywords.iter().any(|k| same(k, leaf(p))))
        .collect();
    // Lightroom writes the levels above a keyword as keywords too: Places, Norway, Oslo for
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
    // Declared on this Description or above it: exiftool gives each namespace a Description of
    // its own, and a prefix declared on a later one is not in force here.
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

/// A keyword of the keyword tree, "Places|Norway|Oslo", on `path` or off: its name (Oslo)
/// among the keywords, and the whole path in lr:hierarchicalSubject, where darktable, digiKam
/// and Lightroom keep the tree's place of a keyword.
pub fn tag(path: &Path, tag: &str, on: bool) -> Result<(), String> {
    let _held = held();
    let mut meta = read(path);
    let mut paths = tree_paths(path);
    set_tag(&mut meta.keywords, &mut paths, tag, on);
    let own = embedded(path).is_some_and(|x| parse(&x) != Meta::default());
    update(path, !on && meta == Meta::default() && !own, |x| {
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
        let Some(end) = rest.find(';') else { break };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG with an empty APP1 segment followed by the XMP name: no keywords, and no panic.
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
        std::fs::write(sidecar(&file), b"\xff\xfe not UTF-8").unwrap();
        let meta = Meta {
            keywords: vec!["x".into()],
            comment: String::new(),
        };
        assert!(write(&file, &meta).is_err());
        assert_eq!(
            std::fs::read(sidecar(&file)).unwrap(),
            b"\xff\xfe not UTF-8"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// exiftool's layout, a Description for each namespace: "dc" declared on the second one is
    /// declared again on the first, where the keywords go.
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

    /// A keyword of the tree on and off: its name among the keywords, its path in
    /// lr:hierarchicalSubject, the other keywords and paths kept.
    #[test]
    fn tags_with_the_tree_path() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        let paths = |file: &Path| {
            items(
                &std::fs::read_to_string(sidecar(file)).unwrap(),
                "lr:hierarchicalSubject",
            )
        };
        tag(&file, "Places|Norway|Oslo", true).unwrap();
        tag(&file, "Places|Norway|Bergen", true).unwrap();
        assert_eq!(read(&file).keywords, ["Oslo", "Bergen"]);
        assert_eq!(paths(&file), ["Places|Norway|Oslo", "Places|Norway|Bergen"]);
        assert!(
            std::fs::read_to_string(sidecar(&file))
                .unwrap()
                .contains("xmlns:lr=")
        );
        tag(&file, "Places|Norway|Oslo", false).unwrap();
        assert_eq!(read(&file).keywords, ["Bergen"]);
        assert_eq!(paths(&file), ["Places|Norway|Bergen"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// One keyword for two paths that end in it, and in another letter case: set once, kept
    /// while one of the paths is on, and only that path's box ticked.
    #[test]
    fn keeps_a_keyword_another_path_ends_in() {
        let (mut keywords, mut paths) = (vec!["oslo".to_owned()], Vec::new());
        // Set by a program that writes keywords only: both boxes ticked.
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

    /// Keywords typed in the field get paths of their own, which Immich reads; a keyword taken
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

    /// Keywords inside a JPEG, cleared: they stay cleared, not read from the image again.
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
        write(&file, &Meta::default()).unwrap();
        assert_eq!(read(&file), Meta::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keywords_and_comment_round_trip() {
        let meta = Meta {
            keywords: vec!["beach".into(), "Tom & Ann".into()],
            comment: "Summer <2024>\nsecond line".into(),
        };
        // A darktable sidecar: an attribute-only Description with a rating, kept as it is.
        let dt = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description rdf:about=\"\" \
                  xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"3\"/></rdf:RDF></x:xmpmeta>";
        let written = set(dt, &meta, false).unwrap();
        assert!(written.contains("xmp:Rating=\"3\" xmlns:dc="));
        assert_eq!(parse(&written), meta);
        // Written again with other values: replaced, not added.
        let other = Meta {
            keywords: vec!["snow".into()],
            comment: String::new(),
        };
        let again = set(&written, &other, false).unwrap();
        assert_eq!(parse(&again), other);
        assert_eq!(again.matches("<dc:subject>").count(), 1);
        assert_eq!(again.matches("xmlns:dc=").count(), 1);
        // Everything removed: the sidecar is as it was, but for the name space.
        let none = set(&again, &Meta::default(), false).unwrap();
        assert_eq!(parse(&none), Meta::default());
        assert!(none.contains("xmp:Rating=\"3\""));
        assert_eq!(unescape("a&#x41;&#66;&bogus c&amp;"), "aAB&bogus c&");
        assert_eq!(
            Meta::parse_keywords(" a, b,,a ,c "),
            ["a", "b", "c"].map(String::from)
        );
    }

    /// Keywords added from several threads at once: none is lost.
    #[test]
    fn changes_at_once_are_all_kept() {
        let dir = std::env::temp_dir().join(format!("glimt-test-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::thread::scope(|s| {
            for k in 0..8 {
                let path = &path;
                s.spawn(move || {
                    for n in 0..10 {
                        add(path, &[format!("k{k}-{n}")]).unwrap();
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
        std::fs::write(
            sidecar(&file),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject><rdf:Bag><rdf:li>Rose</rdf:li><rdf:li>rose</rdf:li></rdf:Bag></dc:subject>
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">Hello</rdf:li><rdf:li xml:lang="de">Hallo</rdf:li></rdf:Alt></dc:description>
  </rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        // The person and the flower stay two; a tag and an added keyword leave both comments.
        assert_eq!(read(&file).keywords, ["Rose", "rose"]);
        tag(&file, "Places|Oslo", true).unwrap();
        add(&file, &k(&["ROSE"])).unwrap();
        let x = std::fs::read_to_string(sidecar(&file)).unwrap();
        assert_eq!(read(&file).keywords, ["Rose", "rose", "Oslo"]);
        assert!(x.contains(">Hello<") && x.contains(">Hallo<"), "{x}");
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
        write(&a, &meta).unwrap();
        assert_eq!(read(&a), meta);
        std::fs::rename(&a, &b).unwrap();
        follow(&a, &b, false);
        assert_eq!(read(&b), meta);
        std::fs::copy(&b, &c).unwrap();
        follow(&b, &c, true);
        assert_eq!(read(&c), meta);
        // A sidecar at the target is another file's: it is kept, not replaced.
        let other = Meta {
            keywords: vec!["other".into()],
            comment: String::new(),
        };
        write(&a, &other).unwrap();
        follow(&b, &a, true);
        assert_eq!(read(&a), other);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Sidecars broken in thousands of ways: read and written to without a panic.
    #[test]
    fn survives_broken_sidecars() {
        let meta = Meta {
            keywords: vec!["beach".into(), "Tom & Ann".into()],
            comment: "Summer".into(),
        };
        let empty = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                     rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let seed = set(empty, &meta, false).unwrap();
        for bytes in crate::broken(seed.as_bytes()) {
            let xmp = String::from_utf8_lossy(&bytes);
            parse(&xmp);
            set(&xmp, &meta, true);
            set(&xmp, &Meta::default(), false);
        }
    }
}
