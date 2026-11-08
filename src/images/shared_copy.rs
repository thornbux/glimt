//! Copy for Sharing's copies: a JPEG, PNG or WebP with its image data byte for byte and only the
//! metadata the user keeps. The file is walked part by part (`walk_jpeg`, `walk_png`,
//! `walk_webp`), and the copy is made new of the parts kept (`assembled`): an allow-list, so
//! nothing the user did not keep can come along. The EXIF block and the XMP packet are written
//! new of the tags kept (`save::shared_metadata`), with the sidecar's properties in the XMP. The
//! tags the dialog lists come from the same walk (`tags_of`), so the list and the copy agree. The
//! list reads only the metadata of each file (`read_without_image_data`), not its image data, so
//! thousands of photos are listed in seconds.

use crate::images::decode::{MAX_DECODE_BYTES, open_regular_with_metadata, read_head};
use crate::images::save::{MAX_JPEG_EXIF, SharedMetadata, crc32, shared_metadata};
use crate::metadata::xmp::Properties;
use crate::support::lang::tr;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

/// The words that put a tag in a group, looked for in its whole name (its namespace's prefix
/// and its own name) in lower case. A tag with words of several groups is in each of them:
/// GPSDateStamp says where and when, and a file name such as PXL_20231004_142355 holds the date,
/// while a path names the user. A name with none of them is in More tags, unless
/// `GROUP_EXCEPTIONS` says otherwise. Exif's auxiliary namespace (aux:) holds the lens and serial
/// numbers, and PLUS (plus:) holds the licence and its owner.
const GROUP_WORDS: [(Group, &[&str]); 7] = [
    (
        Group::Place,
        &[
            "gps",
            "latitude",
            "longitude",
            "altitude",
            "location",
            "city",
            "country",
        ],
    ),
    (
        Group::Camera,
        &[
            "serial", "make", "model", "lens", "body", "camera", "firmware", "aux:",
        ],
    ),
    (
        Group::Date,
        &[
            "date",
            "time",
            "stamp",
            "history",
            "filename",
            "derivedfrom",
        ],
    ),
    (
        Group::Photographer,
        &[
            "owner",
            "author",
            "creator",
            "artist",
            "copyright",
            "rights",
            "byline",
            "credit",
            "photographer",
            "plus:",
            "filename",
            "derivedfrom",
        ],
    ),
    (
        Group::Software,
        &["software", "tool", "history", "processing"],
    ),
    (Group::Keywords, &["keyword", "mediapro:"]),
    (Group::Preview, &["thumbnail"]),
];

/// The tags whose groups are not those their words give (`GROUP_WORDS`), with the groups they
/// are in instead. Some have no word of their group, such as a caption. Others have a word of
/// a group they say nothing of: an exposure time is no date, the subject's location is a place
/// in the picture, not on the map, and the camera's owner is a person, not the camera.
const GROUP_EXCEPTIONS: &[(&str, &[Group])] = &[
    ("photoshop:State", &[Group::Place]),
    ("mediapro:Location", &[Group::Place]),
    ("GPSProcessingMethod", &[Group::Place]),
    ("exif:GPSProcessingMethod", &[Group::Place]),
    ("SubjectLocation", &[Group::Other]),
    ("exif:SubjectLocation", &[Group::Other]),
    ("ExposureTime", &[Group::Other]),
    ("exif:ExposureTime", &[Group::Other]),
    ("ImageUniqueID", &[Group::Camera]),
    ("exif:ImageUniqueID", &[Group::Camera]),
    ("exifEX:ImageUniqueID", &[Group::Camera]),
    // A position in the motion photo's video, in microseconds, not a date.
    (
        "GCamera:MotionPhotoPresentationTimestampUs",
        &[Group::Camera],
    ),
    (
        "GCamera:MicroVideoPresentationTimestampUs",
        &[Group::Camera],
    ),
    ("CameraOwnerName", &[Group::Photographer]),
    ("exifEX:CameraOwnerName", &[Group::Photographer]),
    ("aux:OwnerName", &[Group::Photographer]),
    ("dc:publisher", &[Group::Photographer]),
    ("dc:contributor", &[Group::Photographer]),
    ("photoshop:Source", &[Group::Photographer]),
    ("photoshop:CaptionWriter", &[Group::Photographer]),
    ("plus:ModelReleaseStatus", &[Group::Photographer]),
    ("plus:ModelReleaseID", &[Group::Photographer]),
    ("Iptc4xmpExt:ModelAge", &[Group::Keywords]),
    ("Iptc4xmpExt:AddlModelInfo", &[Group::Keywords]),
    ("HostComputer", &[Group::Software]),
    ("ImageEditor", &[Group::Software]),
    ("xmp:CreatorTool", &[Group::Software]),
    // The files a photo was made from, by their paths, names and dates, and the programs.
    (
        "xmpMM:DerivedFrom",
        &[Group::Date, Group::Photographer, Group::Software],
    ),
    (
        "xmpMM:Ingredients",
        &[Group::Date, Group::Photographer, Group::Software],
    ),
    ("ImageDescription", &[Group::Keywords]),
    ("UserComment", &[Group::Keywords]),
    ("ImageTitle", &[Group::Keywords]),
    ("XPTitle", &[Group::Keywords]),
    ("XPComment", &[Group::Keywords]),
    ("XPSubject", &[Group::Keywords]),
    ("Comment", &[Group::Keywords]),
    ("Description", &[Group::Keywords]),
    ("Title", &[Group::Keywords]),
    (JPEG_COMMENT, &[Group::Keywords]),
    ("dc:subject", &[Group::Keywords]),
    ("dc:description", &[Group::Keywords]),
    ("dc:title", &[Group::Keywords]),
    ("lr:hierarchicalSubject", &[Group::Keywords]),
    ("Iptc4xmpCore:AltTextAccessibility", &[Group::Keywords]),
    ("Iptc4xmpCore:ExtDescrAccessibility", &[Group::Keywords]),
    ("photoshop:Headline", &[Group::Keywords]),
    ("photoshop:Instructions", &[Group::Keywords]),
    ("exif:UserComment", &[Group::Keywords]),
    ("tiff:ImageDescription", &[Group::Keywords]),
    ("digiKam:TagsList", &[Group::Keywords]),
    ("mwg-rs:Regions", &[Group::Keywords]),
    ("Iptc4xmpExt:PersonInImage", &[Group::Keywords]),
];

/// The tags that are blocks of other tags, not a single tag, with the groups whose tags each
/// can hold. A block is left out when any tag of one of its groups is (`LeftOut::is_left_out`),
/// because it may hold that tag again: a phone's second image has its own EXIF with GPS, and
/// a maker note can hold the city or the owner's name. When it is unclear what a block holds,
/// it counts as holding every group. A part that only decides how the image looks, such as
/// the JFIF header or the ICC profile, is no tag, and every copy keeps it. The Interop
/// directory is not here either, as a copy takes only its own tags (`exif::shared_block`).
const BLOCKS: &[(&str, &[Group])] = &[
    (MPF, &Group::ALL),
    (AFTER_IMAGE, &Group::ALL),
    ("MakerNote", &Group::ALL),
    (PHOTOSHOP_RESOURCES, &Group::ALL),
    (C2PA, &Group::ALL),
    ("XMLPacket", &Group::ALL),
    ("IPTC-NAA", &Group::ALL),
    (RAW_PROFILES, &Group::ALL),
    (JFIF_THUMBNAIL, &[Group::Preview]),
    ("xmpMM:History", &[Group::Software, Group::Date]),
    // The files that a photo was made from, each with its path, name and date.
    (
        "xmpMM:DerivedFrom",
        &[Group::Software, Group::Date, Group::Photographer],
    ),
    (
        "xmpMM:Ingredients",
        &[Group::Software, Group::Date, Group::Photographer],
    ),
];

/// The starts of the names of tags whose contents are not known: a JPEG's other APPn
/// segments, a PNG's and a WebP's other chunks, and the EXIF tags without a name. Each counts
/// as a block that holds every group (`BLOCKS`).
const UNKNOWN_BLOCKS: [&str; 5] = [
    "JPEG APP",
    "PNG chunk ",
    "WebP chunk ",
    "Image 0x",
    "Exif 0x",
];

/// The tag of a JPEG's JFXX segment, a thumbnail beside the JFIF header, and of a thumbnail
/// inside the JFIF header itself (`add_jfif`).
const JFIF_THUMBNAIL: &str = "JFIF thumbnail";

/// The tag of an MPF segment and of the images after the main one that it indexes: a phone's
/// second image or HDR gain map, each with an EXIF block of its own.
const MPF: &str = "Extra images (MPF)";

/// The tag of what follows a JPEG's main image when no MPF segment indexes it.
const AFTER_IMAGE: &str = "Data after the image";

/// The tag of a JPEG's content credentials (C2PA, in APP11 segments).
const C2PA: &str = "Content credentials (C2PA)";

/// The tag of a JPEG's Photoshop resources other than its IPTC IIM, its EXIF and its XMP, such as
/// a thumbnail or the paths of a clipping.
const PHOTOSHOP_RESOURCES: &str = "Photoshop resources";

/// The tag of a PNG's ImageMagick raw profiles other than those of EXIF, XMP and IPTC, which the
/// copy writes new or leaves out (`REWRITTEN_PROFILES`).
const RAW_PROFILES: &str = "Raw profiles";

/// The tag of a JPEG's comment segment (COM).
const JPEG_COMMENT: &str = "JPEG comment";

/// The kinds of a PNG's raw profiles that hold the EXIF, the XMP or the IPTC again. They are
/// never copied, because the copy's own EXIF and XMP hold what is kept of them.
/// shortcut: the IPTC of a PNG's raw profile is not carried into the XMP; read it if users keep
/// IPTC in PNG files.
const REWRITTEN_PROFILES: [&[u8]; 4] = [b"exif", b"app1", b"xmp", b"iptc"];

