//! An XMP packet (`Packet`) and the one walk over it. A property is read, taken out or changed
//! only where the walk finds it, at the top of an rdf:Description (`top_level_properties`), and
//! every walk passes over comments, CDATA sections and processing instructions
//! (`not_element_length`). The packet's bytes are private to this module, so code elsewhere
//! cannot search them for a property's name.

use crate::metadata::xmp::{Namespaces, Properties};
use crate::support::format::without_control_characters;

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

/// The start of an rdf:Description's start tag, as `next_description` finds it.
const DESCRIPTION: &[u8] = b"<rdf:Description";

/// The end tag of an rdf:Description.
const DESCRIPTION_END: &[u8] = b"</rdf:Description>";

/// The kinds of XML markup that are no element, each as how it starts and how it ends: a
/// comment, a CDATA section and a processing instruction. A quote, a '<' or a tag inside one is
/// no part of the packet's structure, so every walk over a packet passes over them whole
/// (`not_element_length`). A CDATA section with a quote in it once stopped the walk, and the
/// editors then wrote a second dc:subject.
const NOT_ELEMENTS: [(&[u8], &[u8]); 3] =
    [(b"<!--", b"-->"), (b"<![CDATA[", b"]]>"), (b"<?", b"?>")];

/// An XMP packet: a sidecar's bytes, the XMP inside an image, or a packet Glimt wrote. Its
/// bytes are private to this module, so code elsewhere reads a property only where its walk
/// finds it, at the top of an rdf:Description (`top_level_properties`), and never by a search of
/// the packet for a property's name, which also finds another document's inside xmpMM:Pantry.
/// It holds bytes, not text, because a copy keeps a packet that is not UTF-8 byte for byte. A
/// reading of its text gives nothing for such a packet, as the readers did before it came in.
/// The bytes leave it only to be written whole (`bytes_to_write`, `into_bytes`).
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct Packet {
    bytes: Vec<u8>,
}

impl Packet {
    /// Returns the packet whose bytes are `bytes`, as they were read from a sidecar or from an
    /// image, or as a writer wrote them.
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Returns the packet of a new sidecar: one empty rdf:Description, which the writers fill.
    pub(crate) fn new_sidecar() -> Self {
        Self::from_bytes(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
             <rdf:Description rdf:about=\"\"/>\n </rdf:RDF>\n</x:xmpmeta>\n\
             <?xpacket end=\"w\"?>\n"
                .into(),
        )
    }

    /// Returns the packet of the text `text`, for a test.
    #[cfg(test)]
    pub(crate) fn for_test(text: &str) -> Self {
        Self::from_bytes(text.into())
    }

    /// Returns the packet's text, for a test to compare.
    #[cfg(test)]
    pub(crate) fn text_for_test(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap()
    }

    /// Returns the packet's bytes, to be written whole into a file.
    pub(crate) fn bytes_to_write(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the packet's bytes, to be written whole into a file.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the packet's text. Returns `None` when it is not UTF-8, which the readers of text
    /// read as a packet with nothing in it.
    fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }

    /// Returns the top-level properties of every rdf:Description of the packet
    /// (`top_level_properties`), in the order they are written.
    pub(crate) fn top_level_properties(&self) -> Vec<TopLevelProperty<'_>> {
        let places = top_level_properties(&self.bytes).into_iter();
        places.map(|place| self.property_at(place)).collect()
    }

    /// Returns whether the packet has the top-level property `tag` (`properties_named`), as an
    /// element or an attribute, as the readers read both. A search for its name would also find
    /// one inside another property, such as another document's in xmpMM:Pantry.
    pub(crate) fn has_property(&self, tag: &str) -> bool {
        !properties_named(&self.bytes, tag).is_empty()
    }

    /// Returns the rdf:li items of the top-level property `tag` (`items_with_languages`), each
    /// as its language and its text, or none when the packet is not UTF-8.
    pub(crate) fn items_with_languages(&self, tag: &str) -> Vec<(String, String)> {
        (self.text()).map_or_else(Vec::new, |xmp| items_with_languages(xmp, tag))
    }

    /// Returns the top-level properties of the packet as Copy for Sharing reads them
    /// (`properties`), or none when the packet is not UTF-8.
    pub(crate) fn properties(&self) -> Properties {
        self.text().map_or_else(Properties::default, properties)
    }

    /// Returns the namespaces that the packet declares anywhere
    /// (`namespace_declarations_with_default`), or none when it is not UTF-8.
    pub(crate) fn namespace_declarations_with_default(&self) -> Vec<(&str, &str)> {
        (self.text()).map_or_else(Vec::new, namespace_declarations_with_default)
    }

    /// Returns the fields of each item whose name as written `is_item` takes, inside the value
    /// of `within`, a property of this packet (`structure_items`).
    pub(crate) fn structure_items(
        &self,
        within: &TopLevelProperty<'_>,
        is_item: impl Fn(&[u8]) -> bool,
    ) -> Vec<Vec<TopLevelProperty<'_>>> {
        let items = structure_items(&self.bytes, within.place.value.clone(), is_item);
        (items.into_iter())
            .map(|fields| (fields.into_iter()).map(|f| self.property_at(f)).collect())
            .collect()
    }

    /// Returns the packet without the property `tag` in any form it is written in
    /// (`without_property`).
    pub(crate) fn without_property(&self, tag: &str) -> Packet {
        Packet::from_bytes(without_property(&self.bytes, tag))
    }

    /// Returns the packet with the elements of the namespace `uri` that `add` writes put in its
    /// first rdf:Description (`insert`). Returns `None` when it has no rdf:Description, or is not
    /// UTF-8.
    pub(crate) fn with_elements_added(
        &self,
        prefix: &str,
        uri: &str,
        add: impl FnOnce(&str) -> String,
    ) -> Option<Packet> {
        let added = insert(self.text()?.to_owned(), prefix, uri, add)?;
        Some(Packet::from_bytes(added.into_bytes()))
    }

    /// Returns the packet with the attribute `local`="`value`" of the namespace `uri` put first
    /// on its first rdf:Description, with the prefix in force there for `uri`, which is
    /// `prefix` declared there when none is (`prefix_in_force`). Returns `None` when it has no
    /// rdf:Description, or is not UTF-8.
    #[cfg(feature = "marks")]
    pub(crate) fn with_attribute_added(
        &self,
        prefix: &str,
        uri: &str,
        local: &str,
        value: &str,
    ) -> Option<Packet> {
        let xmp = self.text()?;
        let (_, declared) = first_description(xmp)?;
        let (prefix, ns) = crate::metadata::xmp::prefix_in_force(&declared, prefix, uri);
        let at = first_description_start(xmp)? + DESCRIPTION.len();
        let added = format!(
            "{}{ns} {prefix}:{local}=\"{value}\"{}",
            &xmp[..at],
            &xmp[at..]
        );
        Some(Packet::from_bytes(added.into_bytes()))
    }

    /// Returns the packet with its `tiff:Orientation` set to `value` (1–8), as an attribute or
    /// an element. The length stays, as a value is one digit. Only the photo's own orientation
    /// changes, a top-level property (`properties_named`), and not that of another document in
    /// xmpMM:Pantry. It is found by its namespace, whatever prefix the packet binds to it.
    pub(crate) fn with_orientation(&self, value: u8) -> Packet {
        let mut xmp = self.bytes.clone();
        for found in properties_named(&self.bytes, "tiff:Orientation") {
            let text = &xmp[found.value.clone()];
            let blanks = text.iter().take_while(|c| c.is_ascii_whitespace()).count();
            if let [digit] = text.trim_ascii()
                && digit.is_ascii_digit()
            {
                xmp[found.value.start + blanks] = b'0' + value.min(9);
            }
        }
        Packet::from_bytes(xmp)
    }

    /// Returns the property of this packet at `place`, a place its walk found.
    fn property_at(&self, place: PropertyPlace) -> TopLevelProperty<'_> {
        TopLevelProperty {
            packet: &self.bytes,
            place,
        }
    }
}

