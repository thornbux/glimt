//! Copy for Sharing's copies: a JPEG, PNG or WebP with its image data byte for byte and only the
//! metadata the user keeps. The file is walked part by part (`walk_jpeg`, `walk_png`,
//! `walk_webp`), and the copy is made new of the parts kept (`assembled`): an allow-list, so
//! nothing the user did not keep can come along. The EXIF block and the XMP packet are written
//! new of the tags kept (`save::shared_metadata`), with the sidecar's properties in the XMP. The
//! tags the dialog lists come from the same walk (`tags_of`), so the list and the copy agree.

use crate::images::save::{MAX_JPEG_EXIF, SharedMetadata, crc32, shared_metadata};
use crate::metadata::exif::EXIF_THUMBNAIL;
use crate::metadata::xmp::Properties;
use crate::support::lang::tr;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

/// The groups that tags with these names belong to. The names are the ones files give tags:
/// EXIF's (`exif::tag_name`), XMP's, a PNG's text keywords, and a few for whole blocks. A name
/// not here falls in a group by its prefix (`Group::of`), or in More tags.
const TAG_GROUPS: &[(&str, Group)] = &[
    ("photoshop:City", Group::Place),
    ("photoshop:State", Group::Place),
    ("photoshop:Country", Group::Place),
    ("Iptc4xmpCore:Location", Group::Place),
    ("Iptc4xmpCore:CountryCode", Group::Place),
    ("Make", Group::Camera),
    ("Model", Group::Camera),
    ("LensMake", Group::Camera),
    ("LensModel", Group::Camera),
    ("LensSpecification", Group::Camera),
    ("BodySerialNumber", Group::Camera),
    ("LensSerialNumber", Group::Camera),
    ("MakerNote", Group::Camera),
    ("CameraFirmware", Group::Camera),
    ("ImageUniqueID", Group::Camera),
    ("tiff:Make", Group::Camera),
    ("tiff:Model", Group::Camera),
    ("exif:ImageUniqueID", Group::Camera),
    ("exifEX:BodySerialNumber", Group::Camera),
    ("exifEX:LensMake", Group::Camera),
    ("exifEX:LensModel", Group::Camera),
    ("exifEX:LensSerialNumber", Group::Camera),
    ("exifEX:LensSpecification", Group::Camera),
    ("exifEX:ImageUniqueID", Group::Camera),
    ("DateTime", Group::Date),
    ("DateTimeOriginal", Group::Date),
    ("DateTimeDigitized", Group::Date),
    ("OffsetTime", Group::Date),
    ("OffsetTimeOriginal", Group::Date),
    ("OffsetTimeDigitized", Group::Date),
    ("SubSecTime", Group::Date),
    ("SubSecTimeOriginal", Group::Date),
    ("SubSecTimeDigitized", Group::Date),
    ("xmp:CreateDate", Group::Date),
    ("xmp:ModifyDate", Group::Date),
    ("xmp:MetadataDate", Group::Date),
    ("exif:DateTimeOriginal", Group::Date),
    ("exif:DateTimeDigitized", Group::Date),
    ("tiff:DateTime", Group::Date),
    ("photoshop:DateCreated", Group::Date),
    ("dc:date", Group::Date),
    ("Creation Time", Group::Date),
    ("tIME", Group::Date),
    ("date:create", Group::Date),
    ("date:modify", Group::Date),
    ("date:timestamp", Group::Date),
    ("Artist", Group::Photographer),
    ("Copyright", Group::Photographer),
    ("CameraOwnerName", Group::Photographer),
    ("Photographer", Group::Photographer),
    ("XPAuthor", Group::Photographer),
    ("Author", Group::Photographer),
    ("dc:creator", Group::Photographer),
    ("dc:rights", Group::Photographer),
    ("dc:publisher", Group::Photographer),
    ("dc:contributor", Group::Photographer),
    ("tiff:Artist", Group::Photographer),
    ("tiff:Copyright", Group::Photographer),
    ("exifEX:CameraOwnerName", Group::Photographer),
    ("photoshop:Credit", Group::Photographer),
    ("photoshop:AuthorsPosition", Group::Photographer),
    ("photoshop:Source", Group::Photographer),
    ("photoshop:CaptionWriter", Group::Photographer),
    ("Iptc4xmpCore:CreatorContactInfo", Group::Photographer),
    ("Software", Group::Software),
    ("ProcessingSoftware", Group::Software),
    ("HostComputer", Group::Software),
    ("ImageEditor", Group::Software),
    ("RAWDevelopingSoftware", Group::Software),
    ("ImageEditingSoftware", Group::Software),
    ("MetadataEditingSoftware", Group::Software),
    ("tiff:Software", Group::Software),
    ("xmp:CreatorTool", Group::Software),
    ("xmpMM:History", Group::Software),
    ("photoshop:History", Group::Software),
    ("xmpMM:DerivedFrom", Group::Software),
    ("xmpMM:Ingredients", Group::Software),
    ("xmpMM:PreservedFileName", Group::Software),
    ("ImageDescription", Group::Keywords),
    ("UserComment", Group::Keywords),
    ("ImageTitle", Group::Keywords),
    ("XPTitle", Group::Keywords),
    ("XPComment", Group::Keywords),
    ("XPKeywords", Group::Keywords),
    ("XPSubject", Group::Keywords),
    ("Comment", Group::Keywords),
    ("Description", Group::Keywords),
    ("Title", Group::Keywords),
    ("JPEG comment", Group::Keywords),
    ("dc:subject", Group::Keywords),
    ("dc:description", Group::Keywords),
    ("dc:title", Group::Keywords),
    ("lr:hierarchicalSubject", Group::Keywords),
    ("Iptc4xmpCore:AltTextAccessibility", Group::Keywords),
    ("Iptc4xmpCore:ExtDescrAccessibility", Group::Keywords),
    ("photoshop:Headline", Group::Keywords),
    ("photoshop:Instructions", Group::Keywords),
    ("exif:UserComment", Group::Keywords),
    ("tiff:ImageDescription", Group::Keywords),
    ("digiKam:TagsList", Group::Keywords),
    ("mwg-rs:Regions", Group::Keywords),
    ("Iptc4xmpExt:PersonInImage", Group::Keywords),
    (EXIF_THUMBNAIL, Group::Preview),
    (JFIF_THUMBNAIL, Group::Preview),
    ("xmp:Thumbnails", Group::Preview),
    ("xap:Thumbnails", Group::Preview),
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
    // The paths and names of the files that a photo was made from. A path names the user, and
    // a name such as PXL_20231004_142355 holds the date.
    (
        "xmpMM:DerivedFrom",
        &[Group::Software, Group::Date, Group::Photographer],
    ),
    (
        "xmpMM:Ingredients",
        &[Group::Software, Group::Date, Group::Photographer],
    ),
    (
        "xmpMM:PreservedFileName",
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

/// The words that make a tag one of Place, whatever its namespace or prefix: a drone's
/// `drone-dji:GpsLatitude` and `AbsoluteAltitude` as much as EXIF's GPS directory.
const PLACE_WORDS: [&str; 4] = ["gps", "latitude", "longitude", "altitude"];

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

    /// The group of the tag named `tag`: by its name in `TAG_GROUPS`, or else by its words and
    /// its prefix. An XMP property's prefix is that of its namespace (`xmp::properties`), not
    /// the one its packet wrote. A name with a word of `PLACE_WORDS` says where, as do the
    /// IPTC's place properties. Exif's auxiliary namespace (aux:) holds the lens and serial
    /// numbers, and the rights namespaces say whose the photo is.
    pub(crate) fn of(tag: &str) -> Group {
        if let Some(&(_, group)) = TAG_GROUPS.iter().find(|(name, _)| *name == tag) {
            return group;
        }
        let lower = tag.to_ascii_lowercase();
        let starts = |prefixes: &[&str]| prefixes.iter().any(|p| tag.starts_with(p));
        if PLACE_WORDS.iter().any(|w| lower.contains(w))
            || starts(&["Iptc4xmpExt:Location", "mediapro:Location"])
        {
            Group::Place
        } else if starts(&["exif:DateTime", "exif:SubSecTime", "exifEX:OffsetTime"]) {
            Group::Date
        } else if starts(&["aux:"]) {
            Group::Camera
        } else if starts(&["xmpRights:", "plus:"]) {
            Group::Photographer
        } else if starts(&["MicrosoftPhoto:LastKeyword", "mediapro:"]) {
            Group::Keywords
        } else {
            Group::Other
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
            let group = Group::of_key(word).unwrap_or_else(|| Group::of(word));
            self.has_left_out[group as usize] = true;
        }
    }

    /// The text of the choice's file, one word to a line.
    pub(crate) fn text(&self) -> String {
        self.words.iter().map(|w| format!("{w}\n")).collect()
    }

    /// Whether the tag `tag` is left out: by its name, by its whole group, or as a block that
    /// can hold a tag of a group that has one left out (`BLOCKS`).
    pub(crate) fn is_left_out(&self, tag: &str) -> bool {
        self.words.contains(tag)
            || self.words.contains(Group::of(tag).key())
            || (block_groups(tag).iter()).any(|g| self.has_left_out[*g as usize])
    }

    /// Whether the whole group `group` is left out.
    pub(crate) fn is_group_left_out(&self, group: Group) -> bool {
        self.words.contains(group.key())
    }

    /// Leaves the tag `tag` out, or keeps it. A tag kept out of a group left out whole leaves
    /// the group's other tags of `found` out one by one instead.
    pub(crate) fn set_tag(&mut self, tag: &str, left_out: bool, found: &[String]) {
        if left_out {
            self.words.insert(tag.to_owned());
        } else {
            self.words.remove(tag);
            let group = Group::of(tag);
            if self.words.remove(group.key()) {
                let others = found.iter().filter(|t| *t != tag && Group::of(t) == group);
                self.words.extend(others.cloned());
            }
        }
        self.count_groups();
    }

    /// Leaves the whole group `group` out, or keeps every tag of it.
    pub(crate) fn set_group(&mut self, group: Group, left_out: bool) {
        if left_out {
            self.words.insert(group.key().to_owned());
        } else {
            // The other groups' keys stay, though `Group::of` puts no key in their groups.
            let is_key = |w: &str| w.starts_with("group:");
            (self.words).retain(|w| w != group.key() && (is_key(w) || Group::of(w) != group));
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
/// before the new EXIF and XMP, the XMP inside it, and the properties its IPTC IIM gives.
struct Walked<'a> {
    container: Container,
    parts: Vec<Part<'a>>,
    before_metadata: usize,
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
    with_walked(path, |walked, metadata| {
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
    let keeps = |tag: &str| kept.contains(tag);
    with_walked(path, |walked, metadata| {
        let (exif, xmp) = metadata.written(&keeps);
        assembled(walked, &keeps, exif, xmp)
    })?
    .transpose()
}

/// Reads `path`, walks it, reads what it may keep of its metadata, and gives both to `then`.
/// Returns `None` for a file that is not a JPEG, PNG or WebP.
fn with_walked<T>(
    path: &Path,
    then: impl FnOnce(&Walked, &SharedMetadata) -> T,
) -> Result<Option<T>, String> {
    let data = crate::images::decode::read_limited(path).map_err(|e| e.to_string())?;
    let Some(mut walked) = walk(&data)? else {
        return Ok(None);
    };
    let iim = std::mem::take(&mut walked.iim);
    let metadata = shared_metadata(path, walked.xmp, iim)?;
    Ok(Some(then(&walked, &metadata)))
}

/// Walks `data` as the container its first bytes say. Returns `None` when it is not a JPEG,
/// PNG or WebP, and an error when it is one that is damaged.
fn walk(data: &[u8]) -> Result<Option<Walked<'_>>, String> {
    let walked = if data.starts_with(&[0xff, 0xd8]) {
        walk_jpeg(data)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        walk_png(data)
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(&b"WEBP"[..]) {
        walk_webp(data)
    } else {
        return Ok(None);
    };
    walked.map(Some).ok_or_else(|| "damaged or cut off".into())
}

/// Walks a JPEG: each segment up to the end of its image (EOI), each scan with its data, and
/// what follows the image. Returns `None` when a segment's length runs past the file.
fn walk_jpeg(data: &[u8]) -> Option<Walked<'_>> {
    let mut walked = Walked::new(Container::Jpeg);
    walked.add(data, 0, 2, None);
    let (mut at, mut has_mpf) = (2, false);
    while at < data.len() {
        // A marker may follow any number of fill bytes (0xff).
        while data.get(at) == Some(&0xff) && data.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        if data.get(at) != Some(&0xff) {
            return None;
        }
        let marker = *data.get(at + 1)?;
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
        0xe1 if body.starts_with(b"Exif\0\0") => return,
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
        match values.iter_mut().find(|(n, _, _)| *n == name) {
            Some((_, _, list)) => list.push(text),
            None => values.push((name, form, vec![text])),
        }
    }
    let namespaces = IIM_NAMESPACES
        .iter()
        .map(|(p, u)| ((*p).to_owned(), (*u).to_owned()));
    Properties {
        list: (values.into_iter())
            .map(|(name, form, texts)| (name.to_owned(), iim_element(name, form, texts)))
            .collect(),
        namespaces: namespaces.collect(),
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
            b"eXIf" => None,
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

/// Walks a WebP: each chunk of its RIFF file. Returns `None` when a chunk runs past the file.
fn walk_webp(data: &[u8]) -> Option<Walked<'_>> {
    let mut walked = Walked::new(Container::WebP);
    walked.add(data, 0, 12, None);
    let mut at = 12;
    while at + 8 <= data.len() {
        let name: [u8; 4] = data[at..at + 4].try_into().ok()?;
        let size = u32::from_le_bytes(data[at + 4..at + 8].try_into().ok()?) as usize;
        let body = data.get(at + 8..(at + 8).checked_add(size)?)?;
        // A chunk of odd size has a padding byte, which the last one may lack.
        let end = (at + 8 + size + size % 2).min(data.len());
        match &name {
            n if WEBP_IMAGE_CHUNKS.contains(&n) => walked.add(data, at, end, None),
            b"EXIF" => {}
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
        use super::Group::{Date, Keywords, Place, Software};
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
        left_out.set_tag(super::EXIF_THUMBNAIL, true, &[]);
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
            ("xmpMM:DerivedFrom", Software),
            ("xmpMM:Ingredients", Software),
            ("xmpMM:PreservedFileName", Software),
        ] {
            assert_eq!(super::Group::of(tag), group, "{tag}");
        }
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