/// The Photoshop resources a copy never carries: the IPTC IIM (0x0404), which goes into the XMP,
/// its digest (0x0425), and copies of the EXIF and XMP (0x0422, 0x0423, 0x0424).
const LEFT_OUT_RESOURCES: [u16; 5] = [0x0404, 0x0422, 0x0423, 0x0424, 0x0425];

/// The PNG chunks that are the image or decide how it looks, which every copy keeps.
const PNG_IMAGE_CHUNKS: [&[u8; 4]; 20] = [
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT",
    b"pHYs", b"bKGD", b"cICP", b"mDCV", b"cLLI", b"hIST", b"sPLT", b"acTL", b"fcTL", b"fdAT",
];

/// The WebP chunks that are the image or decide how it looks, which every copy keeps.
const WEBP_IMAGE_CHUNKS: [&[u8; 4]; 7] = [
    b"VP8 ", b"VP8L", b"VP8X", b"ALPH", b"ANIM", b"ANMF", b"ICCP",
];

/// The start of a JPEG's APP1 segment of XMP, Adobe's name for it.
const XMP_NAME: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// The start of a JPEG's APP13 segment of Photoshop resources.
const PHOTOSHOP_NAME: &[u8] = b"Photoshop 3.0\0";

/// The IPTC IIM datasets of record 2 carried into XMP: the dataset's number, the XMP property of
/// the same meaning in IPTC Core or Dublin Core, and its form: a bag ('b'), an ordered list
/// ('s'), a text in languages ('a'), or a text ('t').
const IIM_IN_XMP: [(u8, &str, char); 18] = [
    (5, "dc:title", 'a'),
    (25, "dc:subject", 'b'),
    (80, "dc:creator", 's'),
    (116, "dc:rights", 'a'),
    (120, "dc:description", 'a'),
    (105, "photoshop:Headline", 't'),
    (110, "photoshop:Credit", 't'),
    (115, "photoshop:Source", 't'),
    (90, "photoshop:City", 't'),
    (95, "photoshop:State", 't'),
    (101, "photoshop:Country", 't'),
    (100, "Iptc4xmpCore:CountryCode", 't'),
    (92, "Iptc4xmpCore:Location", 't'),
    (55, "photoshop:DateCreated", 't'),
    (85, "photoshop:AuthorsPosition", 't'),
    (40, "photoshop:Instructions", 't'),
    (103, "photoshop:TransmissionReference", 't'),
    (122, "photoshop:CaptionWriter", 't'),
];

/// The namespaces of the XMP properties that IPTC IIM is carried into.
const IIM_NAMESPACES: [(&str, &str); 3] = [
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("photoshop", "http://ns.adobe.com/photoshop/1.0/"),
    (
        "Iptc4xmpCore",
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
    ),
];

/// A group of tags, which the dialog shows with a checkbox for all of them and a list of each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Group {
    Place,
    Camera,
    Date,
    Photographer,
    Software,
    Keywords,
    Preview,
    /// More tags: every tag of no other group.
    Other,
}

impl Group {
    /// Every group, in the order the dialog shows them.
    pub(crate) const ALL: [Group; 8] = [
        Group::Place,
        Group::Camera,
        Group::Date,
        Group::Photographer,
        Group::Software,
        Group::Keywords,
        Group::Preview,
        Group::Other,
    ];

    /// The group's name in the dialog.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Group::Place => tr("Place"),
            Group::Camera => tr("Camera and lens"),
            Group::Date => tr("Date and time"),
            Group::Photographer => tr("Photographer and copyright"),
            Group::Software => tr("Software"),
            Group::Keywords => tr("Keywords, comment and alt text"),
            Group::Preview => tr("The preview inside the file"),
            Group::Other => tr("More tags"),
        }
    }

    /// The word kept in the choice (`LeftOut`) when the whole group is left out. Its colon
    /// keeps it apart from the tags' names, none of which starts with "group:".
    fn key(self) -> &'static str {
        match self {
            Group::Place => "group:place",
            Group::Camera => "group:camera",
            Group::Date => "group:date",
            Group::Photographer => "group:photographer",
            Group::Software => "group:software",
            Group::Keywords => "group:keywords",
            Group::Preview => "group:preview",
            Group::Other => "group:other",
        }
    }

    /// Returns the groups of the tag named `tag`, in the order of `Group::ALL`: those
    /// `GROUP_EXCEPTIONS` gives it, or else those whose words (`GROUP_WORDS`) its name has, or
    /// else More tags. An XMP property's prefix is that of its namespace (`xmp::properties`),
    /// not the one its packet wrote. This is the one place that decides a tag's groups: the
    /// dialog lists the tag under each, and the box of any of them leaves it out.
    pub(crate) fn groups_of(tag: &str) -> Vec<Group> {
        if let Some((_, groups)) = GROUP_EXCEPTIONS.iter().find(|(name, _)| *name == tag) {
            return Group::ALL
                .into_iter()
                .filter(|g| groups.contains(g))
                .collect();
        }
        let lower = tag.to_ascii_lowercase();
        let groups: Vec<Group> = (GROUP_WORDS.iter())
            .filter(|(_, words)| words.iter().any(|w| lower.contains(w)))
            .map(|(group, _)| *group)
            .collect();
        if groups.is_empty() {
            vec![Group::Other]
        } else {
            groups
        }
    }

    /// Returns the group whose key (`Group::key`) `word` is, or `None` when it names a tag.
    fn of_key(word: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.key() == word)
    }
}

/// What Copy for Sharing leaves out, as the user ticked it: the names of single tags, and the
/// keys of whole groups (`Group::key`). A tag of a group left out whole is left out also when it
/// was not seen before, so a new GPS field cannot slip through. The dialog starts with the
/// choice made last time, kept in `config::sharing_path` one word to a line.
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct LeftOut {
    words: BTreeSet<String>,
    /// Whether each group, in the order of `Group::ALL`, has a tag left out or is left out
    /// whole. A block (`BLOCKS`) goes with any group it can hold that has. It is counted again
    /// after every change of `words`.
    has_left_out: [bool; 8],
}

impl LeftOut {
    /// Reads a choice from the text of its file, one word to a line.
    pub(crate) fn parse(text: &str) -> LeftOut {
        let words = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let mut left_out = LeftOut {
            words: words.map(str::to_owned).collect(),
            has_left_out: [false; 8],
        };
        left_out.count_groups();
        left_out
    }

    /// Counts again which groups have a tag left out (`has_left_out`).
    fn count_groups(&mut self) {
        self.has_left_out = [false; 8];
        for word in &self.words {
            let groups = Group::of_key(word).map_or_else(|| Group::groups_of(word), |g| vec![g]);
            for group in groups {
                self.has_left_out[group as usize] = true;
            }
        }
    }

    /// The text of the choice's file, one word to a line.
    pub(crate) fn text(&self) -> String {
        self.words.iter().map(|w| format!("{w}\n")).collect()
    }

    /// Whether the tag `tag` is left out: by its name, by any of its groups left out whole, or
    /// as a block that can hold a tag of a group that has one left out (`BLOCKS`).
    pub(crate) fn is_left_out(&self, tag: &str) -> bool {
        self.words.contains(tag)
            || (Group::groups_of(tag).iter()).any(|g| self.words.contains(g.key()))
            || (block_groups(tag).iter()).any(|g| self.has_left_out[*g as usize])
    }

    /// Whether the whole group `group` is left out.
    pub(crate) fn is_group_left_out(&self, group: Group) -> bool {
        self.words.contains(group.key())
    }

    /// Leaves the tag `tag` out, or keeps it. A tag kept out of groups left out whole leaves
    /// the other tags of `found` in those groups out one by one instead.
    pub(crate) fn set_tag(&mut self, tag: &str, left_out: bool, found: &[String]) {
        if left_out {
            self.words.insert(tag.to_owned());
        } else {
            self.words.remove(tag);
            for group in Group::groups_of(tag) {
                if self.words.remove(group.key()) {
                    let is_other = |t: &&String| *t != tag && Group::groups_of(t).contains(&group);
                    self.words.extend(found.iter().filter(is_other).cloned());
                }
            }
        }
        self.count_groups();
    }

    /// Leaves the whole group `group` out, or keeps every tag of it.
    pub(crate) fn set_group(&mut self, group: Group, left_out: bool) {
        if left_out {
            self.words.insert(group.key().to_owned());
        } else {
            // The other groups' keys stay, though `Group::groups_of` puts no key in a group.
            let is_key = |w: &str| w.starts_with("group:");
            let is_in_group = |w: &str| Group::groups_of(w).contains(&group);
            (self.words).retain(|w| w != group.key() && (is_key(w) || !is_in_group(w)));
        }
        self.count_groups();
    }

    /// Leaves every tag out. The orientation and the colour profile are still kept, as they are
    /// not tags of the choice.
    pub(crate) fn remove_all(&mut self) {
        Group::ALL.iter().for_each(|g| self.set_group(*g, true));
    }

    /// Returns the tags of `found` that a copy keeps: those not left out. A tag that is not in
    /// `found`, which the dialog did not show, is never kept.
    pub(crate) fn kept(&self, found: &[String]) -> HashSet<String> {
        (found.iter())
            .filter(|t| !self.is_left_out(t))
            .cloned()
            .collect()
    }
}

/// A resource of a Photoshop block: its ID, its whole bytes and its data.
type PhotoshopResource<'a> = (u16, &'a [u8], &'a [u8]);

/// The container a file walked is in.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Container {
    Jpeg,
    Png,
    WebP,
}

/// A part of the file walked, carried into the copy as it is unless its tag is left out. A part
/// without a tag is image data or decides how the image looks, and every copy has it.
struct Part<'a> {
    bytes: Cow<'a, [u8]>,
    /// Where the part starts in the file walked.
    from: usize,
    tag: Option<String>,
}