/// A property at the top of an rdf:Description of a packet, an attribute of its start tag or an
/// element just inside it (`Packet::top_level_properties`). It borrows the packet it was found
/// in, so it is read only there, and only its own name and value are read.
pub(crate) struct TopLevelProperty<'p> {
    packet: &'p [u8],
    place: PropertyPlace,
}

impl<'p> TopLevelProperty<'p> {
    /// Returns the property's value: an attribute's text between its quotes, or what is between
    /// an element's start and end tags.
    pub(crate) fn value(&self) -> &'p [u8] {
        &self.packet[self.place.value.clone()]
    }

    /// Returns the property's value as text, or `None` when it is not UTF-8.
    pub(crate) fn value_text(&self) -> Option<&'p str> {
        std::str::from_utf8(self.value()).ok()
    }

    /// Returns whether the property is written as an attribute.
    pub(crate) fn is_attribute(&self) -> bool {
        self.place.is_attribute
    }

    /// Returns whether the property is `local` in one of the namespaces `uris`, read as
    /// `PropertyPlace::is_in` reads it.
    pub(crate) fn is_in(
        &self,
        declared_elsewhere: &std::collections::HashSet<(&str, &str)>,
        uris: &[&str],
        local: &str,
    ) -> bool {
        (self.place).is_in(self.packet, declared_elsewhere, uris, local)
    }
}

/// Where a property at the top of an rdf:Description is in its packet, an attribute of its
/// start tag or an element just inside it (`top_level_properties`). Each range is a place in the
/// packet. Code outside this module reads it as a `TopLevelProperty`, which borrows the packet.
struct PropertyPlace {
    /// The name as written, such as "dc:subject". It is private, so that code elsewhere tells
    /// a property by its namespace (`is_in`, `properties_named`), never by the prefix as
    /// written, which another writer may have chosen otherwise.
    name: std::ops::Range<usize>,
    /// The whole property: an attribute from its name to its closing quote, an element from its
    /// "<" to the end of its end tag or of its "/>".
    whole: std::ops::Range<usize>,
    /// The value: an attribute's text between its quotes, or what is between an element's start
    /// and end tags, which is nothing for an empty element.
    value: std::ops::Range<usize>,
    is_attribute: bool,
    /// The place in the packet of the URI of the namespace that the name's prefix is bound to
    /// where the name is written (`Scope`), or `None` when it is bound to none there.
    namespace: Option<std::ops::Range<usize>>,
}

impl PropertyPlace {
    /// Returns whether this property of the packet `xmp` is `local` in one of the namespaces
    /// `uris`. Its prefix is read as it is bound where the property is written (`Scope`). Only
    /// a prefix bound to nothing there is looked up in `declared_elsewhere`
    /// (`is_in_namespace`), the declarations of another packet that may bind it, as a JPEG's
    /// extended XMP may use a prefix that its main packet declares. Before, a prefix was
    /// looked up in the whole packet, so a prefix that two Descriptions bind to two namespaces
    /// was read in both as the first of them binds it.
    fn is_in(
        &self,
        xmp: &[u8],
        declared_elsewhere: &std::collections::HashSet<(&str, &str)>,
        uris: &[&str],
        local: &str,
    ) -> bool {
        let name = &xmp[self.name.clone()];
        let Some(uri) = &self.namespace else {
            return is_in_namespace(name, declared_elsewhere, uris, local);
        };
        let own_local = name
            .iter()
            .position(|&c| c == b':')
            .map_or(name, |colon| &name[colon + 1..]);
        own_local == local.as_bytes() && uris.iter().any(|u| xmp[uri.clone()] == *u.as_bytes())
    }
}

/// The namespaces in force at a place of a packet, as a walk over it opens and closes its
/// elements: those that the start tags of the elements open around the place declare. A name
/// is read by the namespace its prefix is bound to where it is written, so a prefix that two
/// Descriptions bind to two namespaces is read in each as that Description binds it.
#[derive(Default)]
struct Scope<'a> {
    /// The declarations of each element open, outermost first, each as its prefix ("" for a
    /// default namespace) and the place of its URI in the packet.
    open: Vec<Vec<(&'a [u8], std::ops::Range<usize>)>>,
    /// The places of the URIs that the elements open bind each prefix to, the innermost last.
    bound: std::collections::HashMap<&'a [u8], Vec<std::ops::Range<usize>>>,
}

impl<'a> Scope<'a> {
    /// Opens the element whose start tag has its attributes at `head` in `xmp`, with the
    /// namespaces that they declare.
    fn open(&mut self, xmp: &'a [u8], head: std::ops::Range<usize>) {
        let declared: Vec<(&[u8], std::ops::Range<usize>)> = (tag_attributes(xmp, head)
            .into_iter())
        .filter_map(|(name, value)| match &xmp[name] {
            b"xmlns" => Some((&b""[..], value)),
            name => {
                let prefix = name.strip_prefix(b"xmlns:")?;
                (!prefix.is_empty() && !prefix.contains(&b':')).then_some((prefix, value))
            }
        })
        .collect();
        for (prefix, uri) in &declared {
            self.bound.entry(prefix).or_default().push(uri.clone());
        }
        self.open.push(declared);
    }

    /// Closes the innermost element open, and with it the namespaces it declares.
    fn close(&mut self) {
        for (prefix, _) in self.open.pop().into_iter().flatten() {
            if let Some(uris) = self.bound.get_mut(prefix) {
                uris.pop();
            }
        }
    }

    /// Returns the place of the URI that `prefix` is bound to, or `None` when it is bound to
    /// none.
    fn uri(&self, prefix: &[u8]) -> Option<std::ops::Range<usize>> {
        self.bound.get(prefix)?.last().cloned()
    }

    /// Opens and closes the elements whose tags are in `xmp` from `from` up to `to`, the
    /// markup before a Description or between two of them. Comments, CDATA sections and
    /// processing instructions are passed over whole (`not_element_length`). It stops at a tag
    /// or such markup that does not end before `to`.
    fn walk(&mut self, xmp: &'a [u8], mut from: usize, to: usize) {
        while let Some(found) = xmp[from..to].iter().position(|&c| c == b'<') {
            let at = from + found;
            if let Some(length) = not_element_length(&xmp[at..]) {
                match length.map(|length| at + length) {
                    Some(end) if end <= to => from = end,
                    _ => return,
                }
                continue;
            }
            let Some(gt) = start_tag_end(xmp, at).filter(|&gt| gt < to) else {
                return;
            };
            let name_end = at
                + 1
                + (xmp[at + 1..gt].iter())
                    .take_while(|&&c| is_name_byte(c))
                    .count();
            match xmp[at + 1] {
                b'/' => self.close(),
                b'!' => {}
                _ if xmp[gt - 1] == b'/' => {}
                _ => self.open(xmp, name_end..gt),
            }
            from = gt + 1;
        }
    }

    /// Returns the namespaces in force with a prefix, each as (prefix, URI), outermost first,
    /// so that a later one of a prefix is the one in force, as `prefix_in_force` reads them.
    fn in_force(&self, xmp: &'a str) -> Vec<(&'a str, &'a str)> {
        (self.open.iter().flatten())
            .filter(|(prefix, _)| !prefix.is_empty())
            .filter_map(|(prefix, uri)| {
                Some((std::str::from_utf8(prefix).ok()?, xmp.get(uri.clone())?))
            })
            .collect()
    }
}

/// The rdf:li items of property `tag`, each as the language its start tag names (xml:lang, or
/// empty for none) and its text. They are those of every top-level property that is `tag`
/// (`properties_named`), in the order they are written, as the editors take out every one of
/// them (`without_property`) and write back what was read. An item that an earlier one of them
/// has already is left out. A property written as an attribute is one item without a language.
/// Before, only the first was read, and an edit of a sidecar that held `dc:subject` and
/// `ns1:subject` both lost the keywords of the second for good.
fn items_with_languages(xmp: &str, tag: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for found in properties_named(xmp.as_bytes(), tag) {
        let items = match found.is_attribute {
            true => vec![(String::new(), unescape(&xmp[found.value]))],
            false => element_items(xmp, found.whole.start, found.whole.end),
        };
        let earlier = out.len();
        for item in items {
            if !out[..earlier].contains(&item) {
                out.push(item);
            }
        }
    }
    out
}

/// The rdf:li items of the element of `xmp` from `start` to `end`, each as the language its
/// start tag names (xml:lang, or empty for none) and its text.
fn element_items(xmp: &str, start: usize, end: usize) -> Vec<(String, String)> {
    const ITEM: &[u8] = b"<rdf:li";
    let (bytes, mut from, mut out) = (&xmp.as_bytes()[..end], start, Vec::new());
    while let Some(found) = bytes[from..].iter().position(|&c| c == b'<') {
        let at = from + found;
        if let Some(length) = not_element_length(&bytes[at..]) {
            let Some(length) = length else { break };
            from = at + length;
            continue;
        }
        let Some(gt) = start_tag_end(bytes, at) else {
            break;
        };
        let is_item = bytes[at..].starts_with(ITEM)
            && bytes.get(at + ITEM.len()).is_some_and(|&c| ends_a_name(c));
        if !is_item || bytes[gt - 1] == b'/' {
            from = gt + 1;
            continue;
        }
        let Some(item_end) = element_end(bytes, gt + 1, b"rdf:li") else {
            break;
        };
        let language = (attributes(&xmp[at + ITEM.len()..gt]).into_iter())
            .find(|(name, _)| *name == "xml:lang")
            .map_or_else(String::new, |(_, value)| unescape(value));
        out.push((
            language,
            text_of(&xmp[gt + 1..item_end - b"</rdf:li>".len()]),
        ));
        from = item_end;
    }
    out
}

/// Returns the text of `content`, what is between an element's start and end tags, as XML reads
/// it. Its entities are read, a CDATA section gives its text as it is written, and comments and
/// processing instructions are left out (`not_element_length`).
fn text_of(content: &str) -> String {
    let (mut out, mut from) = (String::new(), 0);
    while let Some(found) = content[from..].find('<') {
        let at = from + found;
        out += &unescape(&content[from..at]);
        let rest = &content[at..];
        match not_element_length(rest.as_bytes()) {
            Some(Some(length)) => {
                if let Some(text) = rest[..length].strip_prefix("<![CDATA[") {
                    out += &text[..text.len() - b"]]>".len()];
                }
                from = at + length;
            }
            _ => {
                out.push('<');
                from = at + 1;
            }
        }
    }
    out + &unescape(&content[from..])
}

/// `xmp` with the elements of the namespace `uri` that `add` writes put in its first
/// rdf:Description. `add` is given the prefix to write them with, the one in force there for
/// `uri` (`prefix_in_force`), which is `prefix` declared there when none is.
fn insert(
    xmp: String,
    prefix: &str,
    uri: &str,
    add: impl FnOnce(&str) -> String,
) -> Option<String> {
    let (end, declared) = first_description(&xmp)?;
    let (in_force, ns) = crate::metadata::xmp::prefix_in_force(&declared, prefix, uri);
    let add = add(&in_force);
    // A Description without content (<rdf:Description .../>) is opened to hold the elements.
    Some(match xmp[..end].strip_suffix('/') {
        Some(head) if !add.is_empty() => {
            format!("{head}{ns}>{add}\n  </rdf:Description>{}", &xmp[end + 1..])
        }
        Some(_) => xmp,
        None => format!("{}{ns}>{add}{}", &xmp[..end], &xmp[end + 1..]),
    })
}

/// Reads the top-level properties of every rdf:Description of `xmp` (`top_level_properties`),
/// written as attributes or as elements. An attribute is given as the element it stands for,
/// so a packet written of them holds only elements. A property is named by the namespace its
/// prefix is bound to where it is written (`name_in_namespace`). Each is given the namespaces in
/// force where it is written for the prefixes it uses. The Descriptions share them while they
/// bind no prefix two ways, and one that binds a prefix otherwise than those before it starts
/// namespaces of its own, which `packet` writes in a Description of their own. Before, every
/// prefix was read as the first declaration of the packet binds it, so a property of another
/// namespace written with the same prefix was copied into the first one.
fn properties(xmp: &str) -> Properties {
    let mut groups: Vec<Vec<(String, String)>> = Vec::new();
    let mut bound: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut found_in_group: Vec<(String, String, usize)> = Vec::new();
    each_description(xmp.as_bytes(), |scope, found| {
        // The places are at ASCII characters, so each slice is whole UTF-8.
        let uses = (found.iter()).flat_map(|f| prefixes_in(&xmp[f.whole.clone()]));
        let needed: Vec<(&str, &str)> = uses
            .filter_map(|prefix| Some((prefix, &xmp[scope.uri(prefix.as_bytes())?])))
            .collect();
        let binds_otherwise = (needed.iter()).any(|(p, u)| bound.get(p).is_some_and(|b| b != u));
        if groups.is_empty() || binds_otherwise {
            groups.push(Vec::new());
            bound.clear();
        }
        let group = groups.len() - 1;
        for (prefix, uri) in needed {
            if bound.insert(prefix, uri).is_none() {
                groups[group].push((prefix.to_owned(), uri.to_owned()));
            }
        }
        for found in found {
            let name = &xmp[found.name.clone()];
            let Some((prefix, local)) = name.split_once(':') else {
                continue;
            };
            let element = match found.is_attribute {
                true => format!("<{name}>{}</{name}>", &xmp[found.value]),
                false => xmp[found.whole].to_owned(),
            };
            let uri = found.namespace.map(|uri| &xmp[uri]);
            found_in_group.push((name_in_namespace(prefix, local, uri), element, group));
        }
    });
    let groups: Vec<Namespaces> = groups.into_iter().map(Namespaces::new).collect();
    let list = (found_in_group.into_iter())
        .map(|(name, element, group)| (name, element, groups[group].clone()))
        .collect();
    Properties { list }
}