/// A file walked for a copy for sharing: its parts in order, how many of the first ones go
/// before the new EXIF and XMP, the EXIF block and the XMP inside it, and the properties its
/// IPTC IIM gives.
struct Walked<'a> {
    container: Container,
    parts: Vec<Part<'a>>,
    before_metadata: usize,
    exif: Option<&'a [u8]>,
    xmp: Option<&'a [u8]>,
    iim: Properties,
    /// Where a JPEG's main image ends and what follows it starts, when anything does.
    image_end: Option<usize>,
}

impl<'a> Walked<'a> {
    fn new(container: Container) -> Self {
        Walked {
            container,
            parts: Vec::new(),
            before_metadata: 0,
            exif: None,
            xmp: None,
            iim: Properties::default(),
            image_end: None,
        }
    }

    /// Adds the bytes `from..to` of `data` as a part with the tag `tag`, or none.
    fn add(&mut self, data: &'a [u8], from: usize, to: usize, tag: Option<&str>) {
        self.parts.push(Part {
            bytes: Cow::Borrowed(&data[from..to]),
            from,
            tag: tag.map(str::to_owned),
        });
    }
}

/// Returns the names of the tags of `path` that a copy for sharing can keep or leave out,
/// sorted, each once: those of its EXIF, its XMP, its sidecar and its IPTC, and its other parts
/// with a tag. Returns `None` for a file that is not a JPEG, PNG or WebP, and an error for one
/// that cannot be read or is damaged.
pub(crate) fn tags_of(path: &Path) -> Result<Option<Vec<String>>, String> {
    let read = read_without_image_data(path).map_err(|e| e.to_string())?;
    let Some(data) = read else {
        return Ok(None);
    };
    with_walked(path, &data, |walked, metadata| {
        let mut tags = metadata.tag_names();
        tags.extend(walked.parts.iter().filter_map(|p| p.tag.clone()));
        tags.sort();
        tags.dedup();
        tags
    })
}

/// Makes the copy for sharing of `path` that keeps the tags `kept`, besides what every copy
/// keeps. Returns `None` for a file that is not a JPEG, PNG or WebP, and an error for one that
/// cannot be read or is damaged.
pub(crate) fn copy(path: &Path, kept: &HashSet<String>) -> Result<Option<Vec<u8>>, String> {
    // A video of a gigabyte is another format, told by its first bytes, not a file too large.
    let head = read_head(path, 12).map_err(|e| e.to_string())?;
    if container_of(&head).is_none() {
        return Ok(None);
    }
    let data = crate::images::decode::read_limited(path).map_err(|e| e.to_string())?;
    let keeps = |tag: &str| kept.contains(tag);
    with_walked(path, &data, |walked, metadata| {
        let (exif, xmp) = metadata.written(&keeps);
        assembled(walked, &keeps, exif, xmp)
    })?
    .transpose()
}

/// Walks `data`, read of `path`, reads what it may keep of its metadata, and gives both to
/// `then`. The EXIF block is the one found in the walk, so the file is not read again for it.
/// Returns `None` for a file that is not a JPEG, PNG or WebP.
fn with_walked<T>(
    path: &Path,
    data: &[u8],
    then: impl FnOnce(&Walked, &SharedMetadata) -> T,
) -> Result<Option<T>, String> {
    let Some(mut walked) = walk(data)? else {
        return Ok(None);
    };
    let iim = std::mem::take(&mut walked.iim);
    let metadata = shared_metadata(path, walked.exif, walked.xmp, iim)?;
    Ok(Some(then(&walked, &metadata)))
}

/// Returns the container that the first bytes `head` of a file say it is in, or `None` when
/// they are not those of a JPEG, PNG or WebP.
fn container_of(head: &[u8]) -> Option<Container> {
    if head.starts_with(&[0xff, 0xd8]) {
        Some(Container::Jpeg)
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Container::Png)
    } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(&b"WEBP"[..]) {
        Some(Container::WebP)
    } else {
        None
    }
}

/// Walks `data` as the container its first bytes say. Returns `None` when it is not a JPEG,
/// PNG or WebP, and an error when it is one that is damaged.
fn walk(data: &[u8]) -> Result<Option<Walked<'_>>, String> {
    let walked = match container_of(data) {
        Some(Container::Jpeg) => walk_jpeg(data),
        Some(Container::Png) => walk_png(data),
        Some(Container::WebP) => walk_webp(data),
        None => return Ok(None),
    };
    walked.map(Some).ok_or_else(|| "damaged or cut off".into())
}

/// Reads what `tags_of` walks of `path`: the file without its image data, in a few small reads.
/// A JPEG is read up to its first scan (`jpeg_without_scans`), and a PNG's and a WebP's chunks
/// of image data are skipped (`chunks_without_image_data`). The walkers read what is left as
/// they read the whole file. Returns `None` when the file's first bytes are not those of a
/// JPEG, PNG or WebP, so a video is not read at all.
fn read_without_image_data(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let (mut file, metadata) = open_regular_with_metadata(path)?;
    let mut head = [0; 12];
    match file.read_exact(&mut head) {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        read => read?,
    }
    match container_of(&head) {
        None => Ok(None),
        Some(Container::Jpeg) => jpeg_without_scans(path, file, metadata.len()).map(Some),
        Some(container) => {
            chunks_without_image_data(file, head, metadata.len(), container).map(Some)
        }
    }
}

/// Reads the JPEG `path` of `size` bytes up to its first scan, from a quarter of a MB on and
/// more only when its segments are longer. An end of image (EOI) stands for the scans, and one
/// byte more after it when the file `file` does not end in one, so the walk sees that something
/// follows the image. A file read whole is given as it is.
/// shortcut: what follows the first scan is not read, so a comment between the scans of a
/// progressive JPEG is not listed, and a copy leaves it out; read the scans too if one matters.
fn jpeg_without_scans(path: &Path, mut file: std::fs::File, size: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek};
    let mut cap: u64 = 1 << 18;
    loop {
        let head = read_head(path, cap)?;
        if head.len() as u64 >= size || cap >= MAX_DECODE_BYTES {
            return Ok(head);
        }
        if let Some(scan) = crate::metadata::exif::jpeg_scan_start(&head) {
            let mut out = [&head[..scan], &[0xff, 0xd9]].concat();
            let mut tail = [0; 2];
            file.seek(std::io::SeekFrom::End(-2))?;
            file.read_exact(&mut tail)?;
            if tail != [0xff, 0xd9] {
                out.push(0);
            }
            return Ok(out);
        }
        cap = (cap * 16).min(MAX_DECODE_BYTES);
    }
}

/// Reads a PNG's or a WebP's chunks from `file`, after its first 12 bytes `head`, without those
/// of image data. Each of those stands as an empty chunk, and its data is skipped with a seek.
/// A PNG is read up to its end (IEND), a WebP up to the end its RIFF header gives, and both at
/// most to the file's size `size`. Returns an error when what is read would be larger than a
/// decode reads (`MAX_DECODE_BYTES`).
fn chunks_without_image_data(
    file: std::fs::File,
    head: [u8; 12],
    size: u64,
    container: Container,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let is_png = container == Container::Png;
    let mut file = std::io::BufReader::new(file);
    let (mut out, end) = if is_png {
        // A PNG's first chunk starts after its 8 bytes of signature.
        file.seek_relative(-4)?;
        (head[..8].to_vec(), size)
    } else {
        let riff = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        (head.to_vec(), size.min(8 + u64::from(riff)))
    };
    let mut at = out.len() as u64;
    while at + 8 <= end {
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let (name, len) = if is_png {
            let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
            (&header[4..], u64::from(len))
        } else {
            let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
            (&header[..4], u64::from(len))
        };
        // The chunk's data and what follows it: a PNG's CRC, or a WebP's byte to an even size.
        let whole = len + if is_png { 4 } else { len % 2 };
        let is_image = match is_png {
            true => matches!(name, b"IDAT" | b"fdAT"),
            false => matches!(name, b"VP8 " | b"VP8L" | b"ALPH" | b"ANMF"),
        };
        if is_image {
            let empty = match is_png {
                true => [&[0; 4][..], name, &[0; 4]].concat(),
                false => [name, &[0; 4]].concat(),
            };
            out.extend(empty);
            file.seek_relative(whole as i64)?;
        } else {
            let kept = whole.min(end - at - 8);
            if out.len() as u64 + 8 + kept > MAX_DECODE_BYTES {
                return Err(std::io::Error::other("its metadata is too large"));
            }
            out.extend(header);
            let start = out.len();
            out.resize(start + kept as usize, 0);
            file.read_exact(&mut out[start..])?;
        }
        at += 8 + whole;
        if is_png && name == b"IEND" {
            break;
        }
    }
    if !is_png {
        let riff = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&riff.to_le_bytes());
    }
    Ok(out)
}

/// Walks a JPEG: each segment up to the end of its image (EOI), each scan with its data, and
/// what follows the image. Bytes between segments that are no marker, which some phones and
/// editors write, are skipped to the next marker as decoders skip them, and are not copied.
/// Returns `None` when a segment's length runs past the file.
fn walk_jpeg(data: &[u8]) -> Option<Walked<'_>> {
    let mut walked = Walked::new(Container::Jpeg);
    walked.add(data, 0, 2, None);
    let (mut at, mut has_mpf) = (2, false);
    while at < data.len() {
        // A marker may follow any number of fill bytes (0xff).
        while data.get(at) == Some(&0xff) && data.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        let marker = *data.get(at + 1)?;
        // Bytes that are no marker are left out up to the next 0xff, as decoders skip them.
        if data[at] != 0xff || marker == 0 {
            at += 1 + data[at + 1..].iter().position(|&b| b == 0xff)?;
            continue;
        }
        if marker == 0xd9 {
            walked.add(data, at, at + 2, None);
            at += 2;
            break;
        }
        // A marker without a length, outside a scan where it does not belong.
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            walked.add(data, at, at + 2, None);
            at += 2;
            continue;
        }
        let len = u16::from_be_bytes([*data.get(at + 2)?, *data.get(at + 3)?]) as usize;
        let end = at + 2 + len;
        if len < 2 || end > data.len() {
            return None;
        }
        if marker == 0xda {
            let scan_end = entropy_end(data, end);
            walked.add(data, at, scan_end, None);
            at = scan_end;
            continue;
        }
        has_mpf |= marker == 0xe2 && data[at + 4..end].starts_with(b"MPF\0");
        add_jpeg_segment(&mut walked, data, at, end);
        at = end;
    }
    if at < data.len() {
        walked.image_end = Some(at);
        let tag = if has_mpf { MPF } else { AFTER_IMAGE };
        walked.add(data, at, data.len(), Some(tag));
    }
    let jfif = (walked.parts.get(1)).is_some_and(|p| p.bytes.get(4..9) == Some(&b"JFIF\0"[..]));
    walked.before_metadata = 1 + jfif as usize;
    Some(walked)
}