/// Returns the prefixes that `text`, a property as written, may use: each run of the letters
/// of a name before a ':'. It may give a word of a value too, which no namespace in force binds
/// as a rule, or binds to no harm.
fn prefixes_in(text: &str) -> Vec<&str> {
    let (mut out, mut start) = (Vec::new(), 0);
    for (i, &c) in text.as_bytes().iter().enumerate() {
        if c == b':' && start < i {
            out.push(&text[start..i]);
        }
        if c == b':' || !is_name_byte(c) {
            start = i + 1;
        }
    }
    out
}

/// Calls `each` with the namespaces in force in each rdf:Description of `xmp` (`Scope`) and
/// its top-level properties (`top_level_properties`), in the order they are written.
fn each_description<'a>(xmp: &'a [u8], mut each: impl FnMut(&Scope<'a>, Vec<PropertyPlace>)) {
    let (mut scope, mut from) = (Scope::default(), 0);
    while let Some(start) = next_description(xmp, from) {
        scope.walk(xmp, from, start);
        let Some(gt) = start_tag_end(xmp, start) else {
            break;
        };
        let head = start + DESCRIPTION.len()..gt;
        scope.open(xmp, head.clone());
        let mut found = Vec::new();
        attribute_properties(xmp, head, &scope, &mut found);
        let end = match xmp[gt - 1] == b'/' {
            true => Some(gt + 1),
            false => element_properties(xmp, gt + 1, DESCRIPTION_END, &mut scope, &mut found),
        };
        each(&scope, found);
        scope.close();
        match end {
            Some(end) => from = end,
            None => break,
        }
    }
}

/// Returns the top-level properties of every rdf:Description of `xmp`, in the order they are
/// written: the attributes of its start tag, but for the namespace declarations and the rdf:
/// and xml: ones, and the elements just inside it. What is inside a property is no property of
/// the photo, such as the Descriptions of other documents in xmpMM:Pantry, so the readers and
/// editors never see it. It is the one reading of where a property is, for them and for Copy
/// for Sharing. A Description is read up to what does not read as a property, and the walk
/// stops there, because what follows cannot be told apart from the inside of a property.
/// Comments, CDATA sections and processing instructions are passed over whole, also inside a
/// property (`not_element_length`). Each step goes on from where the last one ended, so a
/// hostile packet takes linear time. Each property's prefix is read as it is bound where the
/// property is written (`Scope`).
fn top_level_properties(xmp: &[u8]) -> Vec<PropertyPlace> {
    let mut out = Vec::new();
    each_description(xmp, |_, found| out.extend(found));
    out
}

/// Returns the top-level properties of `xmp` (`top_level_properties`) that are `tag`, a name
/// with the prefix of its namespace in `KNOWN_NAMESPACES`, such as "dc:subject". A property
/// counts when its prefix is bound to that namespace where it is written, whatever the prefix
/// is, as a tool that writes `ns1:subject` binds it. It counts too when it is written as `tag`
/// and its prefix is bound to nothing there, as a broken packet may have it. A `tag` whose
/// prefix is not in `KNOWN_NAMESPACES` is found as it is written. It is the one way the
/// readers and editors find a property by its name. Before, they compared the prefix as
/// written, so they read no keywords of a packet that wrote `ns1:subject` and wrote a second
/// `dc:subject` beside it, which exiv2 refuses.
fn properties_named(xmp: &[u8], tag: &str) -> Vec<PropertyPlace> {
    let (prefix, local) = tag.split_once(':').unwrap_or(("", tag));
    let uris: Vec<&str> = (KNOWN_NAMESPACES.iter())
        .filter(|(known, _)| *known == prefix)
        .map(|(_, uri)| *uri)
        .collect();
    let nowhere_else = std::collections::HashSet::new();
    (top_level_properties(xmp).into_iter())
        .filter(|found| {
            let as_written = uris.is_empty() || found.namespace.is_none();
            (as_written && xmp[found.name.clone()] == *tag.as_bytes())
                || found.is_in(xmp, &nowhere_else, &uris, local)
        })
        .collect()
}