/// Returns where the entropy-coded data of a scan that starts at `at` ends: at the next marker
/// that is not a stuffed 0xff (0xff00), a restart marker (RSTn) or a fill byte, or at the end
/// of `data` for a file cut off in it.
fn entropy_end(data: &[u8], mut at: usize) -> usize {
    while let Some(i) = data
        .get(at..)
        .and_then(|d| d.iter().position(|&b| b == 0xff))
    {
        let ff = at + i;
        match data.get(ff + 1) {
            Some(0 | 0xd0..=0xd7) => at = ff + 2,
            Some(0xff) => at = ff + 1,
            Some(_) => return ff,
            None => return data.len(),
        }
    }
    data.len()
}

/// Adds the JPEG segment `data[at..end]` to `walked`: kept, under a tag, read for its XMP or
/// IPTC IIM, or left out because the copy writes it new.
fn add_jpeg_segment<'a>(walked: &mut Walked<'a>, data: &'a [u8], at: usize, end: usize) {
    let (marker, body) = (data[at + 1], &data[at + 4..end]);
    let tag = match marker {
        0xe0 if body.starts_with(b"JFIF\0") => return add_jfif(walked, data, at, end),
        0xe0 if body.starts_with(b"JFXX\0") => Some(JFIF_THUMBNAIL.to_owned()),
        // The EXIF and XMP are written new of what is kept. Extended XMP, the rest of a packet
        // too long for one segment, is not carried.
        // shortcut: Extended XMP (Google's depth maps) is left out; carry it if users ask.
        0xe1 if body.starts_with(b"Exif\0\0") => {
            walked.exif.get_or_insert(&body[6..]);
            return;
        }
        0xe1 if body.starts_with(XMP_NAME) => {
            walked.xmp.get_or_insert(&body[XMP_NAME.len()..]);
            return;
        }
        0xe1 if body.starts_with(b"http://ns.adobe.com/xmp/extension/\0") => return,
        0xe2 if body.starts_with(b"ICC_PROFILE\0") => None,
        0xe2 if body.starts_with(b"MPF\0") => Some(MPF.to_owned()),
        0xeb if body.starts_with(b"JP") => Some(C2PA.to_owned()),
        0xed if body.starts_with(PHOTOSHOP_NAME) => {
            return add_photoshop(walked, &body[PHOTOSHOP_NAME.len()..], at);
        }
        0xee if body.starts_with(b"Adobe") => None,
        0xfe => Some(JPEG_COMMENT.to_owned()),
        0xe0..=0xef => Some(format!("JPEG APP{}", marker - 0xe0)),
        _ => None,
    };
    walked.add(data, at, end, tag.as_deref());
}

/// Adds a JPEG's JFIF segment `data[at..end]` to `walked`. Its header decides how the image is
/// shown, so every copy has it, but without the thumbnail it may hold (its width and height not
/// 0). That thumbnail moves into a JFXX segment of its own under `JFIF_THUMBNAIL`, at 3 bytes a
/// pixel (extension code 0x13), so it can be left out as the other previews can.
fn add_jfif<'a>(walked: &mut Walked<'a>, data: &'a [u8], at: usize, end: usize) {
    let body = &data[at + 4..end];
    let Some(header) = body.get(..14) else {
        return walked.add(data, at, end, None);
    };
    let (width, height) = (header[12], header[13]);
    let pixels = body.get(14..14 + 3 * width as usize * height as usize);
    walked.parts.push(Part {
        bytes: Cow::Owned(jpeg_segment(0xe0, &[&header[..12], &[0, 0]].concat())),
        from: at,
        tag: None,
    });
    if let Some(pixels) = pixels.filter(|p| !p.is_empty()) {
        let jfxx = [&b"JFXX\0\x13"[..], &[width, height], pixels].concat();
        walked.parts.push(Part {
            bytes: Cow::Owned(jpeg_segment(0xe0, &jfxx)),
            from: at,
            tag: Some(JFIF_THUMBNAIL.to_owned()),
        });
    }
}

/// Reads the Photoshop resources `resources` of a JPEG's APP13 segment at `at`: its IPTC IIM
/// into `walked`'s properties, and the resources a copy may keep into a new segment under
/// `PHOTOSHOP_RESOURCES`. A segment whose resources cannot be read is left out whole.
fn add_photoshop(walked: &mut Walked, resources: &[u8], at: usize) {
    let Some(list) = photoshop_resources(resources) else {
        return;
    };
    let mut kept = Vec::new();
    for (id, whole, data) in list {
        if id == 0x0404 {
            walked.iim.add_missing(iim_properties(data));
        } else if !LEFT_OUT_RESOURCES.contains(&id) {
            kept.extend_from_slice(whole);
        }
    }
    let len = 2 + PHOTOSHOP_NAME.len() + kept.len();
    if kept.is_empty() || len > 0xffff {
        return;
    }
    let segment = [
        &[0xff, 0xed][..],
        &(len as u16).to_be_bytes(),
        PHOTOSHOP_NAME,
        &kept,
    ]
    .concat();
    walked.parts.push(Part {
        bytes: Cow::Owned(segment),
        from: at,
        tag: Some(PHOTOSHOP_RESOURCES.to_owned()),
    });
}

/// Returns the groups whose tags the block `tag` can hold (`BLOCKS`, `UNKNOWN_BLOCKS`). Returns
/// none when it is a single tag.
fn block_groups(tag: &str) -> &'static [Group] {
    if let Some(&(_, groups)) = BLOCKS.iter().find(|(name, _)| *name == tag) {
        groups
    } else if UNKNOWN_BLOCKS.iter().any(|p| tag.starts_with(p)) {
        &Group::ALL
    } else {
        &[]
    }
}

/// Returns the resources of a Photoshop block, each as (its ID, its whole bytes, its data).
/// Returns `None` when one of them does not read as a resource or runs past the block.
fn photoshop_resources(block: &[u8]) -> Option<Vec<PhotoshopResource<'_>>> {
    let (mut out, mut at) = (Vec::new(), 0);
    while at < block.len() {
        let start = at;
        if block.get(at..at + 4)? != b"8BIM" {
            return None;
        }
        let id = u16::from_be_bytes([*block.get(at + 4)?, *block.get(at + 5)?]);
        // The name: a length byte and the name, padded to an even length.
        let name = 1 + *block.get(at + 6)? as usize;
        at += 6 + name + name % 2;
        let size = u32::from_be_bytes(block.get(at..at + 4)?.try_into().ok()?) as usize;
        let data = block.get(at + 4..(at + 4).checked_add(size)?)?;
        at = (at + 4 + size + size % 2).min(block.len());
        out.push((id, &block[start..at], data));
    }
    Some(out)
}

/// Reads the IPTC IIM datasets `iim` into the XMP properties of the same meaning
/// (`IIM_IN_XMP`). The text is UTF-8, or else Latin-1, as older programs wrote it. Reading stops
/// at what does not read as a dataset.
fn iim_properties(iim: &[u8]) -> Properties {
    let mut values: Vec<(&str, char, Vec<String>)> = Vec::new();
    let mut at = 0;
    while let (Some(&0x1c), Some(&record), Some(&dataset)) =
        (iim.get(at), iim.get(at + 1), iim.get(at + 2))
    {
        let Some(size) = iim.get(at + 3..at + 5) else {
            break;
        };
        let size = u16::from_be_bytes([size[0], size[1]]) as usize;
        // An extended dataset (the top bit set) is longer than 32 KB, which no text is.
        let Some(bytes) = iim.get(at + 5..at + 5 + size).filter(|_| size < 0x8000) else {
            break;
        };
        at += 5 + size;
        let Some(&(_, name, form)) = IIM_IN_XMP
            .iter()
            .find(|(d, _, _)| record == 2 && *d == dataset)
        else {
            continue;
        };
        let text = String::from_utf8(bytes.to_vec())
            .unwrap_or_else(|_| bytes.iter().map(|&b| b as char).collect());
        // XML cannot hold a control character, and one would make the whole packet unreadable.
        let text = text.replace(|c: char| c.is_control() && !matches!(c, '\t' | '\n'), "");
        match values.iter_mut().find(|(n, _, _)| *n == name) {
            Some((_, _, list)) => list.push(text),
            None => values.push((name, form, vec![text])),
        }
    }
    let namespaces = IIM_NAMESPACES
        .iter()
        .map(|(p, u)| ((*p).to_owned(), (*u).to_owned()));
    let namespaces = crate::metadata::xmp::Namespaces::new(namespaces.collect());
    Properties {
        list: (values.into_iter())
            .map(|(name, form, texts)| {
                let element = iim_element(name, form, texts);
                (name.to_owned(), element, namespaces.clone())
            })
            .collect(),
    }
}