/// Returns where the first rdf:Description of `xmp` at `from` or after it starts, at its '<'.
/// Comments, CDATA sections and processing instructions are passed over whole
/// (`not_element_length`), so a Description inside one is not found. Returns `None` when there
/// is none, or when such markup before it does not end.
fn next_description(xmp: &[u8], mut from: usize) -> Option<usize> {
    while let Some(found) = xmp[from..].iter().position(|&c| c == b'<') {
        let at = from + found;
        if let Some(length) = not_element_length(&xmp[at..]) {
            from = at + length?;
            continue;
        }
        let after_name = at + DESCRIPTION.len();
        if xmp[at..].starts_with(DESCRIPTION)
            && xmp.get(after_name).is_some_and(|&c| ends_a_name(c))
        {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Returns the length of the comment, CDATA section or processing instruction that `rest`
/// starts with (`NOT_ELEMENTS`), from its '<' to past its end. Returns `None` when `rest`
/// starts with none of them, and `Some(None)` when it starts one that does not end.
fn not_element_length(rest: &[u8]) -> Option<Option<usize>> {
    let (starts, ends) = NOT_ELEMENTS
        .iter()
        .find(|(starts, _)| rest.starts_with(starts))?;
    let end = find_bytes(&rest[starts.len()..], ends);
    Some(end.map(|end| starts.len() + end + ends.len()))
}

/// Adds the properties written as attributes in `head`, the place of a Description's start tag
/// after its name, to `out`, each with the namespace its prefix is bound to in `scope`. It reads
/// up to what does not read as an attribute (`tag_attributes`).
fn attribute_properties(
    xmp: &[u8],
    head: std::ops::Range<usize>,
    scope: &Scope,
    out: &mut Vec<PropertyPlace>,
) {
    for (name, value) in tag_attributes(xmp, head) {
        let written = &xmp[name.clone()];
        let Some(colon) = written.iter().position(|&c| c == b':') else {
            continue;
        };
        if [&b"xmlns:"[..], b"rdf:", b"xml:"]
            .iter()
            .any(|p| written.starts_with(p))
        {
            continue;
        }
        out.push(PropertyPlace {
            namespace: scope.uri(&written[..colon]),
            whole: name.start..value.end + 1,
            name,
            value,
            is_attribute: true,
        });
    }
}

/// Returns the attributes written in `head`, the place of a start tag after its name, each as
/// the places of its name and of its value between its quotes. It reads up to what does not
/// read as an attribute.
fn tag_attributes(
    xmp: &[u8],
    head: std::ops::Range<usize>,
) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let blanks_from = |i: usize| {
        let i = i.min(head.end);
        i + (xmp[i..head.end].iter())
            .take_while(|c| c.is_ascii_whitespace())
            .count()
    };
    let mut at = head.start;
    loop {
        let name_start = blanks_from(at);
        let name_end = name_start
            + (xmp[name_start..head.end].iter())
                .take_while(|&&c| is_name_byte(c))
                .count();
        let equals = blanks_from(name_end);
        if name_end == name_start || equals >= head.end || xmp[equals] != b'=' {
            return out;
        }
        let open = blanks_from(equals + 1);
        let quote = xmp[open..head.end].first().copied();
        let Some(quote) = quote.filter(|&q| q == b'"' || q == b'\'') else {
            return out;
        };
        let Some(close) = (xmp[open + 1..head.end].iter()).position(|&c| c == quote) else {
            return out;
        };
        let close = open + 1 + close;
        out.push((name_start..name_end, open + 1..close));
        at = close + 1;
    }
}

/// Adds the elements in `xmp` from `from` on, the properties inside a Description or the
/// fields inside a structure's item, to `out`. They end at the end tag `end`, such as
/// `DESCRIPTION_END`. Each is given the namespace its prefix is bound to in `scope` with the
/// declarations of its own start tag. Returns the place just after that end tag, or `None` when
/// something else stops the reading.
fn element_properties<'a>(
    xmp: &'a [u8],
    mut from: usize,
    end_tag: &[u8],
    scope: &mut Scope<'a>,
    out: &mut Vec<PropertyPlace>,
) -> Option<usize> {
    loop {
        from += (xmp[from..].iter())
            .take_while(|c| c.is_ascii_whitespace())
            .count();
        let rest = &xmp[from..];
        if let Some(length) = not_element_length(rest) {
            from += length?;
            continue;
        }
        if rest.starts_with(end_tag) {
            return Some(from + end_tag.len());
        }
        if rest.first() != Some(&b'<') {
            return None;
        }
        let name_length = rest[1..].iter().take_while(|&&c| is_name_byte(c)).count();
        let name = from + 1..from + 1 + name_length;
        if name_length == 0 || !xmp.get(name.end).is_some_and(|&c| ends_a_name(c)) {
            return None;
        }
        let gt = start_tag_end(xmp, from)?;
        let (value, end) = match xmp[gt - 1] == b'/' {
            true => (gt + 1..gt + 1, gt + 1),
            false => {
                let end = element_end(xmp, gt + 1, &xmp[name.clone()])?;
                (gt + 1..end - name_length - 3, end)
            }
        };
        let written = &xmp[name.clone()];
        let prefix = written
            .iter()
            .position(|&c| c == b':')
            .map_or(&b""[..], |colon| &written[..colon]);
        scope.open(xmp, name.end..gt);
        let namespace = scope.uri(prefix);
        scope.close();
        out.push(PropertyPlace {
            name,
            whole: from..end,
            value,
            is_attribute: false,
            namespace,
        });
        from = end;
    }
}

/// Returns the fields of each item whose name as written `is_item` takes, inside `within`, the
/// place of a property's value in `xmp`, in the order the items are written. Such items are the
/// parts of a structure, such as the Container:Item elements of a motion photo's
/// Container:Directory, and they are found at any depth inside the value. An item's fields are
/// read in each form that XMP writes a structure in (`item_fields`). Comments, CDATA sections
/// and processing instructions are passed over whole (`not_element_length`). The walk stops at
/// such markup that does not end, and at an item that does not end.
fn structure_items(
    xmp: &[u8],
    within: std::ops::Range<usize>,
    is_item: impl Fn(&[u8]) -> bool,
) -> Vec<Vec<PropertyPlace>> {
    let (xmp, mut from, mut out) = (&xmp[..within.end], within.start, Vec::new());
    while let Some(found) = xmp[from..].iter().position(|&c| c == b'<') {
        let at = from + found;
        if let Some(length) = not_element_length(&xmp[at..]) {
            let Some(length) = length else { break };
            from = at + length;
            continue;
        }
        let Some(gt) = start_tag_end(xmp, at) else {
            break;
        };
        let name_end = at
            + 1
            + (xmp[at + 1..gt].iter())
                .take_while(|&&c| is_name_byte(c))
                .count();
        if !is_item(&xmp[at + 1..name_end]) {
            from = gt + 1;
            continue;
        }
        let mut fields = Vec::new();
        let Some(end) = item_fields(xmp, at, name_end, gt, &mut fields) else {
            break;
        };
        out.push(fields);
        from = end;
    }
    out
}

/// Adds the fields of the structure's item whose start tag starts at `start` in `xmp`, with
/// its name ending at `name_end` and its '>' at `gt`, to `out` (`structure_items`). XMP writes
/// them in three forms, and all three are read: as attributes of the item's start tag, as
/// elements inside it (rdf:parseType="Resource", as ExifTool writes them), and as the
/// attributes and elements of an rdf:Description just inside it. The elements are read up to
/// what does not read as one. Returns the place just past the item's end tag, or `None` when
/// it does not end. A field's prefix is read by the declarations of the item and of the tags
/// inside it around the field. One that they bind to nothing is looked up by the caller
/// (`PropertyPlace::is_in`).
fn item_fields(
    xmp: &[u8],
    start: usize,
    name_end: usize,
    gt: usize,
    out: &mut Vec<PropertyPlace>,
) -> Option<usize> {
    let scope = &mut Scope::default();
    scope.open(xmp, name_end..gt);
    attribute_properties(xmp, name_end..gt, scope, out);
    if xmp[gt - 1] == b'/' {
        return Some(gt + 1);
    }
    let end = element_end(xmp, gt + 1, &xmp[start + 1..name_end])?;
    // The end tag is "</", the name and ">".
    let end_tag = end - (name_end - start) - 2;
    let mut inside = Vec::new();
    // What follows a field that does not read as one is not read, and the fields before it
    // count, as at the top of a Description.
    element_properties(xmp, gt + 1, &xmp[end_tag..end], scope, &mut inside);
    for field in inside {
        if xmp[field.name.clone()] != DESCRIPTION[1..] {
            out.push(field);
            continue;
        }
        let Some(description_gt) = start_tag_end(xmp, field.whole.start) else {
            continue;
        };
        let head = field.name.end..description_gt;
        scope.open(xmp, head.clone());
        attribute_properties(xmp, head, scope, out);
        if xmp[description_gt - 1] != b'/' {
            element_properties(xmp, description_gt + 1, DESCRIPTION_END, scope, out);
        }
        scope.close();
    }
    Some(end)
}

/// Whether `c` may be in an XML name, such as "dc:subject". A byte of a letter beyond ASCII is.
fn is_name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':') || c >= 0x80
}

/// Whether `c` ends the name of a start tag.
fn ends_a_name(c: u8) -> bool {
    c.is_ascii_whitespace() || c == b'>' || c == b'/'
}

/// Returns the first place of `what` in `hay`, or `None` when it is not there.
fn find_bytes(hay: &[u8], what: &[u8]) -> Option<usize> {
    hay.windows(what.len()).position(|w| w == what)
}