/// Returns the XMP element of the property `name` in the form `form` (as in `IIM_IN_XMP`) with
/// the texts `texts`. A text in languages and a plain text take the first text only. IIM's date
/// (CCYYMMDD) is written as XMP's (CCYY-MM-DD).
fn iim_element(name: &str, form: char, texts: Vec<String>) -> String {
    use crate::metadata::xmp::escape;
    let first = texts.first().map_or("", String::as_str);
    let items = |kind: &str| {
        let lis: String = texts
            .iter()
            .map(|t| format!("<rdf:li>{}</rdf:li>", escape(t)))
            .collect();
        format!("<{name}><rdf:{kind}>{lis}</rdf:{kind}></{name}>")
    };
    match form {
        'b' => items("Bag"),
        's' => items("Seq"),
        'a' => format!(
            "<{name}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></{name}>",
            escape(first)
        ),
        _ if name == "photoshop:DateCreated" && first.len() == 8 && first.is_ascii() => {
            let date = format!("{}-{}-{}", &first[..4], &first[4..6], &first[6..]);
            format!("<{name}>{}</{name}>", escape(&date))
        }
        _ => format!("<{name}>{}</{name}>", escape(first)),
    }
}

/// Walks a PNG: each chunk up to its end (IEND). What follows IEND is left out. Returns `None`
/// when a chunk runs past the file or the first is not the header (IHDR).
fn walk_png(data: &[u8]) -> Option<Walked<'_>> {
    let mut walked = Walked::new(Container::Png);
    walked.add(data, 0, 8, None);
    let mut at = 8;
    while at < data.len() {
        let len = u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        let name: [u8; 4] = data.get(at + 4..at + 8)?.try_into().ok()?;
        let body = data.get(at + 8..at + 8 + len)?;
        if end > data.len() || (at == 8 && &name != b"IHDR") {
            return None;
        }
        let tag = match &name {
            n if PNG_IMAGE_CHUNKS.contains(&n) => Some(None),
            b"eXIf" => {
                walked.exif.get_or_insert(body);
                None
            }
            b"tEXt" | b"zTXt" | b"iTXt" => png_text_tag(&mut walked, &name, body),
            b"tIME" => Some(Some("tIME".to_owned())),
            n => Some(Some(format!("PNG chunk {}", String::from_utf8_lossy(n)))),
        };
        if let Some(tag) = tag {
            walked.add(data, at, end, tag.as_deref());
        }
        at = end;
        if &name == b"IEND" {
            break;
        }
    }
    walked.before_metadata = 2;
    (walked.parts.len() >= 2).then_some(walked)
}

/// Returns the tag of a PNG's text chunk `name` with the contents `body`: its keyword, or
/// `RAW_PROFILES` for a raw profile kept. Returns `None` for a chunk the copy writes new or
/// never carries: the XMP, read into `walked`, and the raw profiles of `REWRITTEN_PROFILES`.
/// shortcut: an XMP chunk that is compressed is not read, so its properties are not carried;
/// unpack it if a program is found that writes one.
fn png_text_tag<'a>(
    walked: &mut Walked<'a>,
    name: &[u8],
    body: &'a [u8],
) -> Option<Option<String>> {
    let keyword = &body[..body.iter().position(|&b| b == 0).unwrap_or(body.len())];
    if keyword == b"XML:com.adobe.xmp" {
        // After the keyword's NUL: not compressed (0), the method, then the language and the
        // translated keyword, each ending in NUL, and the text.
        let rest = body.get(keyword.len() + 1..);
        let plain = rest.filter(|r| name == b"iTXt" && r.first() == Some(&0));
        let text = plain.and_then(|r| {
            let mut parts = r.get(2..)?.splitn(3, |&b| b == 0);
            let _ = (parts.next()?, parts.next()?);
            parts.next()
        });
        if let Some(text) = text {
            walked.xmp.get_or_insert(text);
        }
        return None;
    }
    if let Some(kind) = keyword.strip_prefix(b"Raw profile type ") {
        let rewritten = REWRITTEN_PROFILES
            .iter()
            .any(|k| kind.eq_ignore_ascii_case(k));
        return (!rewritten).then(|| Some(RAW_PROFILES.to_owned()));
    }
    Some(Some(String::from_utf8_lossy(keyword).into_owned()))
}

/// Walks a WebP: each chunk of its RIFF file, up to the end its RIFF header gives. What follows
/// that end is not part of the image and is not copied. Returns `None` when a chunk runs past
/// the file.
fn walk_webp(data: &[u8]) -> Option<Walked<'_>> {
    let mut walked = Walked::new(Container::WebP);
    walked.add(data, 0, 12, None);
    let riff_end = 8 + u32::from_le_bytes(data.get(4..8)?.try_into().ok()?) as usize;
    let data = &data[..riff_end.min(data.len())];
    let mut at = 12;
    while at + 8 <= data.len() {
        let name: [u8; 4] = data[at..at + 4].try_into().ok()?;
        let size = u32::from_le_bytes(data[at + 4..at + 8].try_into().ok()?) as usize;
        let body = data.get(at + 8..(at + 8).checked_add(size)?)?;
        // A chunk of odd size has a padding byte, which the last one may lack.
        let end = (at + 8 + size + size % 2).min(data.len());
        match &name {
            n if WEBP_IMAGE_CHUNKS.contains(&n) => walked.add(data, at, end, None),
            b"EXIF" => {
                walked.exif.get_or_insert(body);
            }
            b"XMP " => {
                walked.xmp.get_or_insert(body);
            }
            n => {
                let tag = format!("WebP chunk {}", String::from_utf8_lossy(n));
                walked.add(data, at, end, Some(&tag));
            }
        }
        at = end;
    }
    walked.before_metadata = 1;
    Some(walked)
}

/// Makes the copy of the file `walked`: its parts that `keeps` keeps, with the new EXIF block
/// `exif` and XMP packet `xmp` where the container has them.
fn assembled(
    walked: &Walked,
    keeps: &dyn Fn(&str) -> bool,
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let kept: Vec<&Part> = (walked.parts.iter())
        .filter(|p| p.tag.as_deref().is_none_or(keeps))
        .collect();
    let (head, rest) = kept.split_at(walked.before_metadata.min(kept.len()));
    let mut out: Vec<u8> = head.iter().flat_map(|p| p.bytes.iter().copied()).collect();
    match walked.container {
        Container::Jpeg => {
            if let Some(exif) = exif {
                if exif.len() > MAX_JPEG_EXIF {
                    return Err("the EXIF block kept is too large for a JPEG".into());
                }
                out.extend(jpeg_segment(0xe1, &[b"Exif\0\0", &exif[..]].concat()));
            }
            // shortcut: a packet too long for one segment is left out, which Extended XMP
            // would hold; write that if users keep such long packets.
            if let Some(xmp) = xmp.filter(|x| 2 + XMP_NAME.len() + x.len() <= 0xffff) {
                out.extend(jpeg_segment(0xe1, &[XMP_NAME, &xmp[..]].concat()));
            }
            jpeg_rest(&mut out, rest, walked.image_end);
        }
        Container::Png => {
            if let Some(exif) = exif {
                out.extend(png_chunk(b"eXIf", &exif));
            }
            if let Some(xmp) = xmp {
                // The keyword, not compressed, no language and no translated keyword.
                let text = [&b"XML:com.adobe.xmp\0\0\0\0\0"[..], &xmp].concat();
                out.extend(png_chunk(b"iTXt", &text));
            }
            rest.iter().for_each(|p| out.extend_from_slice(&p.bytes));
        }
        Container::WebP => {
            rest.iter().for_each(|p| out.extend_from_slice(&p.bytes));
            return webp_with_metadata(out, exif, xmp);
        }
    }
    Ok(out)
}

/// Adds the JPEG parts `rest` to `out`. An MPF segment kept gets the offsets of the images
/// after the main one moved by as much as the bytes between the segment and those images
/// changed, as they are copied as they are. `image_end` is where they started in the file.
fn jpeg_rest(out: &mut Vec<u8>, rest: &[&Part], image_end: Option<usize>) {
    // (where its contents start in `out`, and did in the file, and their length).
    let mut mpf = None;
    let mut new_end = None;
    for part in rest {
        if part.tag.as_deref() == Some(MPF) && part.bytes.get(4..8) == Some(&b"MPF\0"[..]) {
            mpf.get_or_insert((out.len() + 4, part.from + 4, part.bytes.len() - 4));
        }
        if Some(part.from) == image_end {
            new_end = Some(out.len());
        }
        out.extend_from_slice(&part.bytes);
    }
    if let (Some((new_at, old_at, len)), Some(new_end), Some(old_end)) = (mpf, new_end, image_end) {
        // The offsets count from the TIFF header, four bytes into the contents.
        let by = (new_end as i64 - new_at as i64) - (old_end as i64 - old_at as i64);
        let _ = crate::metadata::exif::shift_mp_offsets(
            &mut out[new_at..new_at + len],
            by,
            new_end as u32,
        );
    }
}

/// Returns the JPEG segment of marker `marker` with the contents `body`, which fit in one.
fn jpeg_segment(marker: u8, body: &[u8]) -> Vec<u8> {
    let len = (2 + body.len()) as u16;
    [&[0xff, marker][..], &len.to_be_bytes(), body].concat()
}

/// Returns the PNG chunk `name` with the contents `body` and its CRC.
fn png_chunk(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let named = [&name[..], body].concat();
    let crc = crc32(&named).to_be_bytes();
    [&(body.len() as u32).to_be_bytes()[..], &named, &crc].concat()
}