/// The namespaces that `xmp` declares anywhere, each as (prefix, URI), in the order they are
/// written. A default namespace (`xmlns="…"`) has the prefix "". It is only for a name that
/// another packet may bind, as a JPEG's extended XMP may use a prefix that its main packet
/// declares (`PropertyPlace::is_in`). A property of a packet is read by the namespaces in
/// force where it is written (`Scope`), never by these. A declaration inside a comment, a CDATA
/// section or a processing instruction declares nothing, so these are passed over whole
/// (`not_element_length`), and the reading stops at one that does not end. Before, a
/// Description commented out declared its prefixes for the one after it, and the writers wrote
/// a property with its prefix bound to nothing.
fn namespace_declarations_with_default(xmp: &str) -> Vec<(&str, &str)> {
    let bytes = xmp.as_bytes();
    let (mut out, mut from, mut at) = (Vec::new(), 0, 0);
    while let Some(found) = bytes[at..].iter().position(|&c| c == b'<') {
        let start = at + found;
        let Some(length) = not_element_length(&bytes[start..]) else {
            at = start + 1;
            continue;
        };
        // The markup starts at a '<', so the text before it is whole UTF-8.
        declarations_in(&xmp[from..start], &mut out);
        let Some(length) = length else {
            return out;
        };
        (from, at) = (start + length, start + length);
    }
    declarations_in(&xmp[from..], &mut out);
    out
}

/// Adds the namespaces that `text`, a part of a packet outside its comments, CDATA sections
/// and processing instructions, declares to `out`. `namespace_declarations_with_default` cuts
/// the packet into such parts at the markup that `not_element_length` passes over.
fn declarations_in<'a>(text: &'a str, out: &mut Vec<(&'a str, &'a str)>) {
    for (at, _) in text.match_indices("xmlns") {
        // A namespace's name is short. Without a cap, a packet of a million unclosed quotes
        // would be searched to its end a million times. The cap is moved back to the start of
        // a letter, as a cut inside one would panic.
        let rest = &text[at + 5..text.floor_char_boundary(at + 1024)];
        // XML allows spaces around the "=", as in `xmlns:tiff = "…"`.
        let declared = rest.split_once('=');
        let Some((name, uri)) =
            declared.and_then(|(p, v)| Some((p.trim_end(), quoted(v.trim_start())?)))
        else {
            continue;
        };
        // A default namespace, written `xmlns="…"`, is given the prefix "".
        if name.is_empty() {
            out.push(("", uri));
        } else if let Some(prefix) = name.strip_prefix(':')
            && is_name(prefix)
            && !prefix.contains(':')
        {
            out.push((prefix, uri));
        }
    }
}

/// Returns whether `name`, an element's or attribute's name as written ("GContainer:Item"), is
/// `local` in one of the namespaces `uris`. Its prefix counts when any of the `declared`
/// namespaces (`namespace_declarations_with_default`) binds it to one of them, whatever the
/// prefix is. One writer's prefix is not another's: ExifTool writes Google's Container as
/// GContainer, and Dynamic Depth binds Container to a namespace of its own. A name without a
/// prefix counts when a default namespace (the prefix "") is one of them. XML puts only an
/// element's name in the default namespace, and the readers of properties list no attribute
/// without a prefix. Before, such an element was in no namespace.
///
/// The declarations are a set of (prefix, URI), so a name is looked up once for each of `uris`,
/// however many declarations a packet has. Before, every declaration was compared for each
/// name, and a crafted packet of 16 MiB with a declaration and a property in each Description
/// took about nine minutes (hours at 100 MB).
pub(crate) fn is_in_namespace(
    name: &[u8],
    declared: &std::collections::HashSet<(&str, &str)>,
    uris: &[&str],
    local: &str,
) -> bool {
    let colon = name.iter().position(|&c| c == b':');
    let (prefix, rest) = match colon {
        Some(colon) => (&name[..colon], &name[colon + 1..]),
        None => (&b""[..], name),
    };
    let Ok(prefix) = std::str::from_utf8(prefix) else {
        return false;
    };
    rest == local.as_bytes() && uris.iter().any(|uri| declared.contains(&(prefix, *uri)))
}

/// Returns where the start tag of the first rdf:Description in `xmp` ends, at its '>' past any
/// '>' in its quoted values, and the namespaces with a prefix in force there (`Scope::in_force`):
/// those declared on it and on the elements around it, outermost first. The writers put what
/// they add into this Description, and read it as `properties` does, so a declaration written
/// `xmlns:dc = "…"` is not declared twice. Before, a declaration on an element that had ended
/// before it counted too, and a property was written with a prefix bound to nothing there.
/// Returns `None` when `xmp` has no Description or its start tag does not end.
fn first_description(xmp: &str) -> Option<(usize, Vec<(&str, &str)>)> {
    let start = first_description_start(xmp)?;
    let end = start_tag_end(xmp.as_bytes(), start)?;
    let mut scope = Scope::default();
    scope.walk(xmp.as_bytes(), 0, start);
    scope.open(xmp.as_bytes(), start + DESCRIPTION.len()..end);
    Some((end, scope.in_force(xmp)))
}

/// Returns where the first rdf:Description of `xmp` starts, at its '<', past the comments,
/// CDATA sections and processing instructions (`next_description`), so that a writer never puts
/// a property into a Description that is commented out. Returns `None` when there is none.
fn first_description_start(xmp: &str) -> Option<usize> {
    next_description(xmp.as_bytes(), 0)
}

/// Returns the name of the property `local` written with `prefix`, which is bound to the
/// namespace `uri`, or to none, where it is written. It has the prefix of its namespace in
/// `KNOWN_NAMESPACES`, or else the prefix as written. A known prefix bound to another
/// namespace, or to none, is replaced by that namespace in braces ("{urn:other}Make"), so the
/// name cannot pass for a known one. An empty prefix is a default namespace's, and a property
/// in an unknown default namespace is named by it in braces too, as is one whose name would
/// start like a word of Copy for Sharing's choice that is no tag ("group:x",
/// `shared_copy::GROUP_KEY_START`). Saved, such a name was read back as a later group word,
/// which leaves out no block, while in the dialog it left out every block. The namespace's
/// control characters are replaced (`without_control_characters`), as the name is saved in
/// Copy for Sharing's choice, one name to a line.
fn name_in_namespace(prefix: &str, local: &str, uri: Option<&str>) -> String {
    let is_group_word =
        crate::images::shared_copy::GROUP_KEY_START.strip_suffix(':') == Some(prefix);
    match uri.and_then(|u| KNOWN_NAMESPACES.iter().find(|(_, known)| *known == u)) {
        Some((known, _)) => format!("{known}:{local}"),
        None if prefix.is_empty()
            || is_group_word
            || KNOWN_NAMESPACES.iter().any(|(known, _)| *known == prefix) =>
        {
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
fn start_tag_end(xmp: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (i, &c) in xmp[start..].iter().enumerate() {
        match (quote, c) {
            (None, b'"' | b'\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, b'>') => return Some(start + i),
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

/// Returns the property element `element` with only its own value. Every property written
/// inside it, as an element or as an attribute, is left out unless `keeps` keeps its name. A
/// name is read by the namespace its prefix is bound to there (`name_inside`): by a declaration
/// inside the element, by a default namespace, or else by the namespaces `namespaces` in force
/// where the element is written, as `properties` reads the properties themselves. The RDF and
/// XML names that give the value its form (rdf:Seq, rdf:li, rdf:parseType, xml:lang) stay,
/// and comments go. Without it, a kept property could carry a value the user left out, such
/// as a GPS position written inside a gain map's property. Returns an empty text when the
/// element does not read as balanced XML (an element that never ends, a start tag without its
/// '>'), so that a property that cannot be read is left out whole rather than written into a
/// packet broken.
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
        // A comment goes. A CDATA section is text, which may hold a quote or a '<', and it
        // stays, as a processing instruction does.
        if let Some(length) = not_element_length(rest.as_bytes()) {
            let Some(length) = length else {
                return String::new();
            };
            if !rest.starts_with("<!--") {
                out += &rest[..length];
            }
            from = start + length;
            continue;
        }
        let Some(gt) = start_tag_end(element.as_bytes(), start) else {
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
        if rest.starts_with("<!") {
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
                false => element_end(element.as_bytes(), gt + 1, name.as_bytes())
                    .unwrap_or(element.len()),
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
/// `name_in_namespace` gives it. Its prefix is looked up in `declared`, the declarations of its
/// own element, then in those of the elements `open` around it, innermost first, and last in
/// the `namespaces` in force where the property is written. An element without a prefix is in
/// the default namespace, if one is declared. Returns `None` for what is no property: a name in
/// the RDF or XML namespace, a namespace declaration, an attribute without a prefix, and an
/// element without a namespace. The rdf and xml prefixes bound to nothing count as theirs, as
/// before the namespaces were read.
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
/// end tag, counting the elements of the same name inside it. Comments, CDATA sections and
/// processing instructions are passed over whole (`not_element_length`). Returns `None` when
/// it does not end.
fn element_end(xmp: &[u8], mut from: usize, name: &[u8]) -> Option<usize> {
    let mut depth = 1;
    while let Some(found) = xmp[from..].iter().position(|&c| c == b'<') {
        let at = from + found;
        let rest = &xmp[at..];
        if let Some(length) = not_element_length(rest) {
            from = at + length?;
            continue;
        }
        let closed = rest.strip_prefix(b"</").and_then(|r| r.strip_prefix(name));
        if closed.is_some_and(|r| r.first() == Some(&b'>')) {
            depth -= 1;
            from = at + name.len() + 3;
            if depth == 0 {
                return Some(from);
            }
            continue;
        }
        let gt = start_tag_end(xmp, at)?;
        let after_name = rest[1..].strip_prefix(name).and_then(|r| r.first());
        if after_name.is_some_and(|&c| ends_a_name(c)) && xmp[gt - 1] != b'/' {
            depth += 1;
        }
        from = gt + 1;
    }
    None
}

/// `text`, an XML value, with its entities and character references read. The text is a value,
/// not markup, so it has no comment to pass over (`not_element_length`).
pub(crate) fn unescape(text: &str) -> String {
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

/// Leaves the property `name` out of `xmp`, written as an attribute (` name="…"` or with single
/// quotes) or as an element (`<name …>…</name>` or `<name …/>`), every time it is a top-level
/// property of an rdf:Description (`top_level_properties`). One inside another property is
/// left as it is, such as the rating of another document in xmpMM:Pantry. An element goes with
/// the white space before it, so a sidecar written again and again does not gather empty lines.
/// Every XMP editor that replaces a property takes the old one out here, as a search for one
/// spelling misses the others (`xmp:Rating='3'`) and leaves the property twice. A property with
/// a comment or a CDATA section inside it goes whole too, as the walk reads its end past them.
/// Before, one with a comment was left, and the editor then wrote the property a second time.
fn without_property(xmp: &[u8], name: &str) -> Vec<u8> {
    let (mut out, mut copied) = (Vec::with_capacity(xmp.len()), 0);
    for found in properties_named(xmp, name) {
        let start = found.whole.start;
        let start = match found.is_attribute {
            true if start > copied && xmp[start - 1].is_ascii_whitespace() => start - 1,
            true => start,
            false => copied + xmp[copied..start].trim_ascii_end().len(),
        };
        out.extend_from_slice(&xmp[copied..start]);
        copied = found.whole.end;
    }
    out.extend_from_slice(&xmp[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::Packet;

    /// A namespace declared only inside a comment, a CDATA section or a processing instruction
    /// is not in force at the first Description, so a writer declares it where it writes
    /// (xmp's `markup_that_is_no_element_is_passed_over`).
    #[test]
    fn a_namespace_declared_in_markup_that_is_no_element_is_not_in_force() {
        let hidden = "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
            <!-- <rdf:Description xmlns:lr=\"http://ns.adobe.com/lightroom/1.0/\"/> -->\
            <?note xmlns:dc='http://purl.org/dc/elements/1.1/'?>\
            <![CDATA[ xmlns:x='u' ]]><rdf:Description rdf:about=\"\"/></rdf:RDF>";
        let (_, declared) = super::first_description(hidden).unwrap();
        assert_eq!(
            declared,
            [("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")]
        );
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

    /// Packets broken in thousands of ways: their properties read, and a packet written of them,
    /// without a panic.
    #[test]
    fn survives_broken_xmp_properties() {
        let seed = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
                    rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
                    dc:format='image/jpeg'><!--c--><dc:subject><rdf:Bag><rdf:li>a</rdf:li>\
                    <rdf:li><rdf:Description dc:x='1' dc:y=\"2\"/></rdf:li><dc:z><dc:z>3\
                    </dc:z></dc:z></rdf:Bag></dc:subject><dc:rights/></rdf:Description>\
                    <rdf:Description rdf:about=\"\" xmlns:dc='urn:terms'><dc:subject \
                    xmlns:dc='urn:dc'>b</dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>";
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
            let text = String::from_utf8_lossy(&bytes);
            let read = super::properties(&text);
            crate::metadata::xmp::packet(&read);
            super::properties_named(&bytes, "dc:subject");
            if let Some((_, declared)) = super::first_description(&text) {
                crate::metadata::xmp::prefix_in_force(
                    &declared,
                    "dc",
                    "http://purl.org/dc/elements/1.1/",
                );
            }
            for (name, element, namespaces) in &read.list {
                super::with_own_value_only(element, namespaces, &|n| n != name);
            }
        }
    }

    /// A motion photo's directory broken in thousands of ways: the fields of its items read in
    /// each of their forms, without a panic.
    #[test]
    fn survives_broken_structure_items() {
        let seed = "<rdf:Description><Container:Directory><rdf:Seq><rdf:li \
                    rdf:parseType='Resource'><Container:Item Item:Semantic=\"Primary\"/>\
                    </rdf:li><rdf:li><Container:Item rdf:parseType='Resource'><!-- c -->\
                    <Item:Length>12</Item:Length><Item:Padding>0</Item:Padding>\
                    </Container:Item></rdf:li><rdf:li><Container:Item><rdf:Description \
                    Item:Mime='video/mp4'><Item:Length>7</Item:Length></rdf:Description>\
                    </Container:Item></rdf:li></rdf:Seq></Container:Directory></rdf:Description>";
        let read = |xmp: &[u8]| {
            let found = super::top_level_properties(xmp);
            let directory = found.into_iter().find(|f| !f.is_attribute);
            let is_item = |name: &[u8]| name == b"Container:Item";
            directory.map(|d| super::structure_items(xmp, d.value, is_item))
        };
        let lengths: Vec<Vec<&str>> = (read(seed.as_bytes()).unwrap().iter())
            .map(|fields| fields.iter().map(|f| &seed[f.value.clone()]).collect())
            .collect();
        assert_eq!(
            lengths,
            [vec!["Primary"], vec!["12", "0"], vec!["video/mp4", "7"]]
        );
        for bytes in crate::support::test_support::broken(seed.as_bytes()) {
            read(&bytes);
        }
    }

    /// A default namespace puts an element's name without a prefix in it, for the readers of
    /// what follows a JPEG's image. The writers, which write only names with a prefix, never
    /// see it. Before, it was not read, and such a name was in no namespace.
    #[test]
    fn a_default_namespace_is_read_for_names_without_a_prefix() {
        let xmp = "<rdf:Description xmlns:p = 'urn:p'><Directory xmlns = 'urn:d'/>\
            <!-- <a xmlns='urn:hidden'/> --></rdf:Description>";
        let all = super::namespace_declarations_with_default(xmp);
        assert_eq!(all, [("p", "urn:p"), ("", "urn:d")]);
        assert_eq!(super::first_description(xmp).unwrap().1, [("p", "urn:p")]);
        let all = all.into_iter().collect();
        let is = |name: &[u8], uri: &str| super::is_in_namespace(name, &all, &[uri], "Directory");
        assert!(is(b"Directory", "urn:d"));
        assert!(is(b"p:Directory", "urn:p"));
        assert!(!is(b"Directory", "urn:p"));
    }

    /// The XMP's orientation upright in both forms; nothing else changed, and broken XMP is
    /// survived.
    #[test]
    fn sets_xmp_upright() {
        let upright = |xmp: &[u8]| {
            Packet::from_bytes(xmp.to_vec())
                .with_orientation(1)
                .into_bytes()
        };
        let xmp = upright(br#"<rdf:Description tiff:Orientation="6" a="6"><tiff:Orientation>8</tiff:Orientation></rdf:Description>"#);
        assert_eq!(
            xmp,
            br#"<rdf:Description tiff:Orientation="1" a="6"><tiff:Orientation>1</tiff:Orientation></rdf:Description>"#
        );
        // The orientation under another prefix bound to TIFF's namespace is set too.
        let other = upright(
            br#"<rdf:Description xmlns:ns1="http://ns.adobe.com/tiff/1.0/" ns1:Orientation="6"/>"#,
        );
        assert!(other.ends_with(br#"ns1:Orientation="1"/>"#));
        let odd = b"<rdf:Description tiff:Orientation='12' x:y='1'><tiff:Orientation/>";
        assert_eq!(upright(odd), odd);
    }

    #[test]
    fn survives_broken_xmp_orientation() {
        for xmp in crate::support::test_support::broken(
            br#"<rdf:Description tiff:Orientation="6"><tiff:Orientation>8</tiff:Orientation></rdf:Description>"#,
        ) {
            Packet::from_bytes(xmp).with_orientation(1);
        }
    }

    /// A property is left out of XMP in one pass, also from a hostile packet, and never through
    /// a comment. Before, 100,000 properties to leave out moved the rest of the packet each
    /// time, and a `<!--<xmp:Thumbnails>-->` comment was cut to the next end tag, so the
    /// comment's end went and the packet was broken.
    #[test]
    fn leaves_xmp_properties_out_in_one_pass() {
        // Each text is the rest of a Description's start tag and what it holds.
        let without_thumbnails = |x: &str| {
            let packet = format!("<rdf:Description{x}</rdf:Description>");
            let out = Packet::from_bytes(packet.into_bytes()).without_property("xmp:Thumbnails");
            let out = String::from_utf8(out.into_bytes()).unwrap();
            let inside = out.strip_prefix("<rdf:Description").unwrap();
            inside
                .strip_suffix("</rdf:Description>")
                .unwrap()
                .to_owned()
        };
        let kept = "><!--<xmp:Thumbnails>--><a/>";
        assert_eq!(
            without_thumbnails(&format!("{kept}<xmp:Thumbnails>t</xmp:Thumbnails><b/>")),
            format!("{kept}<b/>")
        );
        // A property with a comment or CDATA section inside it goes whole, also when that
        // holds what looks like its end tag.
        for inside in [
            "<!-- -->",
            "<!-- </xmp:Thumbnails> -->",
            "<![CDATA[</xmp:Thumbnails>]]>",
        ] {
            let commented = format!("><xmp:Thumbnails>t{inside}</xmp:Thumbnails><b/>");
            assert_eq!(without_thumbnails(&commented), "><b/>");
        }
        assert_eq!(
            without_thumbnails(
                " xmp:Thumbnails='1' b=\"2\" xmp:Thumbnails = \"3\"><xmp:Thumbnails/>"
            ),
            " b=\"2\">"
        );
        let started = std::time::Instant::now();
        let many = " xmp:Thumbnails=\"1\"".repeat(100_000);
        assert_eq!(without_thumbnails(&format!("{many}>")), ">");
        let open = format!(">{}", "<xmp:Thumbnails>".repeat(100_000));
        assert_eq!(without_thumbnails(&open), open);
        let unclosed = format!(">{}", "<xmp:Thumbnails".repeat(100_000));
        assert_eq!(without_thumbnails(&unclosed), unclosed);
        let comments = format!(">{}", "<!-- -->".repeat(100_000));
        assert_eq!(without_thumbnails(&comments), comments);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    /// A packet read and written back unchanged is the same bytes: the boundary of `Packet`
    /// keeps what is not UTF-8 too, which a copy and a turn keep byte for byte
    /// (`save::derived_xmp`), and the readers of text read such a packet as one with nothing in
    /// it, as they did before the type came in. The risk is that a packet changes on its way
    /// through `Packet` when nothing in it was edited.
    #[test]
    fn a_packet_is_written_back_byte_for_byte() {
        let text = "<?xpacket begin=\"\u{feff}\"?>\r\n<x:xmpmeta xmlns:x='adobe:ns:meta/'>\
            <rdf:RDF><rdf:Description rdf:about='' xmlns:tiff='http://ns.adobe.com/tiff/1.0/' \
            xmlns:dc='http://purl.org/dc/elements/1.1/' tiff:Orientation='1'>\r\n\
            <!-- a comment --><dc:subject><rdf:Bag><rdf:li>\u{c5}lesund</rdf:li></rdf:Bag>\
            </dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>\r\n<?xpacket end='w'?>";
        // The same packet in Latin-1, as an old writer may have left it inside an image.
        let latin1 = text.replace('\u{c5}', "\u{1}").into_bytes();
        let latin1: Vec<u8> = (latin1.iter())
            .map(|&b| if b == 1 { 0xc5 } else { b })
            .collect();
        assert!(std::str::from_utf8(&latin1).is_err());
        for bytes in [text.as_bytes().to_vec(), latin1.clone()] {
            let packet = Packet::from_bytes(bytes.clone());
            assert_eq!(packet.bytes_to_write(), bytes);
            assert_eq!(packet.without_property("xmp:Thumbnails"), packet);
            assert_eq!(packet.with_orientation(1), packet);
            for how in [
                crate::images::save::Derived::Turned,
                crate::images::save::Derived::Copy,
            ] {
                let derived = crate::images::save::derived_xmp(&packet, how);
                assert_eq!(derived.into_bytes(), bytes);
            }
        }
        let latin1 = Packet::from_bytes(latin1);
        assert!(latin1.properties().list.is_empty());
        assert!(latin1.items_with_languages("dc:subject").is_empty());
        assert_eq!(
            Packet::for_test(text)
                .items_with_languages("dc:subject")
                .len(),
            1
        );
    }
}