/// Returns the WebP `webp` with the new EXIF block `exif` and XMP packet `xmp`. Its extended
/// header's flags for EXIF and XMP are cleared first, as the old ones are not in it, and set
/// again for those added. Its RIFF size is set to what it holds.
fn webp_with_metadata(
    mut webp: Vec<u8>,
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    if webp.get(12..16) == Some(&b"VP8X"[..])
        && let Some(flags) = webp.get_mut(20)
    {
        *flags &= !0x0c;
    }
    let size = (webp.len() - 8) as u32;
    webp[4..8].copy_from_slice(&size.to_le_bytes());
    if exif.is_none() && xmp.is_none() {
        return Ok(webp);
    }
    // A simple WebP gets an extended header, which needs the image's size.
    let (width, height) = image::ImageReader::new(std::io::Cursor::new(&webp))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    for (name, data) in [(b"EXIF", exif), (b"XMP ", xmp)] {
        if let Some(data) = data {
            webp = crate::images::save::webp_with_chunk(&webp, name, &data, width, height)?;
        }
    }
    Ok(webp)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    /// The EXIF block of the tests, big-endian: Make "Canon", orientation 6, a body serial
    /// number in the Exif directory and a map datum in the GPS directory, each a text that is
    /// found nowhere else.
    fn exif_block() -> Vec<u8> {
        let entry = |d: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            d.extend(tag.to_be_bytes());
            d.extend(kind.to_be_bytes());
            d.extend(count.to_be_bytes());
            d.extend(value.to_be_bytes());
        };
        let mut d = b"MM\0*\0\0\0\x08".to_vec();
        // IFD0 at 8, four entries, to 62; "Canon" at 62.
        d.extend(4u16.to_be_bytes());
        entry(&mut d, 0x010f, 2, 6, 62);
        entry(&mut d, 0x0112, 3, 1, 6 << 16);
        entry(&mut d, 0x8769, 4, 1, 68);
        entry(&mut d, 0x8825, 4, 1, 96);
        d.extend(0u32.to_be_bytes());
        d.extend(b"Canon\0");
        // The Exif directory at 68, to 86, and the serial number at 86, to 96.
        d.extend(1u16.to_be_bytes());
        entry(&mut d, 0xa431, 2, 9, 86);
        d.extend(0u32.to_be_bytes());
        d.extend(b"SN-77777\0\0");
        // The GPS directory at 96, to 114, and its map datum at 114.
        d.extend(1u16.to_be_bytes());
        entry(&mut d, 0x0012, 2, 14, 114);
        d.extend(0u32.to_be_bytes());
        d.extend(b"WGS-84-SECRET\0");
        d
    }

    /// The XMP packet inside the test files: a latitude and a keyword.
    const XMP: &str = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
        xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
        rdf:about=\"\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\" \
        xmlns:dc=\"http://purl.org/dc/elements/1.1/\" exif:GPSLatitude=\"59,55.123N\">\
        <dc:subject><rdf:Bag><rdf:li>harbour</rdf:li></rdf:Bag></dc:subject>\
        </rdf:Description></rdf:RDF></x:xmpmeta>";

    /// The sidecar of the test files: an alt text written in Glimt for publishing.
    const SIDECAR: &str = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
        xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
        rdf:about=\"\" xmlns:Iptc4xmpCore=\"http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/\">\
        <Iptc4xmpCore:AltTextAccessibility><rdf:Alt><rdf:li xml:lang=\"x-default\">A boat\
        </rdf:li></rdf:Alt></Iptc4xmpCore:AltTextAccessibility></rdf:Description></rdf:RDF>\
        </x:xmpmeta>";

    /// The texts a copy that leaves out the place and the serial number must not hold.
    const LEFT_OUT: [&[u8]; 4] = [b"WGS-84-SECRET", b"59,55.123N", b"SN-77777", b"Oslo-Secret"];

    /// The texts it must still hold: the make, the keyword and the alt text of the sidecar.
    const KEPT: [&[u8]; 3] = [b"Canon", b"harbour", b"A boat"];

    /// A picture of 16×16 with a gradient, so its pixels differ.
    fn picture() -> image::RgbImage {
        image::RgbImage::from_fn(16, 16, |x, y| image::Rgb([x as u8 * 16, y as u8 * 16, 99]))
    }

    /// A folder of its own for a test.
    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("glimt-shared-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes `bytes` as `name` in `dir`, with the test sidecar, and copies it leaving out the
    /// place and the serial number. Returns the copy.
    fn shared(dir: &std::path::Path, name: &str, bytes: &[u8]) -> Vec<u8> {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(crate::metadata::xmp::sidecar(&path), SIDECAR).unwrap();
        let found = super::tags_of(&path).unwrap().unwrap();
        for tag in [
            "GPSMapDatum",
            "exif:GPSLatitude",
            "BodySerialNumber",
            "Make",
        ] {
            assert!(found.iter().any(|t| t == tag), "{tag} not in {found:?}");
        }
        let mut left_out = super::LeftOut::default();
        left_out.set_group(super::Group::Place, true);
        left_out.set_tag("BodySerialNumber", true, &found);
        super::copy(&path, &left_out.kept(&found)).unwrap().unwrap()
    }

    /// Checks the copy `copy` of `original`: no text left out is anywhere in it, every text kept
    /// is, and it decodes to the same pixels.
    fn check(original: &[u8], copy: &[u8]) {
        let has = |text: &[u8]| copy.windows(text.len()).any(|w| w == text);
        // The original holds them, so their absence in the copy says something.
        let had = |text: &[u8]| original.windows(text.len()).any(|w| w == text);
        assert!(LEFT_OUT[..3].iter().chain(&KEPT[..1]).all(|t| had(t)));
        for text in LEFT_OUT {
            assert!(
                !has(text),
                "{} is in the copy",
                String::from_utf8_lossy(text)
            );
        }
        for text in KEPT.iter().chain([&&b"\x01\x12\0\x03\0\0\0\x01\0\x06"[..]]) {
            assert!(
                has(text),
                "{} is not in the copy",
                String::from_utf8_lossy(text)
            );
        }
        let pixels = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgba8();
        assert!(pixels(original) == pixels(copy));
    }

    /// A JPEG copy keeps its image data byte for byte: everything from the first scan to the
    /// end. The place (in EXIF, XMP and IPTC) and the serial number are gone, and the make, the
    /// keyword, the sidecar's alt text, the orientation and the IPTC creator are kept.
    #[test]
    fn a_jpeg_copy_leaves_out_what_was_chosen() {
        let dir = folder("jpeg");
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
        picture().write_with_encoder(encoder).unwrap();
        let iim = [
            &[0x1c, 2, 80, 0, 12][..],
            b"Ola Nordmann",
            &[0x1c, 2, 90, 0, 11],
            b"Oslo-Secret",
        ]
        .concat();
        let resource = [
            &b"8BIM\x04\x04\0\0"[..],
            &(iim.len() as u32).to_be_bytes(),
            &iim,
        ]
        .concat();
        let segments = [
            super::jpeg_segment(0xe1, &[&b"Exif\0\0"[..], &exif_block()].concat()),
            super::jpeg_segment(0xe1, &[super::XMP_NAME, XMP.as_bytes()].concat()),
            super::jpeg_segment(0xed, &[super::PHOTOSHOP_NAME, &resource].concat()),
            super::jpeg_segment(0xfe, b"a note"),
        ]
        .concat();
        let original = [&jpeg[..2], &segments, &jpeg[2..]].concat();
        let copy = shared(&dir, "a.jpg", &original);
        check(&original, &copy);
        let scans = crate::metadata::exif::jpeg_scan_start(&jpeg).unwrap();
        assert!(copy.ends_with(&jpeg[scans..]), "the image data changed");
        let has = |text: &[u8]| copy.windows(text.len()).any(|w| w == text);
        assert!(has(b"Ola Nordmann") && has(b"a note"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A PNG copy keeps every IDAT chunk byte for byte, and the text chunks kept; the place and
    /// the serial number are gone from its eXIf and XMP.
    #[test]
    fn a_png_copy_leaves_out_what_was_chosen() {
        let dir = folder("png");
        let mut png = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png);
        picture().write_with_encoder(encoder).unwrap();
        let chunks = [
            super::png_chunk(b"eXIf", &exif_block()),
            super::png_chunk(
                b"iTXt",
                &[&b"XML:com.adobe.xmp\0\0\0\0\0"[..], XMP.as_bytes()].concat(),
            ),
            super::png_chunk(b"tEXt", b"Author\0Kari"),
        ]
        .concat();
        let original = [&png[..33], &chunks, &png[33..]].concat();
        let copy = shared(&dir, "a.png", &original);
        check(&original, &copy);
        let idat = |b: &[u8]| -> Vec<Vec<u8>> {
            let walked = super::walk_png(b).unwrap();
            let parts = walked.parts.into_iter().map(|p| p.bytes.into_owned());
            parts
                .filter(|p| p.get(4..8) == Some(&b"IDAT"[..]))
                .collect()
        };
        assert!(!idat(&original).is_empty() && idat(&original) == idat(&copy));
        assert!(copy.windows(4).any(|w| w == b"Kari"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A WebP copy keeps its image chunk byte for byte; the place and the serial number are gone
    /// from its EXIF and XMP chunks.
    #[test]
    fn a_webp_copy_leaves_out_what_was_chosen() {
        let dir = folder("webp");
        let mut webp = Vec::new();
        let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut webp);
        picture().write_with_encoder(encoder).unwrap();
        let save = crate::images::save::webp_with_chunk;
        let with_exif = save(&webp, b"EXIF", &exif_block(), 16, 16).unwrap();
        let original = save(&with_exif, b"XMP ", XMP.as_bytes(), 16, 16).unwrap();
        let copy = shared(&dir, "a.webp", &original);
        check(&original, &copy);
        let image_chunk = &webp[12..];
        assert!(copy.windows(image_chunk.len()).any(|w| w == image_chunk));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A choice keeps what it says: a group left out whole leaves out a tag never seen before,
    /// a tag kept out of it leaves the others out one by one, and Remove All leaves out every
    /// tag. It reads back from its text as it was.
    #[test]
    fn the_choice_leaves_out_what_was_ticked() {
        let found: Vec<String> = ["GPSLatitude", "GPSAltitude", "Make", "xmp:Rating"]
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        let mut left_out = super::LeftOut::default();
        left_out.set_group(super::Group::Place, true);
        assert!(left_out.is_left_out("GPS 0x001e") && left_out.is_left_out("exif:GPSSpeed"));
        left_out.set_tag("GPSAltitude", false, &found);
        assert!(left_out.is_left_out("GPSLatitude") && !left_out.is_left_out("GPSAltitude"));
        assert!(!left_out.is_group_left_out(super::Group::Place));
        let kept = left_out.kept(&found);
        let want: HashSet<String> = ["GPSAltitude", "Make", "xmp:Rating"]
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        assert_eq!(kept, want);
        assert_eq!(super::LeftOut::parse(&left_out.text()), left_out);
        left_out.remove_all();
        assert!(left_out.kept(&found).is_empty());
        left_out.set_group(super::Group::Other, false);
        assert_eq!(left_out.kept(&found), ["xmp:Rating".to_owned()].into());
    }

    /// Ticking only Place leaves no GPS anywhere in the copy: not in the EXIF of a phone's
    /// second image (MPF) after the main one, not in a drone's own XMP properties
    /// (`drone-dji:`), and not in Exif's GPS properties under another prefix (`ns1:`).
    #[test]
    fn place_alone_leaves_no_gps_anywhere() {
        let dir = folder("place");
        let encode = |size: u32| {
            let mut jpeg = Vec::new();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
            image::RgbImage::from_fn(size, size, |x, _| image::Rgb([x as u8 * 9, 40, 99]))
                .write_with_encoder(encoder)
                .unwrap();
            jpeg
        };
        let (main, second) = (encode(16), encode(8));
        let exif = super::jpeg_segment(0xe1, &[&b"Exif\0\0"[..], &exif_block()].concat());
        let second = [&second[..2], &exif, &second[2..]].concat();
        let mpf = b"MPF\0MM\0*\0\0\0\x08\0\x01\xb0\x02\0\x07\0\0\0\x20\0\0\0\x1a\0\0\0\0\
            \x20\0\0\0\0\0\x01\0\0\0\0\0\0\x10\0\0\0\0\0\0\0\0";
        let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
            xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
            rdf:about=\"\" xmlns:drone-dji=\"http://www.dji.com/drone-dji/1.0/\" \
            xmlns:ns1=\"http://ns.adobe.com/exif/1.0/\" drone-dji:GpsLatitude=\"+59.9123\" \
            drone-dji:GpsLongtitude=\"+10.7522\" drone-dji:RelativeAltitude=\"+87.5\" \
            ns1:GPSAltitude=\"8750/100\" drone-dji:FlightRollDegree=\"+1.20\"/>\
            </rdf:RDF></x:xmpmeta>";
        let segments = [
            super::jpeg_segment(0xe2, mpf),
            super::jpeg_segment(0xe1, &[super::XMP_NAME, xmp.as_bytes()].concat()),
        ]
        .concat();
        let original = [&main[..2], &segments, &main[2..], &second].concat();
        let path = dir.join("drone.jpg");
        std::fs::write(&path, &original).unwrap();
        let found = super::tags_of(&path).unwrap().unwrap();
        let mut left_out = super::LeftOut::default();
        left_out.set_group(super::Group::Place, true);
        let copy = super::copy(&path, &left_out.kept(&found)).unwrap().unwrap();
        let has = |b: &[u8], text: &[u8]| b.windows(text.len()).any(|w| w == text);
        for gps in [
            &b"WGS-84-SECRET"[..],
            b"59.9123",
            b"10.7522",
            b"87.5",
            b"8750/100",
        ] {
            assert!(has(&original, gps));
            let gps_text = String::from_utf8_lossy(gps);
            assert!(!has(&copy, gps), "{gps_text} is in the copy");
        }
        // The roll is no place, so it stays.
        assert!(has(&copy, b"+1.20"));
        let pixels = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgba8();
        assert!(pixels(&original) == pixels(&copy));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A block that can hold tags of other groups is left out with any tag of those groups,
    /// and the names that say where, when or who are in their groups whatever their prefix.
    #[test]
    fn blocks_go_with_any_group_they_can_carry() {
        use super::Group::{Date, Keywords, Photographer, Place, Software};
        let mut left_out = super::LeftOut::default();
        let blocks = [
            super::MPF,
            super::AFTER_IMAGE,
            "MakerNote",
            super::PHOTOSHOP_RESOURCES,
            super::C2PA,
            "XMLPacket",
            "IPTC-NAA",
            super::RAW_PROFILES,
            "JPEG APP12",
            "PNG chunk caBX",
            "WebP chunk XYZW",
            "Image 0x8649",
        ];
        assert!(blocks.iter().all(|b| !left_out.is_left_out(b)));
        left_out.set_tag("GPSAltitude", true, &[]);
        for block in blocks {
            assert!(left_out.is_left_out(block), "{block} is kept");
        }
        assert!(!left_out.is_left_out(super::JFIF_THUMBNAIL) && !left_out.is_left_out("Make"));
        left_out.set_tag(crate::metadata::exif::EXIF_THUMBNAIL, true, &[]);
        assert!(left_out.is_left_out(super::JFIF_THUMBNAIL));
        let mut dated = super::LeftOut::default();
        dated.set_tag("DateTime", true, &[]);
        assert!(dated.is_left_out("xmpMM:DerivedFrom") && !dated.is_left_out("Make"));
        for (tag, group) in [
            ("drone-dji:GpsLatitude", Place),
            ("drone-dji:GpsLongtitude", Place),
            ("drone-dji:AbsoluteAltitude", Place),
            ("dc:date", Date),
            ("exif:SubSecTimeOriginal", Date),
            ("exifEX:OffsetTimeOriginal", Date),
            ("digiKam:TagsList", Keywords),
            ("MicrosoftPhoto:LastKeywordXMP", Keywords),
            ("MicrosoftPhoto:LastKeywordIPTC", Keywords),
            ("mwg-rs:Regions", Keywords),
            ("Iptc4xmpExt:PersonInImage", Keywords),
        ] {
            assert_eq!(super::Group::groups_of(tag), [group], "{tag}");
        }
        for block in ["xmpMM:DerivedFrom", "xmpMM:Ingredients"] {
            let groups = super::Group::groups_of(block);
            assert_eq!(groups, [Date, Photographer, Software], "{block}");
        }
    }

    /// Real tags, from exiftool's tables and from what phones, Lightroom and Windows write, are
    /// left out by the box of each group they say something of, and by no other. A tag that
    /// says when as well as where, or a file name that holds the date and a path that names the
    /// user, goes with each of its groups.
    #[test]
    fn each_tag_goes_with_every_group_it_belongs_to() {
        use super::Group::{self, Keywords};
        use super::Group::{Camera, Date, Other, Photographer, Place, Preview, Software};
        let tags: [(&str, &[Group]); 50] = [
            ("DateTimeOriginal", &[Date]),
            ("DateTime", &[Date]),
            ("OffsetTimeOriginal", &[Date]),
            ("SubSecTimeOriginal", &[Date]),
            ("xmp:CreateDate", &[Date]),
            ("xmp:MetadataDate", &[Date]),
            ("photoshop:DateCreated", &[Date]),
            ("MicrosoftPhoto:DateAcquired", &[Date]),
            ("GPano:FirstPhotoDate", &[Date]),
            ("GPano:LastPhotoDate", &[Date]),
            ("tIME", &[Date]),
            ("GPSDateStamp", &[Place, Date]),
            ("GPSTimeStamp", &[Place, Date]),
            ("exif:GPSTimeStamp", &[Place, Date]),
            ("GPSLatitude", &[Place]),
            ("GPSProcessingMethod", &[Place]),
            ("drone-dji:AbsoluteAltitude", &[Place]),
            ("photoshop:City", &[Place]),
            ("photoshop:State", &[Place]),
            ("Iptc4xmpCore:Location", &[Place]),
            ("Iptc4xmpExt:LocationShown", &[Place]),
            ("Make", &[Camera]),
            ("LensModel", &[Camera]),
            ("BodySerialNumber", &[Camera]),
            ("aux:SerialNumber", &[Camera]),
            ("aux:Lens", &[Camera]),
            ("GCamera:MotionPhotoPresentationTimestampUs", &[Camera]),
            ("aux:OwnerName", &[Photographer]),
            ("CameraOwnerName", &[Photographer]),
            ("Artist", &[Photographer]),
            ("dc:creator", &[Photographer]),
            ("dc:rights", &[Photographer]),
            ("xmpRights:UsageTerms", &[Photographer]),
            ("photoshop:Credit", &[Photographer]),
            ("crs:RawFileName", &[Date, Photographer]),
            ("xmpMM:PreservedFileName", &[Date, Photographer]),
            ("Software", &[Software]),
            ("xmp:CreatorTool", &[Software]),
            ("photoshop:History", &[Date, Software]),
            ("dc:subject", &[Keywords]),
            ("XPKeywords", &[Keywords]),
            ("Iptc4xmpCore:AltTextAccessibility", &[Keywords]),
            (crate::metadata::exif::EXIF_THUMBNAIL, &[Preview]),
            ("ExposureTime", &[Other]),
            ("exif:ExposureTime", &[Other]),
            ("SubjectDistance", &[Other]),
            ("SubjectLocation", &[Other]),
            ("FocalLength", &[Other]),
            ("xmp:Rating", &[Other]),
            ("xmpMM:DocumentID", &[Other]),
        ];
        let mut wrong = Vec::new();
        for (tag, groups) in tags {
            for group in Group::ALL {
                let mut left_out = super::LeftOut::default();
                left_out.set_group(group, true);
                if left_out.is_left_out(tag) != groups.contains(&group) {
                    wrong.push(format!("{tag} with {group:?}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// A tag of two groups that is kept out of one group left out whole stays kept, and the
    /// other tags of that group are left out one by one instead.
    #[test]
    fn a_tag_of_two_groups_can_be_kept() {
        let found: Vec<String> = ["GPSDateStamp", "DateTimeOriginal", "GPSLatitude"]
            .map(str::to_owned)
            .to_vec();
        let mut left_out = super::LeftOut::default();
        left_out.set_group(super::Group::Date, true);
        assert!(left_out.is_left_out("GPSDateStamp"));
        left_out.set_tag("GPSDateStamp", false, &found);
        assert!(!left_out.is_left_out("GPSDateStamp"));
        assert!(left_out.is_left_out("DateTimeOriginal") && !left_out.is_left_out("GPSLatitude"));
    }

    /// A thumbnail inside a JPEG's JFIF header is a tag of the preview group: it goes with
    /// Remove All and leaves the header with no thumbnail, and kept it moves into a JFXX
    /// segment of its own.
    #[test]
    fn a_jfif_thumbnail_is_a_preview() {
        let dir = folder("jfif");
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
        picture().write_with_encoder(encoder).unwrap();
        // The encoder's own JFIF header, if it wrote one, is replaced by one with a thumbnail of
        // 2×1 pixels.
        let rest = match jpeg[2..4] {
            [0xff, 0xe0] => 4 + u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize,
            _ => 2,
        };
        let jfif = super::jpeg_segment(0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\x02\x01TH-UMB");
        let original = [&jpeg[..2], &jfif, &jpeg[rest..]].concat();
        let path = dir.join("a.jpg");
        std::fs::write(&path, &original).unwrap();
        let found = super::tags_of(&path).unwrap().unwrap();
        assert!(
            found.iter().any(|t| t == super::JFIF_THUMBNAIL),
            "{found:?}"
        );
        let has = |b: &[u8], text: &[u8]| b.windows(text.len()).any(|w| w == text);
        let mut left_out = super::LeftOut::default();
        let kept = super::copy(&path, &left_out.kept(&found)).unwrap().unwrap();
        assert!(has(&kept, b"JFXX\0\x13\x02\x01TH-UMB"));
        left_out.remove_all();
        let copy = super::copy(&path, &left_out.kept(&found)).unwrap().unwrap();
        assert!(!has(&copy, b"TH-UMB") && has(&copy, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        let pixels = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgba8();
        assert!(pixels(&original) == pixels(&copy) && pixels(&original) == pixels(&kept));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A JPEG with bytes between its segments, as some phones and editors write, and a WebP
    /// with bytes after its RIFF file are copied as decoders read them, without those bytes.
    #[test]
    fn copies_files_with_bytes_between_their_parts() {
        let dir = folder("junk");
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
        picture().write_with_encoder(encoder).unwrap();
        let comment = super::jpeg_segment(0xfe, b"a note");
        let junky = [&jpeg[..2], &comment, b"\0\0JUNK\xff\0", &jpeg[2..]].concat();
        let mut webp = Vec::new();
        let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut webp);
        picture().write_with_encoder(encoder).unwrap();
        let trailing = [&webp[..], b"TRAILERJUNK"].concat();
        for (name, original) in [("a.jpg", junky), ("a.webp", trailing)] {
            let path = dir.join(name);
            std::fs::write(&path, &original).unwrap();
            let found = super::tags_of(&path).unwrap().unwrap();
            let copy = super::copy(&path, &found.into_iter().collect())
                .unwrap()
                .unwrap();
            let has = |text: &[u8]| copy.windows(text.len()).any(|w| w == text);
            assert!(!has(b"JUNK") && !has(b"TRAILER"), "{name}");
            let pixels = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgba8();
            assert!(pixels(&original) == pixels(&copy), "{name}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// IPTC text with control characters, which XML cannot hold, is carried into the XMP
    /// without them, so the packet stays XML and its other properties are read.
    #[test]
    fn iptc_text_is_carried_without_control_characters() {
        let iim = [&[0x1c, 2, 80, 0, 13][..], b"Ola\x07Nord\x1bmann"].concat();
        let read = super::iim_properties(&iim);
        let element = &read.list[0].1;
        assert!(element.contains("OlaNordmann"), "{element}");
        assert!(!element.chars().any(|c| c.is_control()), "{element:?}");
    }

    /// Writes a file of `size` bytes at `path` with `parts` at their places and holes between
    /// them, so a test has a file of hundreds of megabytes that takes no room on the disk.
    fn sparse(path: &std::path::Path, size: u64, parts: &[(u64, &[u8])]) {
        use std::io::{Seek, Write};
        let mut file = std::fs::File::create(path).unwrap();
        file.set_len(size).unwrap();
        for (at, bytes) in parts {
            file.seek(std::io::SeekFrom::Start(*at)).unwrap();
            file.write_all(bytes).unwrap();
        }
    }

    /// A video larger than a decode reads is another format, not an error, and its tags are
    /// not looked for.
    #[test]
    fn a_large_video_is_another_format() {
        let dir = folder("video");
        let path = dir.join("a.mp4");
        sparse(&path, 600 << 20, &[(0, b"\0\0\0\x18ftypmp42\0\0\0\0")]);
        assert_eq!(super::tags_of(&path), Ok(None));
        assert_eq!(super::copy(&path, &HashSet::new()), Ok(None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The tags of a JPEG, a PNG and a WebP larger than a decode reads are read from their
    /// metadata alone, also those after the image data, and what follows a JPEG's image.
    #[test]
    fn reads_the_tags_of_large_files_without_their_image_data() {
        let dir = folder("large");
        let size: u64 = 600 << 20;
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
        picture().write_with_encoder(encoder).unwrap();
        let exif = super::jpeg_segment(0xe1, &[&b"Exif\0\0"[..], &exif_block()].concat());
        let jpeg = [&jpeg[..2], &exif, &jpeg[2..]].concat();
        sparse(&dir.join("a.jpg"), size, &[(0, &jpeg)]);
        let mut png = Vec::new();
        picture()
            .write_with_encoder(image::codecs::png::PngEncoder::new(&mut png))
            .unwrap();
        let before = super::png_chunk(b"tEXt", b"Author\0Kari");
        let idat = [&((size as u32) - 200).to_be_bytes()[..], b"IDAT"].concat();
        let after = [
            super::png_chunk(b"tEXt", b"Comment\0late"),
            super::png_chunk(b"IEND", b""),
        ]
        .concat();
        let head = [&png[..33], &before, &idat].concat();
        let end = head.len() as u64 + size - 200 + 4;
        sparse(
            &dir.join("a.png"),
            end + after.len() as u64,
            &[(0, &head), (end, &after)],
        );
        let image_size = (size as u32) - 200;
        let xmp = [
            &b"XMP "[..],
            &(XMP.len() as u32).to_le_bytes(),
            XMP.as_bytes(),
        ]
        .concat();
        let riff = 4 + 8 + image_size + xmp.len() as u32;
        let head = [
            &b"RIFF"[..],
            &riff.to_le_bytes(),
            b"WEBPVP8L",
            &image_size.to_le_bytes(),
        ];
        let head = head.concat();
        let end = head.len() as u64 + image_size as u64;
        sparse(
            &dir.join("a.webp"),
            end + xmp.len() as u64,
            &[(0, &head), (end, &xmp)],
        );
        for (name, wanted) in [
            ("a.jpg", &["Make", "GPSMapDatum", super::AFTER_IMAGE][..]),
            ("a.png", &["Author", "Comment"]),
            ("a.webp", &["dc:subject", "exif:GPSLatitude"]),
        ] {
            let found = super::tags_of(&dir.join(name)).unwrap().unwrap();
            for tag in wanted {
                assert!(
                    found.iter().any(|t| t == tag),
                    "{name}: {tag} not in {found:?}"
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The walkers of JPEG segments, PNG chunks and WebP chunks, and the copy made of what they
    /// found, never panic on files broken in thousands of ways. Nor does the reader of
    /// Photoshop resources and IPTC IIM.
    #[test]
    fn survives_broken_jpeg_png_and_webp() {
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90);
        image::RgbImage::new(8, 8)
            .write_with_encoder(encoder)
            .unwrap();
        let mpf = b"MPF\0MM\0*\0\0\0\x08\0\x01\xb0\x02\0\x07\0\0\0\x20\0\0\0\x1a\0\0\0\0\
            \x20\0\0\0\0\0\x01\0\0\0\0\0\0\x10\0\0\0\0\0\0\0\0";
        let segments = [
            super::jpeg_segment(0xe2, mpf),
            super::jpeg_segment(
                0xed,
                b"Photoshop 3.0\08BIM\x04\x04\0\0\0\0\0\x05\x1c\x02\x19\0\x00\0",
            ),
            super::jpeg_segment(0xfe, b"a note"),
        ]
        .concat();
        let jpeg = [&jpeg[..2], &segments, &jpeg[2..], b"after"].concat();
        let mut png = Vec::new();
        image::RgbImage::new(8, 8)
            .write_with_encoder(image::codecs::png::PngEncoder::new(&mut png))
            .unwrap();
        let text = super::png_chunk(b"iTXt", b"XML:com.adobe.xmp\0\0\0\0\0<x/>");
        let png = [&png[..33], &text, &png[33..]].concat();
        let mut webp = Vec::new();
        image::RgbImage::new(8, 8)
            .write_with_encoder(image::codecs::webp::WebPEncoder::new_lossless(&mut webp))
            .unwrap();
        let webp = crate::images::save::webp_with_chunk(&webp, b"XMP ", b"<x/>", 8, 8).unwrap();
        let keeps = |_: &str| true;
        for seed in [jpeg, png, webp] {
            for data in crate::support::test_support::broken(&seed) {
                if let Ok(Some(walked)) = super::walk(&data) {
                    let _ = super::assembled(&walked, &keeps, Some(vec![0; 9]), Some(vec![1; 9]));
                }
            }
        }
        let resources = b"8BIM\x04\x04\0\0\0\0\0\x0a\x1c\x02\x50\0\x05Kari \x1c\x02";
        for data in crate::support::test_support::broken(resources) {
            for (_, _, data) in super::photoshop_resources(&data).unwrap_or_default() {
                super::iim_properties(data);
            }
            super::iim_properties(&data);
        }
    }
}
