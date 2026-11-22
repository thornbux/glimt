//! The file list: which files are images (by extension and file type), reading a folder or a tree
//! of them, and sorting them (by name as numbers, date, size or EXIF date).

use crate::files::external::{is_heif, is_video};
use crate::files::file_io::Identity;
use crate::images::save::FileId;
use crate::support::threads::lock_even_if_poisoned;
use crate::{images::raw, metadata::exif};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub const EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "tif", "tiff", "bmp", "ico", "qoi", "pbm", "pgm", "ppm",
    "pnm", "pam", "hdr", "lsm", "gel",
];

/// TIFF under other names: Zeiss LSM microscopy and GE Typhoon gels (square-root coded, shown as
/// stored). Only their first page: an LSM file's others are thumbnails.
pub const TIFF_LIKE: &[&str] = &["tif", "tiff", "lsm", "gel"];

/// HEIC/HEIF photos (iPhone), decoded by ffmpeg with the heic feature.
pub const HEIC_EXTS: &[&str] = if cfg!(feature = "heic") {
    &["heic", "heif", "hif"]
} else {
    &[]
};

/// AVIF photos, decoded by ffmpeg (dav1d) with the avif feature.
pub const AVIF_EXTS: &[&str] = if cfg!(feature = "avif") {
    &["avif"]
} else {
    &[]
};

/// JPEG XL photos, with the jxl feature.
pub const JXL_EXTS: &[&str] = if cfg!(feature = "jxl") { &["jxl"] } else { &[] };

/// SVG images, with the svg feature.
pub const SVG_EXTS: &[&str] = if cfg!(feature = "svg") {
    &["svg", "svgz"]
} else {
    &[]
};

/// Each order with its config value and menu label.
pub const SORTS: [(Sort, &str, &str); 8] = [
    (Sort::Name, "name", "Sort by name"),
    (Sort::Number, "number", "Sort by number"),
    (Sort::Date, "date", "Sort by date"),
    (Sort::Created, "created", "Sort by file creation date"),
    (
        Sort::ExifOriginal,
        "exif_original",
        "Sort by EXIF date original",
    ),
    (
        Sort::ExifDigitized,
        "exif_digitized",
        "Sort by EXIF date digitized",
    ),
    (Sort::Size, "size", "Sort by size"),
    (Sort::Extension, "extension", "Sort by extension"),
];

/// How long Glimt's own reading of a share waits after a program was started on a file
/// (`program_started`). A video player on a share reads the start of its file then, and the
/// share serves Glimt's look at each file of a tree between the player's reads.
const PROGRAM_STARTS_FOR: std::time::Duration = std::time::Duration::from_secs(3);

/// How long the walk of a tree on a share waits for an image that the window waits for
/// (`ImagesAwaited`). An image whose decode takes longer, on a share that stopped answering, no
/// longer holds the walk.
const IMAGE_AWAITED_FOR: std::time::Duration = std::time::Duration::from_secs(5);

/// How long the walk of a tree on a share waits at most, between two folders, for the images
/// that the window waits for (`ImagesAwaited`). A slideshow of four panes with a delay of a
/// second awaits four decodes after each other, and a held arrow key one after the other, so
/// the walk would wait for as long as they go on. With this limit it reads a folder at least
/// once in every second and the time that a folder takes to read, and the image awaited still
/// has a second of the share to itself before each folder.
const WALK_WAITS_FOR_IMAGES_AT_MOST: std::time::Duration = std::time::Duration::from_secs(1);

/// How many looks at files in a listing's thread share one budget of
/// `WALK_WAITS_FOR_IMAGES_AT_MOST` to wait for the images awaited (`ReadsFirst`). On a share,
/// where a look at a file's EXIF date takes about 5 ms, that is a pause of at most a second after
/// about half a second of looks.
const LOOKS_PER_PAUSE: u64 = 100;

/// The folders where a listing found a link to a file (`holds_links`).
/// shortcut: it only grows, by one path for each such folder listed; forget a folder at its
/// next listing when a session lists very many of them.
static FOLDERS_WITH_LINKS: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Whether a look or read on a share that nobody waits for holds the one turn there is
/// (`BackgroundTurn`). Such work goes one at a time, so that a share that serves its calls one at
/// a time always has the image the window waits for next in its queue.
static BACKGROUND_TURN_TAKEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// When a program was last started on the user's files: Play Video, Open With or a tool.
static PROGRAM_STARTED_AT: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
thread_local! {
    // How many programs this thread noted as started (`program_started`), so that a test can see
    // that a way to start a program on the user's files notes it, whatever other tests start.
    pub(crate) static PROGRAMS_STARTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // How many times this thread looked at a file for what it is sorted by (`sort_key`), so that
    // a test can count the looks of a listing that it drives in its own thread.
    pub(crate) static FILES_LOOKED_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // How many times this thread compared two rows with `in_order`, so that a test can count the
    // comparisons that a rename makes.
    pub(crate) static ROWS_COMPARED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What a file is sorted by besides its name, in an order that looks at the file: its time,
/// its size or its EXIF date, as `Rows::sorted` sorts by it. Each row of the list carries it in
/// such an order (`Rows`), so that a part of a tree goes into its place by it, and the window
/// looks at no file.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey {
    /// An order by the name alone.
    #[default]
    Name,
    /// The modification or creation time in nanoseconds since 1970 (0 when unknown, and the
    /// largest number after the year 2554), or `None` when the file could not be looked at.
    /// Its 8 bytes keep the key at 32 bytes, where 16 bytes made it 48.
    Time(Option<u64>),
    /// The size in bytes, or `None` when the file could not be looked at.
    Size(Option<u64>),
    /// Whether the file has no EXIF date, so that those come last, and the date.
    Exif(bool, Option<String>),
}

/// Where the names of a list went when a part of a tree was merged into it
/// (`Rows::merged`): the places of the names that the part left out, and the places among
/// the names that stayed before which the part's names went. The window carries what it holds
/// by place (the decoded images, the decodes on their way, the panes) through it, with no look
/// at a name.
#[derive(Debug, Default, PartialEq)]
pub struct PlacesMoved {
    /// The places in the list before of the names the part left out, in order.
    gone: Vec<usize>,
    /// For each name of the part, the place among the names that stayed before which it went.
    at: Vec<usize>,
}

impl PlacesMoved {
    /// Returns the place in the merged list of the name at place `i` of the list before, or
    /// `None` when the part left it out.
    pub fn new_place(&self, i: usize) -> Option<usize> {
        if self.gone.binary_search(&i).is_ok() {
            return None;
        }
        let stayed = i - self.gone.partition_point(|&g| g < i);
        Some(stayed + self.at.partition_point(|&a| a <= stayed))
    }
}

/// What the rows of a list or of a part of one are sorted in, as `Rows` holds it.
#[derive(Clone, Debug, Default, PartialEq)]
enum RowOrder {
    /// In no sort order: a collection's or a plain list's own order, or names taken as they
    /// came, such as the one image opened.
    #[default]
    Unsorted,
    /// In an order by the names alone (Name, Number or Extension), as (order, ascending, case).
    ByName((Sort, bool, bool)),
    /// In an order that looks at the files (Date, Created, Size or an EXIF date), as (order,
    /// ascending, case), with what each row is sorted by in that order, at the row's place.
    ByKey((Sort, bool, bool), Vec<SortKey>),
}

/// Where a part's rows go among a list's rows (`Rows::places_of`).
enum Places {
    /// The part is in another order than the list.
    Refused,
    /// The part brings nothing new and takes nothing out, so the list stays as it is.
    AsTheyAre,
    /// The places of the list's rows that the part takes out, and for each of the part's rows
    /// the place among the rows that stay before which it goes (`PlacesMoved`).
    Moved(Vec<usize>, Vec<usize>),
}

/// The rows of the file list, or of a part of a tree's listing: the names with the identities
/// they were listed with, in the order the rows are in. In an order that looks at the files,
/// each row carries what it is sorted by in that very order. Rows are made only in this module,
/// and rows in such an order only with a key of that order for each row: by `Rows::sorted`, and
/// by what is made from rows (a merge, a filter, places taken out), which carries each row's
/// key with it. So no list or part in such an order exists without its keys, and the keys of
/// two orders are never compared, because a part goes into a list only in the order the list
/// is in (`Rows::merged`). Before, the list kept its keys apart from its order, and a list left
/// without them, or with the keys of another order, took the parts of a tree in the wrong
/// places.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    files: Vec<OsString>,
    identities: Vec<Identity>,
    order: RowOrder,
}

impl Rows {
    /// Returns rows of `names` as they are, in no sort order.
    pub fn unsorted(names: Vec<ListedName>) -> Rows {
        let (files, identities) = names.into_iter().unzip();
        Rows {
            files,
            identities,
            order: RowOrder::Unsorted,
        }
    }

    /// Returns rows of `names` in `order`, as the Sort menu puts them. In an order that looks at
    /// the files, each name is given what it is sorted by from `key`, which is asked once for
    /// each name, with its place in `names`, and the rows are put in order as `in_order`
    /// compares them. `key` is where a file is looked at, so the window passes none that looks.
    pub fn sorted(
        mut names: Vec<ListedName>,
        order: (Sort, bool, bool),
        mut key: impl FnMut(usize, &OsString) -> SortKey,
    ) -> Rows {
        let (by, ascending, case) = order;
        if !by.looks_at_files() {
            // The names come in any order (a tree's walk, a collection's own). `sort_by_names`
            // keeps the names that the order holds equal in the order it is given them, so they
            // are put in byte order first, which is how `in_order` tells them apart.
            names.sort_unstable();
            sort_by_names(&mut names, by, ascending, case);
            let (files, identities) = names.into_iter().unzip();
            return Rows {
                files,
                identities,
                order: RowOrder::ByName(order),
            };
        }
        // The name as the order compares it is made once for each file, and the bytes only
        // where both are the same, as `in_order` compares them.
        let name = |f: &OsString| {
            let s = f.to_string_lossy();
            match case {
                true => s.into_owned(),
                false => s.to_lowercase(),
            }
        };
        let mut keyed: Vec<(SortKey, String, ListedName)> = (names.into_iter().enumerate())
            .map(|(i, f)| (key(i, &f.0), name(&f.0), f))
            .collect();
        keyed.sort_unstable_by(|a, b| {
            (a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.0.as_encoded_bytes().cmp(b.2.0.as_encoded_bytes()))
        });
        if !ascending {
            keyed.reverse();
        }
        let mut rows = Rows::empty_in(&RowOrder::ByKey(order, Vec::new()), keyed.len());
        for (key, _, (file, identity)) in keyed {
            rows.push((file, identity), Some(key));
        }
        rows
    }

    /// Returns no rows, in the order of `like`, with room for `n`.
    fn empty_in(like: &RowOrder, n: usize) -> Rows {
        Rows {
            files: Vec::with_capacity(n),
            identities: Vec::with_capacity(n),
            order: match like {
                RowOrder::Unsorted => RowOrder::Unsorted,
                RowOrder::ByName(order) => RowOrder::ByName(*order),
                RowOrder::ByKey(order, _) => RowOrder::ByKey(*order, Vec::with_capacity(n)),
            },
        }
    }

    /// Adds the row of `name` at the end, with `key`, what it is sorted by, in an order that
    /// looks at the files. Every caller has a key for each row of such an order, as it takes
    /// them from rows of that order.
    fn push(&mut self, (file, identity): ListedName, key: Option<SortKey>) {
        self.files.push(file);
        self.identities.push(identity);
        if let (RowOrder::ByKey(_, keys), Some(key)) = (&mut self.order, key) {
            keys.push(key);
        }
    }

    /// The names of the rows, in their order.
    pub fn files(&self) -> &[OsString] {
        &self.files
    }

    /// Returns the identity of the file of the row at `i` as it was listed, or `None` when it
    /// is not known.
    pub fn identity(&self, i: usize) -> Identity {
        self.identities.get(i).copied().flatten()
    }

    /// The number of rows.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Returns the order the rows are in, as (order, ascending, case), or `None` when they
    /// are in no sort order.
    pub fn order(&self) -> Option<(Sort, bool, bool)> {
        match &self.order {
            RowOrder::Unsorted => None,
            RowOrder::ByName(order) | RowOrder::ByKey(order, _) => Some(*order),
        }
    }

    /// Returns what the row at `i` is sorted by, in an order that looks at the files, or
    /// `None` in another order.
    pub fn key(&self, i: usize) -> Option<&SortKey> {
        match &self.order {
            RowOrder::ByKey(_, keys) => keys.get(i),
            _ => None,
        }
    }

    /// Returns the names with their identities, in the rows' order.
    pub fn listed_names(&self) -> Vec<ListedName> {
        (self.files.iter().cloned())
            .zip(self.identities.iter().copied())
            .collect()
    }

    /// Returns whether `other` holds the names of these rows, in their order and with the
    /// identities they have here.
    pub fn holds_the_files_of(&self, other: &Rows) -> bool {
        self.files == other.files && self.identities == other.identities
    }

    /// Keeps only the rows whose names `keep` holds to, each with its key.
    pub fn retain(&mut self, keep: impl FnMut(&OsString) -> bool) {
        let kept: Vec<bool> = self.files.iter().map(keep).collect();
        self.keep_places(|i| kept[i]);
    }

    /// Takes the rows at the sorted places `removed` out, each with its key, in one pass.
    pub fn remove_places(&mut self, removed: &[usize]) {
        self.keep_places(|i| removed.binary_search(&i).is_err());
    }

    /// Keeps the rows at the places that `keep` holds to, with their keys.
    fn keep_places(&mut self, keep: impl Fn(usize) -> bool) {
        fn retain<T>(items: &mut Vec<T>, keep: &impl Fn(usize) -> bool) {
            let mut i = 0;
            items.retain(|_| {
                i += 1;
                keep(i - 1)
            });
        }
        retain(&mut self.files, &keep);
        retain(&mut self.identities, &keep);
        if let RowOrder::ByKey(_, keys) = &mut self.order {
            retain(keys, &keep);
        }
    }

    /// Gives the rows at the places of `renames` their new names, because their files were
    /// renamed or moved within the list. Each keeps its identity, as a rename keeps the inode,
    /// and its key, as a rename keeps the file's times and size. Returns `None` when every row
    /// is still in the order the rows are in. Otherwise it returns the rows put in their order
    /// by their own comparison (`in_order`), with no look at a file, and leaves these rows in no
    /// order, so that they never claim an order they are not in. The caller then replaces them
    /// with the rows returned. Only the renamed rows are out of place, so each of them is put
    /// back by a binary search among the rows that stayed, and the rows are copied once. Before,
    /// every row was sorted again on the window thread, which took 40 ms at 500,000 names.
    pub fn rename(&mut self, renames: &[(usize, OsString)]) -> Option<Rows> {
        for (i, name) in renames {
            if let Some(file) = self.files.get_mut(*i) {
                *file = name.clone();
            }
        }
        let order = self.order()?;
        let none = SortKey::Name;
        let row = |i: usize| (self.key(i).unwrap_or(&none), &self.files[i]);
        let in_place = |i: usize| {
            let after =
                |a: usize| a + 1 >= self.len() || in_order(row(a), row(a + 1), order).is_le();
            (i == 0 || after(i - 1)) && after(i)
        };
        if (renames.iter()).all(|(i, _)| *i >= self.len() || in_place(*i)) {
            return None;
        }
        let mut gone: Vec<usize> = (renames.iter().map(|(i, _)| *i))
            .filter(|&i| i < self.len())
            .collect();
        gone.sort_unstable();
        gone.dedup();
        let mut moved = gone.clone();
        moved.sort_by(|&a, &b| in_order(row(a), row(b), order));
        let mut new = Rows::empty_in(&self.order, moved.len());
        for i in moved {
            new.push(
                (self.files[i].clone(), self.identities[i]),
                self.key(i).cloned(),
            );
        }
        let at = self.places_among_staying(&gone, &new, order);
        let old = (0..self.len()).map(|i| {
            let row = (self.files[i].clone(), self.identities[i]);
            (row, self.key(i).cloned())
        });
        let sorted = Rows::interleaved(&self.order, (old, self.len()), &gone, new, &at);
        self.order = RowOrder::Unsorted;
        Some(sorted)
    }

    /// Forgets the identities of the files named `names`, in one pass over the rows.
    pub fn forget_identities(&mut self, names: &HashSet<&OsStr>) {
        for (name, identity) in self.files.iter().zip(&mut self.identities) {
            if names.contains(name.as_os_str()) {
                *identity = None;
            }
        }
    }

    /// Merges `new`, a part of a tree of only images that these rows lack, into these rows, the
    /// list, so that the list keeps its order and grows. The part leaves out the rows that the
    /// folders it `read` whole no longer hold (`ReadWhole::leaves_out`). Returns the merged rows
    /// with where the rows of the list went, and `new` back when it is in another order than
    /// these rows. An empty list takes the part in its order. A part of new names only needs no
    /// look at the rows of the list to tell which are new, so merging it takes a few comparisons
    /// for each of its own rows and a copy of the others, and no look at a file. In an order that
    /// looks at the files, the rows are placed by their keys, which are of that one order in
    /// both. Before, a part could be placed by the keys of another order than its own.
    pub fn merged(&self, new: Rows, read: &ReadWhole) -> Result<(Rows, PlacesMoved), Rows> {
        let (gone, at) = match self.places_of(&new, read) {
            Places::Refused => return Err(new),
            Places::AsTheyAre => return Ok((self.clone(), PlacesMoved::default())),
            Places::Moved(gone, at) => (gone, at),
        };
        let like = self.order_after_merging(&new);
        let old = (0..self.len()).map(|i| {
            let row = (self.files[i].clone(), self.identities[i]);
            (row, self.key(i).cloned())
        });
        let merged = Rows::interleaved(&like, (old, self.len()), &gone, new, &at);
        Ok((merged, PlacesMoved { gone, at }))
    }

    /// Merges `new` into these rows as `merged` does, taking these rows' names and keys instead
    /// of copying them, for rows that the caller owns, such as the list's rows put in another
    /// order. Returns `new` back when it is in another order than these rows.
    pub fn merged_into(self, new: Rows, read: &ReadWhole) -> Result<Rows, Rows> {
        let (gone, at) = match self.places_of(&new, read) {
            Places::Refused => return Err(new),
            Places::AsTheyAre => return Ok(self),
            Places::Moved(gone, at) => (gone, at),
        };
        let (like, n) = (self.order_after_merging(&new), self.len());
        let Rows {
            files,
            identities,
            order,
        } = self;
        let mut keys = match order {
            RowOrder::ByKey(_, keys) => keys.into_iter(),
            _ => Vec::new().into_iter(),
        };
        let old = (files.into_iter().zip(identities)).map(|row| (row, keys.next()));
        Ok(Rows::interleaved(&like, (old, n), &gone, new, &at))
    }

    /// Returns the order of these rows with a part merged in, without keys: their own, or the
    /// part's when they are none.
    fn order_after_merging(&self, new: &Rows) -> RowOrder {
        let like = match self.is_empty() {
            true => &new.order,
            false => &self.order,
        };
        Rows::empty_in(like, 0).order
    }

    /// Returns where the rows of `new`, a part, go among these rows, and which of these the
    /// folders it `read` whole no longer hold (`merged`).
    fn places_of(&self, new: &Rows, read: &ReadWhole) -> Places {
        let same_order = match (&self.order, &new.order) {
            (RowOrder::ByName(a), RowOrder::ByName(b))
            | (RowOrder::ByKey(a, _), RowOrder::ByKey(b, _)) => a == b,
            _ => false,
        };
        let Some(order) = new.order().filter(|_| same_order || self.is_empty()) else {
            return Places::Refused;
        };
        // A look at each name only when a folder read may have lost one.
        let gone: Vec<usize> = match read.is_empty() {
            true => Vec::new(),
            false => (0..self.len())
                .filter(|&i| read.leaves_out(&self.files[i]))
                .collect(),
        };
        if new.is_empty() && gone.is_empty() {
            return Places::AsTheyAre;
        }
        let at = self.places_among_staying(&gone, new, order);
        Places::Moved(gone, at)
    }

    /// Returns, for each row of `new`, rows in `order`, the place among the rows that stay
    /// when those at the sorted places `gone` are taken out, before which it goes
    /// (`interleaved`). Each place is found by a binary search with `in_order`, after the place
    /// of the row before.
    fn places_among_staying(
        &self,
        gone: &[usize],
        new: &Rows,
        order: (Sort, bool, bool),
    ) -> Vec<usize> {
        let stayed: Vec<usize> = match gone.is_empty() {
            true => Vec::new(),
            false => (0..self.len())
                .filter(|i| gone.binary_search(i).is_err())
                .collect(),
        };
        let old_place = |s: usize| match gone.is_empty() {
            true => s,
            false => stayed[s],
        };
        let staying = self.len() - gone.len();
        let none = SortKey::Name;
        let old_key = |i: usize| self.key(i).unwrap_or(&none);
        let new_key = |j: usize| new.key(j).unwrap_or(&none);
        // The new names are in order, so each one is looked for after the one before.
        let mut from = 0;
        (0..new.len())
            .map(|j| {
                let (mut low, mut high) = (from, staying);
                while low < high {
                    let middle = (low + high) / 2;
                    let i = old_place(middle);
                    let before = (old_key(i), &self.files[i]);
                    match in_order(before, (new_key(j), &new.files[j]), order).is_le() {
                        true => low = middle + 1,
                        false => high = middle,
                    }
                }
                from = low;
                low
            })
            .collect()
    }

    /// Returns the rows `old`, of which there are as many as it is given with, each with its
    /// key, but for those at the sorted places `gone`, with the rows of `new` put in among them:
    /// each before the row that stays at its place of `at`, counted among the rows that stay,
    /// or at the end. The rows are in the order of `like`.
    fn interleaved(
        like: &RowOrder,
        (old, n): (impl Iterator<Item = (ListedName, Option<SortKey>)>, usize),
        gone: &[usize],
        new: Rows,
        at: &[usize],
    ) -> Rows {
        let mut merged = Rows::empty_in(like, n - gone.len() + new.len());
        let Rows {
            files,
            identities,
            order: new_order,
        } = new;
        let mut new_keys = match new_order {
            RowOrder::ByKey(_, keys) => keys.into_iter(),
            _ => Vec::new().into_iter(),
        };
        let mut new_rows = (files.into_iter().zip(identities))
            .zip(at.iter().copied())
            .peekable();
        let staying = old
            .enumerate()
            .filter(|(i, _)| gone.binary_search(i).is_err());
        for (s, (_, (row, key))) in staying.enumerate() {
            while let Some((new_row, _)) = new_rows.next_if(|&(_, at)| at <= s) {
                merged.push(new_row, new_keys.next());
            }
            merged.push(row, key);
        }
        for (row, _) in new_rows {
            merged.push(row, new_keys.next());
        }
        merged
    }

    /// Returns the rows of `kept`, names of these rows in their order with the identities that
    /// a new listing gave them, in the order of these rows, each with what it is sorted by here.
    /// That is a whole list that kept the list's order (`kept_order`). It walks both in step,
    /// with no map of the names. Returns `None` when a name of `kept` is not among these rows
    /// in their order, as when its file left the list after the listing began. Before, such a
    /// list was taken without its keys.
    pub fn in_step(&self, kept: Vec<ListedName>) -> Option<Rows> {
        let mut own = 0..self.len();
        let mut rows = Rows::empty_in(&self.order, kept.len());
        for (file, identity) in kept {
            let i = own.find(|&i| self.files[i] == file)?;
            rows.push((file, identity), self.key(i).cloned());
        }
        Some(rows)
    }

    /// Returns these rows with the rows of `kept` put in, each list of them after as many of
    /// these rows as its number in `kept` says. In an order that looks at the files, each row
    /// put in takes what the row before it is sorted by, or the first row's when it goes
    /// first, so that the keys stay in their order. Rows put into no rows of their own are in no
    /// sort order.
    pub fn with_rows_put_in(self, mut kept: HashMap<usize, Vec<ListedName>>) -> Rows {
        if kept.is_empty() {
            return self;
        }
        if self.is_empty() {
            return Rows::unsorted(kept.remove(&0).unwrap_or_default());
        }
        let put_in = kept.values().map(Vec::len).sum::<usize>();
        let mut rows = Rows::empty_in(&self.order, self.len() + put_in);
        let first_key = self.key(0).cloned();
        for row in kept.remove(&0).unwrap_or_default() {
            rows.push(row, first_key.clone());
        }
        let Rows {
            files,
            identities,
            order,
        } = self;
        let mut keys = match order {
            RowOrder::ByKey(_, keys) => keys.into_iter(),
            _ => Vec::new().into_iter(),
        };
        for (i, row) in files.into_iter().zip(identities).enumerate() {
            let key = keys.next();
            rows.push(row, key.clone());
            for row in kept.remove(&(i + 1)).unwrap_or_default() {
                rows.push(row, key.clone());
            }
        }
        rows
    }
}

/// The names that the list held when a listing began, as 64-bit hashes (`name_hash`), which
/// the listing thread looks new names up in. They cost 8 bytes a name and no copy of the names:
/// a copy of the list and a set over it held about 37 MB at 500,000 names for the whole walk.
/// A name whose hash another one shares only waits for the whole list. Beside them it holds how
/// many names each folder of the list has, by which the thread tells a folder it read whole that
/// no longer holds all of them (`ReadWhole`).
#[derive(Default)]
pub struct NameHashes {
    hashes: Vec<u64>,
    /// How many names of the list lie directly in each folder, relative to the tree ("" is its
    /// top).
    in_folder: std::collections::HashMap<Vec<u8>, u32>,
    /// For each folder above a folder of the list, the folders directly in it on the way there,
    /// which `sorted` works out in the listing thread.
    below: std::collections::HashMap<Vec<u8>, HashSet<Vec<u8>>>,
}

impl NameHashes {
    /// The hashes of `names`, in their order, and how many lie in each folder. `sorted` puts
    /// them in order to be looked up.
    pub fn of(names: &[OsString]) -> NameHashes {
        let mut in_folder: std::collections::HashMap<&[u8], u32> = Default::default();
        // The names of a folder mostly come together, so its count is looked up once for them.
        let mut last: Option<(&[u8], u32)> = None;
        let mut hashes = Vec::with_capacity(names.len());
        for name in names {
            hashes.push(name_hash(name.as_encoded_bytes()));
            let folder = folder_of(name.as_encoded_bytes());
            match &mut last {
                Some((at, count)) if *at == folder => *count += 1,
                _ => {
                    if let Some((at, count)) = last {
                        *in_folder.entry(at).or_default() += count;
                    }
                    last = Some((folder, 1));
                }
            }
        }
        if let Some((at, count)) = last {
            *in_folder.entry(at).or_default() += count;
        }
        let in_folder = (in_folder.into_iter())
            .map(|(folder, count)| (folder.to_vec(), count))
            .collect();
        NameHashes {
            hashes,
            in_folder,
            below: Default::default(),
        }
    }

    /// Puts the hashes in order, in the listing thread, for `holds`, and works out the folders
    /// below each folder for `note_folder`.
    pub fn sorted(mut self) -> NameHashes {
        self.hashes.sort_unstable();
        for folder in self.in_folder.keys() {
            let slashes = (folder.iter().enumerate()).filter(|(_, b)| **b == b'/');
            let ends = slashes.map(|(at, _)| at).chain([folder.len()]);
            let mut above: &[u8] = b"";
            for end in ends.filter(|&end| end > 0) {
                let child = &folder[..end];
                self.below
                    .entry(above.to_vec())
                    .or_default()
                    .insert(child.to_vec());
                above = child;
            }
        }
        self
    }

    /// Returns whether a name of these hashes, sorted, may be `name`.
    pub fn holds(&self, name: &std::ffi::OsStr) -> bool {
        (self.hashes)
            .binary_search(&name_hash(name.as_encoded_bytes()))
            .is_ok()
    }

    /// Whether no name was hashed, as for a listing whose parts are not new names only.
    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    /// Notes in `read` the folder of `folder` that the walk read whole, when the list may hold a
    /// name in or below it that is gone: fewer of the list's names were found in it than it
    /// had, or a folder of the list below it is not among those it holds now. A folder is judged
    /// gone only by the rules the walk read its parent by (`lists_folder_named`). One that the
    /// read leaves out by rule, a hidden folder while hidden files are not shown, is in the list
    /// because the walk reached it through a link, so only its own read tells which of its
    /// names are gone, and its names stay when its parent is read. A hidden folder listed while
    /// hidden files were shown is not among them: Show Hidden Files turned off takes its names
    /// out of the list before the next listing begins (`App::leave_out_names_not_shown`).
    pub fn note_folder(&self, folder: &FolderReadWhole, read: &mut ReadWhole) {
        let at = folder.folder.as_os_str().as_encoded_bytes();
        let images = &folder.so_far[folder.first..];
        let found = images.iter().filter(|f| self.holds(&f.0)).count();
        let fewer = self
            .in_folder
            .get(at)
            .is_some_and(|&had| had as usize > found);
        let subfolders: HashSet<Vec<u8>> = (folder.dirs.iter())
            .filter_map(|d| d.file_name())
            .map(|n| folder.folder.join(n).into_os_string().into_encoded_bytes())
            .collect();
        let below = self.below.get(at).into_iter().flatten();
        let is_read_by_rule = |child: &&Vec<u8>| {
            let name = child.rsplit(|&b| b == b'/').next().unwrap_or(child);
            lists_folder_named(name, folder.types)
        };
        let vanished = (below.clone())
            .filter(is_read_by_rule)
            .any(|child| !subfolders.contains(child));
        if fewer || vanished {
            read.folders.insert(at.to_vec());
            let names = images.iter().map(|f| name_hash(f.0.as_encoded_bytes()));
            let subfolders = subfolders.iter().map(|f| name_hash(f));
            let reached_through_links = below.filter(|c| !is_read_by_rule(c)).map(|c| name_hash(c));
            read.found
                .extend(names.chain(subfolders).chain(reached_through_links));
        }
    }
}

/// A folder of a tree that `list_tree` has read whole, as its `part` callback gets it.
pub struct FolderReadWhole<'a> {
    /// The images of the tree found so far, this folder's last.
    pub so_far: &'a [ListedName],
    /// How many of `so_far` were found before this folder.
    pub first: usize,
    /// The folder, relative to the tree.
    pub folder: &'a Path,
    /// The folders in it, as full paths.
    pub dirs: &'a [PathBuf],
    /// The types the walk lists, by which it left out a hidden folder.
    pub types: Types,
}

/// The folders that a part of a tree read whole where the list may hold a name that is gone,
/// with the hashes of every image and folder found in them. The window leaves out of the list
/// each name in such a folder, or below a folder of it, that was not found there
/// (`leaves_out`), so a folder renamed or an image moved is not shown twice, nor tried under a
/// name that is gone. Only the folders where the list's names were fewer are here, so the
/// window looks at its names only when a part has one.
#[derive(Default, Debug)]
pub struct ReadWhole {
    folders: HashSet<Vec<u8>>,
    found: HashSet<u64>,
}

impl ReadWhole {
    /// Whether no folder read may leave out a name.
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    /// Returns whether `name`, a name of the list relative to the tree, lies in a folder read
    /// whole that no longer holds it, or below a folder read whole that no longer holds the
    /// folder on its way. Each folder above it is one lookup.
    pub fn leaves_out(&self, name: &std::ffi::OsStr) -> bool {
        if self.folders.is_empty() {
            return false;
        }
        let name = name.as_encoded_bytes();
        let slashes = (name.iter().enumerate()).filter(|(_, b)| **b == b'/');
        let ends: Vec<usize> = slashes.map(|(at, _)| at).chain([name.len()]).collect();
        // The folder above each step on the way to the name: the top, then each folder.
        let mut above = 0;
        for end in ends {
            let folder = &name[..above];
            if self.folders.contains(folder) && !self.found.contains(&name_hash(&name[..end])) {
                return true;
            }
            above = end;
        }
        false
    }
}

/// The folders a listing read, each with the modification time it had when it was read, so the
/// folder watcher sees a change made after the reading. The time is `None` when it could not be
/// read.
pub type FoldersRead = Vec<(PathBuf, Option<std::time::SystemTime>)>;

/// A tree as `list_tree` reads it: the top folder's own folders, the images of the tree, the
/// folders read, and the folders below the top that could not be read.
pub type ListedTree = (Vec<PathBuf>, Vec<ListedName>, FoldersRead, Vec<Unread>);

/// A folder as `walk` reads it: its path relative to the tree, its folders, its images, and
/// its change time, taken before it was read.
pub type WalkedFolder = (
    PathBuf,
    Vec<PathBuf>,
    Vec<ListedName>,
    Option<std::time::SystemTime>,
);

/// A folder or file that a listing could not read, with why. When it is the one folder listed,
/// the listing fails, and the window tells it and keeps the list it shows. A part of the listing
/// (a folder of a tree or of several folders, an entry of a collection) is left out instead,
/// and the window tells it, unless the list shown holds files of that part.
pub type Unread = (PathBuf, std::io::Error);

/// An image's name in the list, with the identity its folder entry gave it when it was read.
/// The list keeps both, so that a write can tell when another file has taken the name since.
pub type ListedName = (OsString, Identity);

/// The file types the list shows, as Geeqie's file classes in its file filter.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Types {
    pub images: bool,
    pub raw: bool,
    /// With the video feature.
    pub videos: bool,
    /// Names starting with a dot, files and folders.
    pub hidden: bool,
}

impl Types {
    /// None: for reading only the folders.
    pub const NONE: Types = Types {
        images: false,
        raw: false,
        videos: false,
        hidden: false,
    };

    /// The type of the file at `path`.
    pub fn of(path: &Path) -> Types {
        let only = |t: Types| is_listed(path, t);
        Types {
            images: only(Types {
                images: true,
                ..Types::NONE
            }),
            raw: only(Types {
                raw: true,
                ..Types::NONE
            }),
            videos: only(Types {
                videos: true,
                ..Types::NONE
            }),
            hidden: is_hidden(path),
        }
    }
}

/// File list order, as in Geeqie's sort menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sort {
    Name,
    /// Numbers in names compared by value: "img2" before "img10".
    Number,
    /// Modification time.
    Date,
    Created,
    ExifOriginal,
    ExifDigitized,
    Size,
    /// File type: "jpg" before "mp4" before "png"; then by name.
    Extension,
}

impl Sort {
    /// Whether the order looks at each file, its date, size or EXIF date, which only a thread
    /// may do. The others go by the name alone.
    pub fn looks_at_files(self) -> bool {
        !matches!(self, Sort::Name | Sort::Number | Sort::Extension)
    }

    pub fn row(self) -> (Sort, &'static str, &'static str) {
        SORTS.into_iter().find(|s| s.0 == self).unwrap_or(SORTS[0])
    }
}

impl std::str::FromStr for Sort {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        SORTS
            .into_iter()
            .find(|s| s.1 == text)
            .map(|s| s.0)
            .ok_or(())
    }
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.row().1)
    }
}

/// The images that the window waits for and that are not decoded yet: the shown one, the other
/// panes, and the next one that the slideshow or an arrow key shows. The window says which they
/// are whenever they or their decodes change (`App::publish_awaited_images`), so an image read
/// ahead counts as soon as it becomes the next one. While one is there, the walk of a tree on a
/// share waits between folders, so that the share reads the image first. Before, whether a
/// decode held the walk was decided when the decode began, and with Preload 2 the next image
/// was mostly begun as an image read ahead, which held nothing. Each image is held with the
/// time the window began to wait for it, and it holds the walk for at most
/// `IMAGE_AWAITED_FOR`.
#[derive(Clone, Default)]
pub(crate) struct ImagesAwaited(
    std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, std::time::Instant)>>>,
);

impl ImagesAwaited {
    /// Notes that the window now waits for the images `paths`, and for no others. An image it
    /// waited for before keeps the time it began to wait for it.
    pub(crate) fn set(&self, paths: Vec<PathBuf>) {
        let mut awaited = lock_even_if_poisoned(&self.0);
        if paths.is_empty() && awaited.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let since = |path: &PathBuf| (awaited.iter()).find(|(p, _)| p == path).map(|(_, t)| *t);
        let new: Vec<(PathBuf, std::time::Instant)> = (paths.into_iter())
            .map(|path| (since(&path).unwrap_or(now), path))
            .map(|(began, path)| (path, began))
            .collect();
        *awaited = new;
    }

    /// Returns the images that the window waits for.
    #[cfg(test)]
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        (lock_even_if_poisoned(&self.0).iter())
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// Returns whether the window waits, at `now`, for an image that it began to wait for less
    /// than `IMAGE_AWAITED_FOR` before, unless it waits for `except` too. A decode read ahead
    /// that waits so goes on as soon as its own image is awaited.
    fn holds_the_walk_at(&self, now: std::time::Instant, except: Option<&Path>) -> bool {
        let awaited = lock_even_if_poisoned(&self.0);
        (except.is_none_or(|own| !awaited.iter().any(|(path, _)| path == own)))
            && (awaited.iter())
                .any(|(_, since)| now.saturating_duration_since(*since) < IMAGE_AWAITED_FOR)
    }

    /// Returns whether the window waits for the image `path`.
    fn awaits(&self, path: &Path) -> bool {
        (lock_even_if_poisoned(&self.0).iter()).any(|(p, _)| p == path)
    }
}

/// The one gate that every look and read Glimt makes in the background on a share goes
/// through, so that the share serves the image that the window waits for first. A share such
/// as gvfsd-smb serves its calls largely one at a time, so every look in the background stands
/// in the same queue as the reads of that image. Before a look or a read, the gate waits while a
/// program started on a file may still be reading its start, and while the window waits for an
/// image (`ImagesAwaited`). Elsewhere nothing waits.
///
/// Two kinds of work go through it. Work that someone waits for, such as the listing that the
/// user opened and the EXIF index of a search, goes through `before_a_look`, where a long run of
/// looks shares one budget: the looks wait for the images awaited for at most
/// `WALK_WAITS_FOR_IMAGES_AT_MOST` in every `LOOKS_PER_PAUSE` looks, so a run of looks always
/// yields to the window, and never stops for good. The listing's own looks at its files (the
/// walk's parts, the list put in another order, the sort of the whole list) go through
/// `sort_key`, which makes such a look. Work that nobody waits for, such as the folder watcher,
/// a thumbnail, an image read ahead or the list's sizes and dates, goes through
/// `before_work_nobody_waits_for`. It waits as long as the window waits for another image,
/// which is at most `IMAGE_AWAITED_FOR` for each image, so it runs between the image's decodes
/// and never stops for good. Before, only the walk of a tree waited, and the folder watcher,
/// the thumbnails, the images read ahead and the other readers held the slideshow's next image
/// for many seconds.
pub(crate) struct ReadsFirst {
    /// Set when another listing starts, which ends the wait.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The images that the window waits for.
    awaited: ImagesAwaited,
    /// Whether the places read are on a share (`is_on_a_share`), or `None` when each path is
    /// asked about on its own (`for_files_anywhere`).
    on_a_share: Option<bool>,
    /// The folder last asked about, and whether it is on a share.
    folder_asked: Option<(PathBuf, bool)>,
    /// How many looks were made, which renews the wait's budget every `LOOKS_PER_PAUSE`.
    looks: u64,
    /// How long the looks since the budget was last renewed have waited.
    waited: std::time::Duration,
}

impl ReadsFirst {
    /// Returns the gate for a listing whose places are `on_a_share`, which `stop` ends and
    /// which waits for the images `awaited`.
    pub(crate) fn new(
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        awaited: ImagesAwaited,
        on_a_share: bool,
    ) -> Self {
        ReadsFirst {
            stop,
            awaited,
            on_a_share: Some(on_a_share),
            folder_asked: None,
            looks: 0,
            waited: std::time::Duration::ZERO,
        }
    }

    /// Returns the gate for a reader of files in any folder, which asks for each path whether it
    /// is on a share, and waits for the images `awaited`.
    pub(crate) fn for_files_anywhere(awaited: &ImagesAwaited) -> Self {
        ReadsFirst {
            on_a_share: None,
            ..ReadsFirst::new(Default::default(), awaited.clone(), false)
        }
    }

    /// Looks at the file `name` in `dir` for what it is sorted by in `by` (`sort_key`), after
    /// the wait of a look (`before_a_look`).
    pub(crate) fn sort_key(&mut self, dir: &Path, name: &OsStr, by: Sort) -> SortKey {
        self.before_a_look(dir);
        sort_key(dir, name, by)
    }

    /// Waits, when `path` is on a share, for the reads that go first, with what is left of the
    /// budget of the last `LOOKS_PER_PAUSE` looks, which is renewed before every
    /// `LOOKS_PER_PAUSE`th look.
    pub(crate) fn before_a_look(&mut self, path: &Path) {
        if !self.is_on_a_share(path) {
            return;
        }
        if self.looks.is_multiple_of(LOOKS_PER_PAUSE) {
            self.waited = std::time::Duration::ZERO;
        }
        self.looks += 1;
        let at_most = WALK_WAITS_FOR_IMAGES_AT_MOST.saturating_sub(self.waited);
        let began = std::time::Instant::now();
        wait_for_reads_that_go_first(&self.stop, &self.awaited, None, at_most);
        self.waited += began.elapsed();
    }

    /// Waits, before a look at or read of `path` that nobody waits for, when `path` is on a
    /// share and is not itself an image that the window waits for, for the reads that go
    /// first, and then for the turn of such work (`BackgroundTurn`), for at most
    /// `IMAGE_AWAITED_FOR` in all. Returns the turn, which the work holds until it is done.
    /// Before, the images read ahead began together as soon as no image was awaited, and the
    /// next one then waited behind the others.
    pub(crate) fn before_work_nobody_waits_for(&mut self, path: &Path) -> BackgroundTurn {
        use std::sync::atomic::Ordering;
        let no_turn = BackgroundTurn { holds: false };
        if !self.is_on_a_share(path) || self.awaited.awaits(path) {
            return no_turn;
        }
        let began = std::time::Instant::now();
        wait_for_reads_that_go_first(&self.stop, &self.awaited, Some(path), IMAGE_AWAITED_FOR);
        loop {
            let taken = BACKGROUND_TURN_TAKEN.compare_exchange(
                false,
                true,
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
            if taken.is_ok() {
                return BackgroundTurn { holds: true };
            }
            // A turn held by a read on a share that stopped answering holds the others for at
            // most this long, and the image awaited never waits for it.
            if began.elapsed() >= IMAGE_AWAITED_FOR
                || self.awaited.awaits(path)
                || self.stop.load(Ordering::Relaxed)
            {
                return no_turn;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Returns whether `path` is on a share: as the gate was made, or for a reader of files
    /// anywhere as the mount table says, asked once for each folder in a row.
    fn is_on_a_share(&mut self, path: &Path) -> bool {
        if let Some(on_a_share) = self.on_a_share {
            return on_a_share;
        }
        let folder = path.parent().unwrap_or(path);
        match &self.folder_asked {
            Some((asked, on_a_share)) if asked == folder => *on_a_share,
            _ => {
                let on_a_share = is_on_a_share(path, false);
                self.folder_asked = Some((folder.to_path_buf(), on_a_share));
                on_a_share
            }
        }
    }
}

/// The turn of a look or read on a share that nobody waits for
/// (`ReadsFirst::before_work_nobody_waits_for`), held until the work is done. One that does not
/// hold it, elsewhere than on a share or for an image awaited, gives nothing back.
#[must_use = "the work holds the turn until it is done"]
pub(crate) struct BackgroundTurn {
    /// Whether it holds the turn, which it gives back when it is dropped.
    holds: bool,
}

impl Drop for BackgroundTurn {
    fn drop(&mut self) {
        if self.holds {
            BACKGROUND_TURN_TAKEN.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

/// A JPEG by its name: the one test, for the scaled decode, Check Images, duplicates, the
/// orientation field and the EXIF read.
pub fn is_jpeg(path: &Path) -> bool {
    has_ext(path, &["jpg", "jpeg"])
}

pub fn has_ext(path: &Path, exts: &[&str]) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

/// A new file name from `pattern`: * is the old name without extension, a run of # the number
/// `n` with that many digits; the old extension is kept. As bytes: an old name that is not
/// UTF-8 (Latin-1 from an old camera) keeps its letters.
pub fn pattern_name(pattern: &str, old: &Path, n: usize) -> OsString {
    let (mut out, mut chars) = (OsString::new(), pattern.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push(old.file_stem().unwrap_or_default()),
            '#' => {
                let mut width = 1;
                while chars.next_if_eq(&'#').is_some() {
                    width += 1;
                }
                out.push(format!("{n:0width$}"));
            }
            c => out.push(c.encode_utf8(&mut [0; 4])),
        }
    }
    if let Some(ext) = old.extension() {
        out.push(".");
        out.push(ext);
    }
    out
}

/// Shown in the file list: an image, a RAW file or a video (video feature), of the `types` on.
pub fn is_listed(path: &Path, types: Types) -> bool {
    (types.hidden || !is_hidden(path)) && is_listed_type(path, types)
}

/// A name starting with a dot, which Geeqie and file managers hide.
pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))
}

/// Returns why `name` cannot be the new name of a file, a folder or a collection, or `None` when
/// it can. It is refused when it is empty, `.`, `..` or has a `/`, and when it starts with a dot
/// unless `hidden_shown`, because the list hides such a name and the file would vanish from it.
/// A theme, which is not in the list, is named with `hidden_shown`.
pub fn refused_name(name: &str, hidden_shown: bool) -> Option<String> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Some(crate::tf!("Not a valid name: \"{text}\"", text = name));
    }
    (name.starts_with('.') && !hidden_shown).then(|| {
        crate::tf!(
            "{text} starts with a dot and would be hidden: choose another name",
            text = name
        )
    })
}

fn is_listed_type(path: &Path, types: Types) -> bool {
    (types.images
        && (built_in_image(path) || crate::images::added_formats::for_path(path).is_some()))
        || (types.raw && has_ext(path, raw::EXTS))
        || (types.videos && is_video(path))
}

/// The image formats Glimt reads itself, as built.
fn built_in_image(path: &Path) -> bool {
    has_ext(path, EXTS) || is_heif(path) || has_ext(path, JXL_EXTS) || has_ext(path, SVG_EXTS)
}

/// Whether Glimt reads the format of `path` itself, as built: an added format is not asked.
pub fn built_in_format(path: &Path) -> bool {
    built_in_image(path) || has_ext(path, raw::EXTS) || is_video(path)
}

/// `dir` and every folder below it, one at a time, as a `WalkedFolder`. A folder reached again
/// through a link or a bind mount is read once. A link to a folder is followed only when the
/// folder it leads to is inside `dir`'s real path. A downloaded folder with `up -> ../..`,
/// `pics -> ~/Pictures` or `root -> /` would else list the user's own photos or the whole disk,
/// for Select All and Delete to act on.
///
/// It gives an error for a folder that it could not read. After an error for `dir` itself it
/// stops, and after one for a folder below `dir` it goes on with the rest of the tree, so a
/// damaged or locked subfolder costs only its own images. A folder below `dir` that is gone is
/// walked as empty.
pub fn walk(dir: &Path, types: Types) -> impl Iterator<Item = Result<WalkedFolder, Unread>> {
    walk_by(
        dir,
        types,
        crate::images::save::file_id,
        |p| std::fs::canonicalize(p),
        list_folder,
    )
}

/// Walks the tree as `walk` does, with `file_id` telling the folders apart, `canonical`
/// giving a link's real path and `list` reading a folder. A test can then give the ids that a
/// file system without unique inodes has, a real path that cannot be worked out, and a file
/// made while a folder is read.
pub(crate) fn walk_by<Id, Real, List>(
    dir: &Path,
    types: Types,
    file_id: Id,
    canonical: Real,
    list: List,
) -> impl Iterator<Item = Result<WalkedFolder, Unread>> + use<Id, Real, List>
where
    Id: Fn(&Path) -> Option<FileId>,
    Real: Fn(&Path) -> std::io::Result<PathBuf>,
    List: Fn(&Path, Types) -> std::io::Result<(Vec<PathBuf>, Vec<ListedName>)>,
{
    walk_by_from((dir, Path::new("")), types, file_id, canonical, list)
}

/// Walks the tree at `dir` as `walk_by` does, but only the folder `from` in it, a path relative
/// to `dir`, and the folders below it. Its folders and images are named relative to `dir`, as
/// in a walk of the whole tree, so a subfolder read again replaces its own names in the list. An
/// error for `from` stops it, as one for `dir` stops the walk of the whole tree.
pub(crate) fn walk_by_from<Id, Real, List>(
    (dir, from): (&Path, &Path),
    types: Types,
    file_id: Id,
    canonical: Real,
    list: List,
) -> impl Iterator<Item = Result<WalkedFolder, Unread>> + use<Id, Real, List>
where
    Id: Fn(&Path) -> Option<FileId>,
    Real: Fn(&Path) -> std::io::Result<PathBuf>,
    List: Fn(&Path, Types) -> std::io::Result<(Vec<PathBuf>, Vec<ListedName>)>,
{
    let start = from.to_path_buf();
    let (dir, mut todo) = (dir.to_path_buf(), vec![start.clone()]);
    let mut failed = false;
    let mut seen: HashSet<FileId> = HashSet::new();
    let folder_id = move |full: &Path| file_id(full).unwrap_or_else(|| Err(real_dir(full)));
    let real = real_dir(&dir);
    std::iter::from_fn(move || {
        loop {
            if failed {
                return None;
            }
            let mut rel = todo.pop()?;
            let full = dir.join(&rel);
            // The folder the walk starts from, the top of the tree or a subfolder of it.
            let is_top = rel == start;
            // A followed link's folder is named by its real path in the tree, so every file has
            // one name, the one `--remote select`'s client gives and marks and stars are kept
            // by. Named through the link, the file was another name than its own. A folder that
            // could not be looked at may be a link, so its real path is worked out too.
            // The top folder whose real path cannot be worked out is not read, and that is
            // told, as when it cannot be read.
            if crate::files::file_io::is_link(&full).unwrap_or(true) {
                match canonical(&full) {
                    Err(e) if is_top => {
                        failed = true;
                        return Some(Err((full, e)));
                    }
                    real_path => match inside(real_path, &real) {
                        Some(real_rel) => rel = real_rel,
                        None => continue,
                    },
                }
            }
            if !seen.insert(folder_id(&full)) {
                continue;
            }
            // The time is taken before the folder is read, so the folder watcher sees a file
            // made while it is read, which the read itself may have missed.
            let modified = std::fs::metadata(&full).and_then(|m| m.modified()).ok();
            let (dirs, files) = match list(&full, types) {
                Ok(listed) => listed,
                Err(e) if !is_top && e.kind() == std::io::ErrorKind::NotFound => Default::default(),
                // The folder is named by its path in the tree, as its files would be named, so
                // the window can tell whether the list shown holds files of it.
                Err(e) => {
                    failed = is_top;
                    let at = if is_top { full } else { dir.join(&rel) };
                    return Some(Err((at, e)));
                }
            };
            // Last in, first out: pushed backwards, the folders are read in name order.
            todo.extend(
                dirs.iter()
                    .rev()
                    .filter_map(|d| d.file_name())
                    .map(|n| rel.join(n)),
            );
            return Some(Ok((rel, dirs, files, modified)));
        }
    })
}

/// Notes that a program was just started on a file, so that the reading of a share that nobody
/// waits for waits a moment for it (`wait_for_reads_that_go_first`). A program of the user's is
/// started through `external::start_users_program`, which calls it.
pub(crate) fn program_started() {
    #[cfg(test)]
    PROGRAMS_STARTED.set(PROGRAMS_STARTED.get() + 1);
    *crate::support::threads::lock_even_if_poisoned(&PROGRAM_STARTED_AT) =
        Some(std::time::Instant::now());
}

/// Returns whether a program was started on a file in the last `PROGRAM_STARTS_FOR`, as the
/// time `started` that `program_started` noted says at `now`.
fn is_starting(started: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    started.is_some_and(|at| now.saturating_duration_since(at) < PROGRAM_STARTS_FOR)
}

/// Returns whether a program started on a file may still be reading its start
/// (`program_started`).
pub(crate) fn a_program_is_starting() -> bool {
    let started = *crate::support::threads::lock_even_if_poisoned(&PROGRAM_STARTED_AT);
    is_starting(started, std::time::Instant::now())
}

/// Waits while a program started on a file may still be reading its start, or while the window
/// waits for an image of `awaited` other than `except` and for at most `at_most`, or until
/// `stop` is set. The reading of a tree on a share calls it between folders, with
/// `WALK_WAITS_FOR_IMAGES_AT_MOST`, and every other look or read in the background on a share
/// through the gate (`ReadsFirst`), so that neither a video player started on the same share
/// nor the next image is queued behind them.
fn wait_for_reads_that_go_first(
    stop: &std::sync::atomic::AtomicBool,
    awaited: &ImagesAwaited,
    except: Option<&Path>,
    at_most: std::time::Duration,
) {
    let began = std::time::Instant::now();
    let awaited = || {
        let now = std::time::Instant::now();
        now.duration_since(began) < at_most && awaited.holds_the_walk_at(now, except)
    };
    while (a_program_is_starting() || awaited()) && !stop.load(std::sync::atomic::Ordering::Relaxed)
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Returns whether `dir` is on a share (`file_io::is_network`), where each look at a file is a
/// round trip over the network. With `with_subfolders`, it is also whether a share is mounted
/// in a folder below `dir`, such as a NAS at ~/Pictures/nas, as a walk of the tree goes into
/// it. That is one read of the mount table, not a look at each folder. A mount whose type
/// cannot be read counts as none.
// shortcut: a share mounted in a hidden folder, which the walk leaves out, counts too; look at
// the folders the walk reads if that leaves a tree's sizes uncounted for nothing.
pub(crate) fn is_on_a_share(dir: &Path, with_subfolders: bool) -> bool {
    use crate::files::{file_io::is_network, file_ops};
    file_ops::file_system_type_or_test(dir).is_some_and(|kind| is_network(&kind))
        || (with_subfolders
            && (file_ops::mounted_types_below_or_test(dir).iter()).any(|kind| is_network(kind)))
}

/// Returns the path relative to the tree whose real path is `top` of a link's folder whose real
/// path is `real`, which the walk follows. Returns `None` when the walk does not follow it: the
/// folder is outside the tree, or its real path cannot be worked out. Made absolute instead, as
/// `real_dir` does, it would always lie inside the tree, and a chain of short links to a folder
/// whose real path is longer than realpath takes would lead anywhere.
fn inside(real: std::io::Result<PathBuf>, top: &Path) -> Option<PathBuf> {
    Some(real.ok()?.strip_prefix(top).ok()?.to_path_buf())
}

/// The folders in `dir`, as `list_folder` gives them, and the images in it and every folder
/// below it, as paths relative to `dir`, in name order: `dir` read once. `top` gets `dir`'s own
/// folders as soon as they are read, and `part` each folder read whole, with the images found so
/// far, to show them as the tree is read: a large tree takes long to read whole. Cut short
/// when `stop` is set: another folder was opened, and the tree would be read for nothing. On a
/// share it waits between folders while the window waits for an image of `awaited`.
/// Returns, third, every folder of the tree that was read, `dir` first, for the folder watcher,
/// and fourth, the folders below `dir` that could not be read, which are left out. Returns an
/// error when `dir` itself could not be read. The listings read a tree with `walk_tree`, and
/// the tests with this.
#[cfg(test)]
pub fn list_tree(
    dir: &Path,
    types: Types,
    stop: &std::sync::atomic::AtomicBool,
    awaited: &ImagesAwaited,
    top: impl FnOnce(&[PathBuf]),
    part: impl FnMut(&FolderReadWhole),
) -> Result<ListedTree, Unread> {
    let whole = Path::new("");
    walk_tree((dir, whole), types, stop, awaited, top, part).map(
        |(own, mut out, read, left_out)| {
            // In name order, as list_folder gives a single folder; sort_by_names builds on that.
            out.sort_unstable();
            (own, out, read, left_out)
        },
    )
}

/// Reads the tree at `dir` as `list_tree` does, but gives its images in the order the walk
/// found them, which is the order of the images that `part` gets. The listing of a tree keeps
/// what it looked at of each image by its place in that order, for the sort of the whole list.
/// With `from` other than empty, it reads only that subfolder of the tree and the folders below
/// it (`walk_by_from`), with their names relative to `dir`, and `top` is not called.
pub fn walk_tree(
    (dir, from): (&Path, &Path),
    types: Types,
    stop: &std::sync::atomic::AtomicBool,
    awaited: &ImagesAwaited,
    top: impl FnOnce(&[PathBuf]),
    mut part: impl FnMut(&FolderReadWhole),
) -> Result<ListedTree, Unread> {
    use std::sync::atomic::Ordering::Relaxed;
    let (mut own, mut top, mut out) = (Vec::new(), Some(top), Vec::new());
    let (mut read, mut left_out) = (Vec::new(), Vec::new());
    let on_a_share = is_on_a_share(dir, true);
    // Each look at an entry of a folder goes through the gate on a share, as the looks at the
    // files of a part do.
    let gate = std::cell::RefCell::new(ReadsFirst::new(
        Default::default(),
        awaited.clone(),
        on_a_share,
    ));
    let list = |folder: &Path, types: Types| {
        list_folder_after(folder, types, &mut || {
            gate.borrow_mut().before_a_look(folder)
        })
    };
    let canonical = |p: &Path| std::fs::canonicalize(p);
    let walked = walk_by_from(
        (dir, from),
        types,
        crate::images::save::file_id,
        canonical,
        list,
    );
    for walked in walked.take_while(|_| !stop.load(Relaxed)) {
        let (rel, dirs, files, modified) = match walked {
            Ok(walked) => walked,
            // The walk stops after an error for `dir`, so that one is the only error that
            // comes before `dir` itself was read.
            Err(unread) if read.is_empty() => return Err(unread),
            // The folder stays watched, so a change in it lists the tree again. Its time is
            // taken as it is now, as a time unknown would be a change every second.
            Err(unread) => {
                let modified = std::fs::metadata(&unread.0).and_then(|m| m.modified());
                read.push((unread.0.clone(), modified.ok()));
                left_out.push(unread);
                continue;
            }
        };
        read.push((dir.join(&rel), modified));
        let is_top = rel.as_os_str().is_empty();
        if is_top && let Some(top) = top.take() {
            top(&dirs);
        }
        let first = out.len();
        out.extend((files.into_iter()).map(|(f, id)| (rel.join(f).into_os_string(), id)));
        // The wait comes before the folder's part. In an order that looks at the files, the
        // part looks at each new image on the share, and each of those looks waits again
        // (`ReadsFirst`), for at most a second in every hundred looks.
        if on_a_share {
            wait_for_reads_that_go_first(stop, awaited, None, WALK_WAITS_FOR_IMAGES_AT_MOST);
        }
        part(&FolderReadWhole {
            so_far: &out,
            first,
            folder: &rel,
            dirs: &dirs,
            types,
        });
        if is_top {
            own = dirs;
        }
    }
    Ok((own, out, read, left_out))
}

/// Returns `dir` in the form Glimt keeps and compares folders by: its real path, with its links
/// followed and its `..` worked out, as other programs (the trash, `--remote select`'s client)
/// record it too. A folder that is not there (renamed, or a link to it renamed) gets the real
/// path of the deepest folder above it that is, with the rest as written.
// It waits for the disk, and on a share that stopped answering it waits for ever, so it is
// called only from background threads. The window-thread check in source_rules refuses it there.
pub fn real_dir(dir: &Path) -> PathBuf {
    let Ok(dir) = std::path::absolute(dir) else {
        return dir.to_path_buf();
    };
    // Made absolute only, a folder renamed under a link (/home on /var/home) kept the link's
    // name, and the rename's old path, its real one, was another folder to a collection's entry.
    for above in dir.ancestors() {
        if let Ok(real) = std::fs::canonicalize(above) {
            let rest = dir.strip_prefix(above).unwrap_or(Path::new(""));
            return match rest.as_os_str().is_empty() {
                true => real,
                false => real.join(rest),
            };
        }
    }
    dir
}

/// Returns `path` in the form Glimt keeps and compares files by: its folder's real path
/// (`real_dir`) with its own name. The file's own link is not followed, because the list shows
/// a link under its own name.
pub fn real_path(path: &Path) -> PathBuf {
    let name = path.file_name();
    match (path.parent().filter(|p| !p.as_os_str().is_empty()), name) {
        (parent, Some(name)) => real_dir(parent.unwrap_or(Path::new("."))).join(name),
        (_, None) => real_dir(path),
    }
}

/// Returns whether a listing found a link to a file in `folder`. Its thumbnails are then never
/// set aside (`spawn_worker_pool`), because the folder's name does not tell where a link leads.
pub fn holds_links(folder: &Path) -> bool {
    lock_even_if_poisoned(&FOLDERS_WITH_LINKS).contains(folder)
}

/// Whether a folder entry is a file and whether a folder, from the entry's own type (no `stat`),
/// a link as what it points to. Only files are images: a folder named 2019.jpg is a folder, and
/// a pipe or a link to /dev/zero named a.jpg would block or fill memory when read.
pub fn kind(entry: &std::fs::DirEntry) -> (bool, bool) {
    match entry.file_type() {
        // A link that cannot be followed is neither, so it is not read as an image or folder.
        Ok(kind) if kind.is_symlink() => {
            let path = entry.path();
            let is_file = crate::files::file_io::is_surely_file(&path);
            if is_file && let Some(folder) = path.parent() {
                lock_even_if_poisoned(&FOLDERS_WITH_LINKS).insert(folder.to_owned());
            }
            (
                is_file,
                !is_file && crate::files::file_io::is_surely_dir(&path),
            )
        }
        Ok(kind) => (kind.is_file(), kind.is_dir()),
        Err(_) => (false, false),
    }
}

/// One pass over the folder: (subfolders, image file names with their identities), both sorted
/// by name. The file type and the inode come from the directory entry, so there is no `stat`
/// per file (only symlinks are resolved). A folder that is gone has nothing in it.
///
/// Returns an error when the folder could not be opened or its reading failed partway. Taken
/// as empty, or as the part read, a folder on a share that failed for a moment emptied the
/// list, and the selection with it.
pub fn list_folder(dir: &Path, types: Types) -> std::io::Result<(Vec<PathBuf>, Vec<ListedName>)> {
    list_folder_after(dir, types, &mut || {})
}

/// Reads `dir` as `list_folder` does, and calls `before_each` before the look at each entry. A
/// share that gives no entry's type with its names, as gvfsd-fuse does, has each entry looked at
/// on its own, and the walk of a tree on a share passes each of those looks through the gate
/// (`ReadsFirst::before_a_look`). Before, a folder of hundreds of entries held the share for
/// that many calls in a row, and the image awaited waited behind them.
fn list_folder_after(
    dir: &Path,
    types: Types,
    before_each: &mut dyn FnMut(),
) -> std::io::Result<(Vec<PathBuf>, Vec<ListedName>)> {
    use crate::images::save::identity_of_entry;
    let (mut dirs, mut images) = (Vec::new(), Vec::new());
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((dirs, images)),
        entries => entries?,
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        before_each();
        let (file, folder) = kind(&entry);
        if file && is_listed(&path, types) {
            images.push((entry.file_name(), identity_of_entry(&entry)));
        } else if folder && lists_folder_named(entry.file_name().as_encoded_bytes(), types) {
            dirs.push(path);
        }
    }
    // Byte-wise comparison of the whole path gives name order for paths in one folder, and is
    // ~9× faster than Path::cmp, which re-parses the path in every comparison.
    dirs.sort_unstable_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    images.sort_unstable(); // byte-wise on Unix
    Ok((dirs, images))
}

/// Returns whether a folder read with `types` lists a folder in it named `name`: a hidden one
/// only while hidden files are shown. The walk of a tree reads only the folders listed, and
/// those it reaches through a link.
pub fn lists_folder_named(name: &[u8], types: Types) -> bool {
    types.hidden || !name.starts_with(b".")
}

/// Sorts `files` in an order by the names alone, `by`, with `case` for case-sensitive names
/// ("B" before "a"), and ties back to the name. An order that looks at the files leaves them as
/// they are, which `Rows::sorted` sorts by their keys instead.
fn sort_by_names(files: &mut [ListedName], by: Sort, ascending: bool, case: bool) {
    let name = |(f, _): &ListedName| {
        let s = f.to_string_lossy();
        if case {
            s.into_owned()
        } else {
            s.to_lowercase()
        }
    };
    match by {
        // Files are listed byte-wise, which is case-sensitive name order.
        Sort::Name if case => {}
        Sort::Name => files.sort_by_cached_key(name),
        Sort::Number => {
            // Each name made once, not twice in each of millions of comparisons.
            let mut keyed: Vec<(String, ListedName)> = (files.iter_mut())
                .map(|f| (name(f), std::mem::take(f)))
                .collect();
            // Equal numbers ("img010", "img10") in name order.
            keyed.sort_by(|a, b| natural_cmp(&a.0, &b.0).then_with(|| a.0.cmp(&b.0)));
            for (f, (_, k)) in files.iter_mut().zip(keyed) {
                *f = k;
            }
        }
        Sort::Extension => files.sort_by_cached_key(|f| {
            let n = name(f);
            let ext = n.rsplit_once('.').map_or("", |e| e.1).to_owned();
            (ext, n)
        }),
        // These look at the files, and `Rows::sorted` sorts by them above.
        Sort::Date | Sort::Created | Sort::Size | Sort::ExifOriginal | Sort::ExifDigitized => {}
    }
    if !ascending {
        files.reverse();
    }
}

/// `before` in its order without the names gone, if `listed` has no name it lacks: a folder read
/// again after a sidecar was written keeps its EXIF date order instead of reading every file.
/// shortcut: a file whose date changed under the same name keeps its place until Refresh (F5),
/// the Sort menu or opening the folder again sorts it anew.
pub fn kept_order(before: Vec<OsString>, listed: &[ListedName]) -> Option<Vec<ListedName>> {
    let identities: std::collections::HashMap<&OsString, Identity> =
        listed.iter().map(|(f, id)| (f, *id)).collect();
    let kept: Vec<ListedName> = (before.into_iter())
        .filter_map(|f| Some((identities.get(&f).copied()?, f)))
        .map(|(id, f)| (f, id))
        .collect();
    (kept.len() == listed.len()).then_some(kept)
}

/// Compare with runs of digits by value, so "img2" comes before "img10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        let (Some(ca), Some(cb)) = (a.chars().next(), b.chars().next()) else {
            return a.len().cmp(&b.len());
        };
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let end_a = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
            let end_b = b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len());
            let (na, nb) = (
                a[..end_a].trim_start_matches('0'),
                b[..end_b].trim_start_matches('0'),
            );
            let ord = na.len().cmp(&nb.len()).then(na.cmp(nb));
            if ord.is_ne() {
                return ord;
            }
            (a, b) = (&a[end_a..], &b[end_b..]);
        } else {
            if ca != cb {
                return ca.cmp(&cb);
            }
            (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
        }
    }
}

/// Returns what `file`, a name in `dir`, is sorted by besides its name in the order `by`: a look
/// at the file in an order that looks at the files, which only a thread may make, and
/// `SortKey::Name` otherwise.
pub fn sort_key(dir: &Path, file: &OsStr, by: Sort) -> SortKey {
    #[cfg(test)]
    if by.looks_at_files() {
        FILES_LOOKED_AT.set(FILES_LOOKED_AT.get() + 1);
    }
    let meta = || std::fs::metadata(dir.join(file)).ok();
    let nanos = |t: std::io::Result<std::time::SystemTime>| {
        let since = t
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        since.map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    };
    match by {
        Sort::Name | Sort::Number | Sort::Extension => SortKey::Name,
        Sort::Date => SortKey::Time(meta().map(|m| nanos(m.modified()))),
        // Not every file system records creation time; then the modification time.
        Sort::Created => {
            SortKey::Time(meta().map(|m| nanos(m.created().or_else(|_| m.modified()))))
        }
        Sort::Size => SortKey::Size(meta().map(|m| m.len())),
        Sort::ExifOriginal | Sort::ExifDigitized => {
            let date = exif::date(&dir.join(file), by == Sort::ExifDigitized);
            SortKey::Exif(date.is_none(), date)
        }
    }
}

/// Compares two files, each with its `sort_key` and its name, in the order `(by, ascending,
/// case)`, as `Rows::sorted` puts them: by their keys, then by their names as the order takes
/// them, and then byte by byte, which is the order in which `Rows::sorted` leaves names that the
/// order holds equal. Descending is the other way round.
pub fn in_order(
    a: (&SortKey, &OsString),
    b: (&SortKey, &OsString),
    (by, ascending, case): (Sort, bool, bool),
) -> std::cmp::Ordering {
    #[cfg(test)]
    ROWS_COMPARED.set(ROWS_COMPARED.get() + 1);
    let bytes = || a.1.as_encoded_bytes().cmp(b.1.as_encoded_bytes());
    let name = |f: &OsString| {
        let s = f.to_string_lossy();
        match case {
            true => s.into_owned(),
            false => s.to_lowercase(),
        }
    };
    let by_name = || {
        let (x, y) = (name(a.1), name(b.1));
        match by {
            Sort::Number => natural_cmp(&x, &y).then_with(|| x.cmp(&y)),
            Sort::Extension => {
                let ext = |n: &str| n.rsplit_once('.').map_or("", |e| e.1).to_owned();
                ext(&x).cmp(&ext(&y)).then_with(|| x.cmp(&y))
            }
            _ => x.cmp(&y),
        }
    };
    let order = match (by, case) {
        // Files are listed byte-wise, which is case-sensitive name order.
        (Sort::Name, true) => bytes(),
        _ => (a.0.cmp(b.0)).then_with(by_name).then_with(bytes),
    };
    match ascending {
        true => order,
        false => order.reverse(),
    }
}

/// Returns a 64-bit hash of a name's bytes, the same in every thread of this Glimt, as the
/// listing thread looks up the hashes that the window made (`NameHashes`).
pub fn name_hash(name: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(name);
    hasher.finish()
}

/// Returns the folder of `name`, a path relative to a tree, as bytes: what comes before its
/// last "/", or nothing for a name at the top.
fn folder_of(name: &[u8]) -> &[u8] {
    let last = name.iter().rposition(|&b| b == b'/');
    last.map_or(&name[..0], |at| &name[..at])
}

#[cfg(test)]
mod tests {
    /// A part of new names leaves out of the list the names that a folder it read whole no
    /// longer holds: an image moved away, and every image below a subfolder renamed, also
    /// among the images played, which keep their order. A folder read that holds every name of
    /// the list is not noted, so its part costs the window no look at the list. Before, such
    /// names stayed until the whole list came, which on a share is minutes away, and the
    /// slideshow showed a renamed folder's images twice and tried the old names.
    #[test]
    fn a_part_leaves_out_what_its_folders_no_longer_hold() {
        let names = |v: &[&str]| -> Vec<std::ffi::OsString> { v.iter().map(Into::into).collect() };
        let list = names(&["t.png", "a/1.png", "a/2.png", "b/c/3.png", "d/4.png"]);
        let hashes = super::NameHashes::of(&list).sorted();
        let found = |v: &[&str]| -> Vec<super::ListedName> {
            v.iter().map(|n| ((*n).into(), None)).collect()
        };
        let mut read = super::ReadWhole::default();
        // The top holds t.png and the folders a and d, and b is renamed to e.
        let top = found(&["t.png"]);
        let dirs = [
            std::path::PathBuf::from("/x/a"),
            "/x/d".into(),
            "/x/e".into(),
        ];
        let folder = |so_far, folder, dirs| super::FolderReadWhole {
            so_far,
            first: 0,
            folder,
            dirs,
            types: super::Types::NONE,
        };
        hashes.note_folder(&folder(&top, std::path::Path::new(""), &dirs), &mut read);
        // a holds 1.png only, and d all it had.
        let a = found(&["a/1.png"]);
        hashes.note_folder(&folder(&a, std::path::Path::new("a"), &[]), &mut read);
        let d = found(&["d/4.png"]);
        hashes.note_folder(&folder(&d, std::path::Path::new("d"), &[]), &mut read);
        assert!(!read.folders.contains(b"d".as_slice()), "d lost nothing");
        let gone: Vec<&std::ffi::OsString> = list.iter().filter(|n| read.leaves_out(n)).collect();
        assert_eq!(gone, [&list[2], &list[3]]);
        let names = ["a/1.png", "a/2.png", "b/c/3.png", "d/4.png", "t.png"];
        let by_name = (super::Sort::Name, true, true);
        let old = rows_of(found(&names), &[], by_name);
        let new = rows_of(found(&["e/c/3.png"]), &[], by_name);
        let (merged, _) = old.merged(new, &read).unwrap();
        let want = found(&["a/1.png", "d/4.png", "e/c/3.png", "t.png"]);
        assert_eq!(merged.listed_names(), want);
        // In an order by date, the keys of the names left out go with them.
        let time = |t| super::SortKey::Time(Some(t));
        let by_date = (super::Sort::Date, true, true);
        let old = rows_of(found(&names), &[1, 2, 3, 4, 5].map(time), by_date);
        let new = rows_of(found(&["e/c/3.png"]), &[time(4)], by_date);
        let (merged, _) = old.merged(new, &read).unwrap();
        assert_eq!(merged.listed_names(), want);
        assert_eq!(keys_of(&merged), [1, 4, 4, 5].map(time));
    }

    /// A part keeps the names of a hidden folder that the walk reached through a link while
    /// hidden files are not shown, also when the top folder that holds it is read whole and
    /// noted for a name it lost: the top's read leaves the hidden folder out by rule, so it is
    /// not gone. Its own read, through the link, finds every name it holds, and with hidden files
    /// shown a hidden folder missing from the top is gone. Before, the top's read dropped its
    /// names, and the walk filtered them out again as listed, so they came back only with the
    /// whole list, minutes later on a share, and an image played was shown a second time.
    #[test]
    fn a_hidden_folder_reached_through_a_link_keeps_its_names() {
        let list: Vec<std::ffi::OsString> = ["a.png", ".inner/c.png", "b/1.png", "sub/2.png"]
            .map(Into::into)
            .to_vec();
        let hashes = super::NameHashes::of(&list).sorted();
        let found = |v: &[&str]| -> Vec<super::ListedName> {
            v.iter().map(|n| ((*n).into(), None)).collect()
        };
        // The top holds a.png and sub, which holds the link in -> ../.inner, and b is gone.
        let top = found(&["a.png"]);
        let dirs = [std::path::PathBuf::from("/x/sub")];
        for hidden in [false, true] {
            let types = super::Types {
                hidden,
                ..super::Types::NONE
            };
            let mut read = super::ReadWhole::default();
            let folder = super::FolderReadWhole {
                so_far: &top,
                first: 0,
                folder: std::path::Path::new(""),
                dirs: &dirs,
                types,
            };
            hashes.note_folder(&folder, &mut read);
            let inner = found(&["a.png", ".inner/c.png"]);
            let through_link = super::FolderReadWhole {
                so_far: &inner,
                first: 1,
                folder: std::path::Path::new(".inner"),
                dirs: &[],
                types,
            };
            hashes.note_folder(&through_link, &mut read);
            assert!(
                !read.folders.contains(b".inner".as_slice()),
                "it lost nothing"
            );
            let gone: Vec<&std::ffi::OsString> =
                list.iter().filter(|n| read.leaves_out(n)).collect();
            match hidden {
                false => assert_eq!(gone, [&list[2]], "hidden off"),
                true => assert_eq!(gone, [&list[1], &list[2]], "hidden on"),
            }
        }
    }

    /// The names that a listing of new names only leaves out are handed to its thread as 8
    /// bytes each, with no copy of the names, and every one of them is found. Before, the list
    /// was copied and a set built over the copy, about 37 MB at 500,000 names, and 30 ms in the
    /// window's thread instead of 10.
    #[test]
    fn the_names_listed_before_cost_eight_bytes_each() {
        let names: Vec<std::ffi::OsString> = (0..10_000)
            .map(|i| format!("{:02}/IMG_{i:06}.JPG", i / 1000).into())
            .collect();
        let hashes = super::NameHashes::of(&names).sorted();
        assert_eq!(
            std::mem::size_of_val(hashes.hashes.as_slice()),
            8 * names.len()
        );
        assert!(names.iter().all(|n| hashes.holds(n)));
        assert!(!hashes.holds(std::ffi::OsStr::new("10/IMG_010000.JPG")));
        assert!(super::NameHashes::default().is_empty() && !hashes.is_empty());
    }

    /// A tree on a share is read on only once a program started on a file (Play Video, Open
    /// With) has had a moment to read the start of its file. Before, the player's reads waited
    /// behind a look at each file of the tree. A tree on a disk of its own does not wait.
    #[test]
    fn a_tree_on_a_share_waits_while_a_program_starts() {
        use super::{PROGRAM_STARTS_FOR, is_starting};
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        let now = Instant::now();
        assert!(is_starting(Some(now), now) && !is_starting(None, now));
        assert!(!is_starting(Some(now), now + PROGRAM_STARTS_FOR));
        let dir =
            std::env::temp_dir().join(format!("glimt-test-share-gives-way-{}", std::process::id()));
        for sub in ["a", "b"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("1.jpg"), "x").unwrap();
        }
        let list = || {
            let began = Instant::now();
            let types = super::Types {
                images: true,
                raw: false,
                videos: false,
                hidden: false,
            };
            super::list_tree(
                &dir,
                types,
                &AtomicBool::new(false),
                &Default::default(),
                |_| {},
                |_| {},
            )
            .unwrap();
            began.elapsed()
        };
        // The program was started long enough ago that 400 ms of its time are left.
        let started = Instant::now().checked_sub(PROGRAM_STARTS_FOR - Duration::from_millis(400));
        *crate::support::threads::lock_even_if_poisoned(&super::PROGRAM_STARTED_AT) = started;
        assert!(list() < Duration::from_millis(300), "a disk of its own");
        crate::files::file_ops::test_mount(&dir, Some("fuse.gvfsd-fuse"));
        assert!(list() >= Duration::from_millis(200), "a share");
        crate::files::file_ops::test_mount(&dir, None);
        // A share mounted in a folder of a tree on a disk of its own, such as ~/Pictures/nas.
        // Before, only the tree's top folder was looked at.
        let started = Instant::now().checked_sub(PROGRAM_STARTS_FOR - Duration::from_millis(400));
        *crate::support::threads::lock_even_if_poisoned(&super::PROGRAM_STARTED_AT) = started;
        crate::files::file_ops::test_mount(&dir.join("b"), Some("cifs"));
        assert!(
            list() >= Duration::from_millis(200),
            "a share inside the tree"
        );
        crate::files::file_ops::test_mount(&dir.join("b"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A tree on a share is read on only once the image that the window waits for is decoded,
    /// and an image awaited for `IMAGE_AWAITED_FOR` no longer holds it, so a share that stopped
    /// answering never stops the walk for good. A tree on a disk of its own never waits.
    /// Before, the walk read on, and the next image's reads waited behind its looks at the
    /// share (#256).
    #[test]
    fn a_tree_on_a_share_waits_for_the_image_awaited() {
        use super::{IMAGE_AWAITED_FOR, ImagesAwaited};
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        let awaited = ImagesAwaited::default();
        let now = Instant::now();
        assert!(!awaited.holds_the_walk_at(now, None));
        awaited.set(vec!["a.jpg".into()]);
        assert!(awaited.holds_the_walk_at(Instant::now(), None));
        // An image still awaited keeps the time the window began to wait for it.
        awaited.set(vec!["a.jpg".into(), "b.jpg".into()]);
        assert!(!awaited.holds_the_walk_at(now + IMAGE_AWAITED_FOR + Duration::from_secs(1), None));
        awaited.set(Vec::new());
        assert!(!awaited.holds_the_walk_at(Instant::now(), None));
        let dir = std::env::temp_dir().join(format!(
            "glimt-test-share-awaits-image-{}",
            std::process::id()
        ));
        for sub in ["a", "b"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("1.jpg"), "x").unwrap();
        }
        let list = |awaited: &ImagesAwaited| {
            let began = Instant::now();
            let types = super::Types {
                images: true,
                raw: false,
                videos: false,
                hidden: false,
            };
            let stop = AtomicBool::new(false);
            super::list_tree(&dir, types, &stop, awaited, |_| {}, |_| {}).unwrap();
            began.elapsed()
        };
        awaited.set(vec![dir.join("a/1.jpg")]);
        assert!(
            list(&awaited) < Duration::from_millis(300),
            "a disk of its own"
        );
        crate::files::file_ops::test_mount(&dir, Some("cifs"));
        let decoded = awaited.clone();
        let decoded = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            decoded.set(Vec::new());
        });
        let took = list(&awaited);
        decoded.join().unwrap();
        crate::files::file_ops::test_mount(&dir, None);
        assert!(took >= Duration::from_millis(300), "a share: {took:?}");
        assert!(
            took < Duration::from_secs(3),
            "on once the image is decoded: {took:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The walk of a tree on a share waits for at most a second between two folders, however
    /// many images the window awaits one after the other, so it still reaches the subfolders.
    /// Before, each image held it for up to five seconds, and a slideshow of four panes with a
    /// delay of a second, whose next images were awaited back to back, held it for as long as
    /// it ran.
    #[test]
    fn a_tree_on_a_share_is_read_on_while_images_are_awaited_one_after_another() {
        use super::ImagesAwaited;
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        use std::time::{Duration, Instant};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-share-walks-on-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("1.jpg"), "x").unwrap();
        std::fs::write(dir.join("sub").join("2.jpg"), "x").unwrap();
        crate::files::file_ops::test_mount(&dir, Some("cifs"));
        let (awaited, walked) = (
            ImagesAwaited::default(),
            std::sync::Arc::new(AtomicBool::new(false)),
        );
        // A new image is awaited every 50 ms, for 8 s at most. The first one is awaited before
        // the walk begins, however late the thread starts.
        awaited.set(vec!["first.jpg".into()]);
        let decodes = {
            let (awaited, walked) = (awaited.clone(), walked.clone());
            std::thread::spawn(move || {
                for n in 0..160 {
                    if walked.load(Relaxed) {
                        break;
                    }
                    awaited.set(vec![format!("{n}.jpg").into()]);
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
        };
        let types = super::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let began = Instant::now();
        let (_, files, _, _) = super::list_tree(
            &dir,
            types,
            &AtomicBool::new(false),
            &awaited,
            |_| {},
            |_| {},
        )
        .unwrap();
        let took = began.elapsed();
        walked.store(true, Relaxed);
        decodes.join().unwrap();
        crate::files::file_ops::test_mount(&dir, None);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(files.len(), 2);
        assert!(took >= Duration::from_millis(1500), "it waited: {took:?}");
        assert!(took < Duration::from_secs(4), "it read on: {took:?}");
    }

    /// A local FUSE folder, such as a KDE Vault, a mergerfs pool or the Flatpak's document
    /// portal, is not a share, and keeps the sizes the status bar sums. Before, every FUSE mount
    /// was one, and the status bar called such a folder a network folder.
    #[test]
    fn a_local_fuse_folder_is_not_a_share() {
        use crate::files::file_ops::test_mount;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-local-fuse-{}", std::process::id()));
        for (kind, is_share) in [
            ("fuse.gocryptfs", false),
            ("fuse.cryfs", false),
            ("fuse.mergerfs", false),
            ("fuse.bindfs", false),
            ("fuse.portal", false),
            ("fuseblk", false),
            ("ext4", false),
            ("fuse.sshfs", true),
            ("fuse.rclone", true),
            ("fuse.gvfsd-fuse", true),
            ("fuse.simple-mtpfs", true),
            ("fuse.jmtpfs", true),
            ("fuse.onedriver", true),
            ("fuse.google-drive-ocamlfuse", true),
            ("fuse.goofys", true),
            ("fuse.juicefs", true),
            ("cifs", true),
            ("nfs4", true),
        ] {
            test_mount(&dir, Some(kind));
            assert_eq!(super::is_on_a_share(&dir, false), is_share, "{kind}");
        }
        test_mount(&dir, None);
    }

    /// The images of a tree told after each folder with images, read in name order: the
    /// first told are a's own, before any of b.
    #[test]
    fn tree_told_a_folder_at_a_time() {
        use std::sync::atomic::AtomicBool;
        let dir =
            std::env::temp_dir().join(format!("glimt-test-tree-parts-{}", std::process::id()));
        for (sub, name) in [
            ("a/deep", "1.jpg"),
            ("a", "2.jpg"),
            ("b", "3.jpg"),
            ("c", "4.jpg"),
        ] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join(name), "x").unwrap();
        }
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let mut parts: Vec<Vec<std::ffi::OsString>> = Vec::new();
        let (_, all, _, _) = super::list_tree(
            &dir,
            types,
            &AtomicBool::new(false),
            &Default::default(),
            |_| {},
            |p| {
                if p.so_far.len() > p.first {
                    parts.push(p.so_far.iter().map(|f| f.0.clone()).collect())
                }
            },
        )
        .unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], [std::ffi::OsString::from("a/2.jpg")]);
        assert_eq!(parts[1].last().unwrap(), "a/deep/1.jpg");
        assert_eq!(parts[2].last().unwrap(), "b/3.jpg");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A link in the tree is followed only to a folder inside the tree. A link to a folder
    /// above it (`up -> ../..`), beside it (`beside -> ../../beside`) or anywhere else is not:
    /// it listed the folders around the tree, the user's own photos, or from `/` the whole disk.
    #[test]
    fn a_tree_follows_a_link_only_inside_it() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("glimt-test-tree-up-{}", std::process::id()));
        let tree = base.join("tree");
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::create_dir_all(tree.join(".inner")).unwrap();
        std::fs::create_dir_all(base.join("beside")).unwrap();
        for f in [
            "outside.jpg",
            "tree/a.jpg",
            "beside/b.jpg",
            "tree/.inner/c.jpg",
        ] {
            std::fs::write(base.join(f), "x").unwrap();
        }
        symlink("../..", tree.join("sub/up")).unwrap();
        symlink("../../beside", tree.join("sub/beside")).unwrap();
        symlink(base.join("beside"), tree.join("sub/far")).unwrap();
        symlink("../.inner", tree.join("sub/in")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        // Also when the tree is named through a link, as its real path is what counts.
        symlink(&tree, base.join("named")).unwrap();
        for top in [tree.clone(), base.join("named")] {
            let (_, files, _, _) =
                super::list_tree(&top, types, &stop, &Default::default(), |_| {}, |_| {}).unwrap();
            let files: Vec<_> = files.into_iter().map(|f| f.0).collect();
            // The hidden folder is not listed itself, so its image is seen only through the link,
            // and it is named by its real path in the tree.
            assert_eq!(files, [".inner/c.jpg", "a.jpg"], "{top:?}");
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A file reached through a link to a folder in the tree is named by its real path, the
    /// name `--remote select`'s client gives it, whichever way the walk came first. Named
    /// through the link, `--remote select` missed it in a tree with Include Subfolders.
    #[test]
    fn a_file_through_a_link_in_the_tree_has_its_real_name() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-name-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/x.jpg"), "x").unwrap();
        // Before "real" in name order, so the walk reaches the folder through the link first.
        std::os::unix::fs::symlink("real", dir.join("a-link")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (_, files, _, _) =
            super::list_tree(&dir, types, &stop, &Default::default(), |_| {}, |_| {}).unwrap();
        assert_eq!(files[0].0, "real/x.jpg");
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// On a file system whose folders all have inode 0 (sshfs with `-o use_ino`), the whole
    /// tree is listed, and a link back into it is still read once. Told apart by inode alone,
    /// every folder below the one opened counted as read already, and the tree was cut short.
    #[test]
    fn a_tree_without_unique_inodes_is_listed_whole() {
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-ino-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/deep")).unwrap();
        for f in ["1.jpg", "a/2.jpg", "a/deep/3.jpg"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        std::os::unix::fs::symlink("..", dir.join("a/again")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let file_id =
            |p: &std::path::Path| Some(crate::images::save::file_id_from_inode(p, (1, 0)));
        let real = |p: &std::path::Path| std::fs::canonicalize(p);
        let mut files: Vec<std::path::PathBuf> =
            super::walk_by(&dir, types, file_id, real, super::list_folder)
                .map(Result::unwrap)
                .flat_map(|(rel, _, files, _)| files.into_iter().map(move |f| rel.join(f.0)))
                .collect();
        files.sort();
        assert_eq!(
            files,
            ["1.jpg", "a/2.jpg", "a/deep/3.jpg"].map(std::path::PathBuf::from)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The top folder that cannot be read is an error of the walk, which stops there, so the
    /// listing fails and the list shown stays. Before, it was walked as empty, and a share that
    /// failed for a moment emptied the list and its selection. A folder below the top that
    /// cannot be read, for any reason, is an error too, after which the walk goes on with the
    /// rest of the tree. Before, any error but two stopped the walk, and a damaged subfolder
    /// left the whole tree unlisted at every listing. A folder that is gone has nothing in it.
    #[test]
    fn only_the_top_folder_that_cannot_be_read_fails_the_walk() {
        let dir =
            std::env::temp_dir().join(format!("glimt-test-walk-unread-{}", std::process::id()));
        for sub in ["a", "b", "c"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
            std::fs::write(dir.join(sub).join("1.jpg"), "x").unwrap();
        }
        let types = super::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let real = |p: &std::path::Path| std::fs::canonicalize(p);
        let walked = |failing: std::path::PathBuf, kind: std::io::ErrorKind| {
            let list = move |folder: &std::path::Path, types| match folder == failing {
                true => Err(std::io::Error::from(kind)),
                false => super::list_folder(folder, types),
            };
            let found: Vec<_> =
                super::walk_by(&dir, types, crate::images::save::file_id, real, list)
                    .map(|w| w.map(|(rel, _, files, _)| (rel, files.len())))
                    .map(|w| w.map_err(|(path, e)| (path, e.kind())))
                    .collect();
            found
        };
        let top = walked(dir.clone(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            top,
            [Err((dir.clone(), std::io::ErrorKind::PermissionDenied))]
        );
        for kind in [
            std::io::ErrorKind::Other,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let broken = walked(dir.join("b"), kind);
            let ok = |r: &str, n| Ok((std::path::PathBuf::from(r), n));
            assert_eq!(
                broken,
                [
                    ok("", 0),
                    ok("a", 1),
                    Err((dir.join("b"), kind)),
                    ok("c", 1)
                ]
            );
        }
        let gone = walked(dir.join("b"), std::io::ErrorKind::NotFound);
        assert_eq!(gone.len(), 4);
        assert!(gone.iter().all(Result::is_ok), "{gone:?}");

        let gone = super::list_folder(&dir.join("gone"), types).unwrap();
        assert_eq!(gone, (Vec::new(), Vec::new()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A link to a folder is followed only when its real path is known and inside the tree.
    /// The walk itself is checked, with a link whose real path is too long to work out: taken
    /// from `real_dir`, which makes such a path absolute instead, it was followed.
    #[test]
    fn a_link_whose_real_path_is_unknown_is_not_followed() {
        let top = std::path::Path::new("/home/a/pics");
        assert_eq!(super::inside(Ok(top.join("sub")), top), Some("sub".into()));
        assert_eq!(super::inside(Ok("/home/a".into()), top), None);
        let dir = std::env::temp_dir().join(format!("glimt-test-walk-long-{}", std::process::id()));
        // The folder is hidden, so it is reached only through the link.
        std::fs::create_dir_all(dir.join(".in")).unwrap();
        std::fs::write(dir.join(".in/b.jpg"), "x").unwrap();
        std::fs::write(dir.join("a.jpg"), "x").unwrap();
        std::os::unix::fs::symlink(".in", dir.join("long")).unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let real = |p: &std::path::Path| match p.ends_with("long") {
            true => Err(std::io::Error::from_raw_os_error(36)),
            false => std::fs::canonicalize(p),
        };
        let files: Vec<std::path::PathBuf> = super::walk_by(
            &dir,
            types,
            crate::images::save::file_id,
            real,
            super::list_folder,
        )
        .map(Result::unwrap)
        .flat_map(|(rel, _, files, _)| files.into_iter().map(move |f| rel.join(f.0)))
        .collect();
        assert_eq!(files, [std::path::PathBuf::from("a.jpg")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A path is compared by its folder's real path and its own name: `..` and the folder's
    /// links are worked out, and a link to a file keeps its own name.
    #[test]
    fn real_path_follows_the_folder_not_the_file() {
        let dir = std::env::temp_dir().join(format!("glimt-test-real-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.join("real/a.jpg"), dir.join("real/b.jpg")).unwrap();
        let real = dir.canonicalize().unwrap().join("real");
        assert_eq!(
            super::real_path(&dir.join("link/b.jpg")),
            real.join("b.jpg")
        );
        assert_eq!(
            super::real_path(&dir.join("link/../link/a.jpg")),
            real.join("a.jpg")
        );
        assert_eq!(
            super::real_dir(&dir.join("link/..")),
            real.parent().unwrap()
        );
        // A folder that is not there keeps the real path of the folders above it that are
        // there, and the rest as written: here only / is there.
        assert_eq!(
            super::real_dir(std::path::Path::new("/no/such")),
            std::path::Path::new("/no/such")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder's time is taken before it is read. A file made while the folder is read, which
    /// the read may miss, then changes the folder's time from the one recorded, and the folder
    /// watcher lists the folder again. Taken after the read, the time already held the file,
    /// and the file stayed out of the list.
    #[test]
    fn a_folder_is_timed_before_it_is_read() {
        let dir = std::env::temp_dir().join(format!("glimt-test-walk-time-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        // The folder's own changes are left a clock tick behind, so the file made below is
        // the only change in its tick.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        let list = |folder: &std::path::Path, types| {
            let listed = super::list_folder(folder, types);
            std::fs::write(folder.join("late.jpg"), "x").unwrap();
            listed
        };
        let real = |p: &std::path::Path| std::fs::canonicalize(p);
        for (rel, _, files, modified) in
            super::walk_by(&dir, types, crate::images::save::file_id, real, list)
                .map(Result::unwrap)
        {
            assert!(files.is_empty());
            let now = std::fs::metadata(dir.join(&rel))
                .unwrap()
                .modified()
                .unwrap();
            assert_ne!(modified, Some(now), "{rel:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tree_listing_stops() {
        use std::sync::atomic::AtomicBool;
        let dir = std::env::temp_dir().join(format!("glimt-test-tree-stop-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.jpg"), "x").unwrap();
        let types = super::Types {
            images: true,
            raw: true,
            videos: true,
            hidden: false,
        };
        assert_eq!(
            super::list_tree(
                &dir,
                types,
                &AtomicBool::new(false),
                &Default::default(),
                |_| {},
                |_| {}
            )
            .unwrap()
            .1
            .len(),
            1
        );
        let stopped = super::list_tree(
            &dir,
            types,
            &AtomicBool::new(true),
            &Default::default(),
            |_| {},
            |_| {},
        )
        .unwrap();
        let (own, files, read, left_out) = stopped;
        assert!(own.is_empty() && files.is_empty() && read.is_empty() && left_out.is_empty());
        // The top folder's own folders are told before any folder below is read: stopped
        // there, nothing of sub is listed.
        let stop = AtomicBool::new(false);
        let mut told = Vec::new();
        let (_, files, _, _) = super::list_tree(
            &dir,
            types,
            &stop,
            &Default::default(),
            |dirs| {
                told = dirs.to_vec();
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(told, [dir.join("sub")]);
        assert!(files.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Images are files, or links to files: a folder named like an image is a folder, and a
    /// pipe or a link to a device is left out.
    #[test]
    fn lists_only_files_as_images() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("glimt-test-kinds-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("2019.jpg")).unwrap();
        std::fs::write(dir.join("a.jpg"), "").unwrap();
        symlink(dir.join("a.jpg"), dir.join("b.jpg")).unwrap();
        symlink("/dev/zero", dir.join("zero.jpg")).unwrap();
        symlink(dir.join("gone"), dir.join("broken.jpg")).unwrap();
        let fifo = std::process::Command::new("mkfifo")
            .arg(dir.join("pipe.jpg"))
            .status();
        assert!(fifo.is_ok_and(|s| s.success()));
        let types = super::Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        };
        let (dirs, images) = super::list_folder(&dir, types).unwrap();
        let images: Vec<_> = images.into_iter().map(|f| f.0).collect();
        assert_eq!(images, ["a.jpg", "b.jpg"]);
        assert_eq!(dirs, [dir.join("2019.jpg")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The Types menu: images, RAW and videos each on their own; an opened file's type.
    #[test]
    fn lists_by_type() {
        use super::{Types, is_listed};
        use std::path::Path;
        let raw = Types {
            raw: true,
            ..Types::NONE
        };
        let images = Types {
            images: true,
            ..Types::NONE
        };
        assert!(is_listed(Path::new("a.CR2"), raw) && !is_listed(Path::new("a.cr2"), images));
        assert!(is_listed(Path::new("a.jpg"), images) && !is_listed(Path::new("a.jpg"), raw));
        assert!(!is_listed(
            Path::new("a.txt"),
            Types {
                videos: true,
                images: true,
                raw: true,
                hidden: false
            }
        ));
        let t = Types::of(Path::new("x/a.nef"));
        assert!(t.raw && !t.images && !t.videos && !t.hidden);
        assert!(Types::of(Path::new("x/.a.nef")).hidden);
        assert!(!Types::of(Path::new("a.txt")).images);
    }

    #[test]
    fn pattern_names() {
        let old = std::path::Path::new("/p/IMG_4711.JPG");
        assert_eq!(super::pattern_name("trip_###", old, 7), "trip_007.JPG");
        // Latin-1 "r\xf8ntgen.jpg": its ø kept as the byte it is, not made "\u{fffd}".
        use std::os::unix::ffi::OsStrExt;
        let latin = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/p/r\xf8ntgen.jpg"));
        assert_eq!(
            super::pattern_name("*-#", latin, 1).as_bytes(),
            b"r\xf8ntgen-1.jpg"
        );
        assert_eq!(super::pattern_name("*-#", old, 12), "IMG_4711-12.JPG");
        assert_eq!(
            super::pattern_name("x", std::path::Path::new("/p/noext"), 1),
            "x"
        );
    }

    #[test]
    fn sorts_numbers_by_value() {
        use std::cmp::Ordering::*;
        assert_eq!(super::natural_cmp("img2.jpg", "img10.jpg"), Less);
        assert_eq!(super::natural_cmp("img010", "img10"), Equal);
        assert_eq!(super::natural_cmp("a", "a1"), Less);
        // Numbers in brackets too, as a browser names downloads: a(2) before a(10) before a(100).
        assert_eq!(super::natural_cmp("a(2).jpg", "a(10).jpg"), Less);
        assert_eq!(super::natural_cmp("a(10).jpg", "a(100).jpg"), Less);
        let mut v = listed(&["b10", "B2", "a1", "b9"]);
        super::sort_by_names(&mut v, super::Sort::Number, false, false);
        assert_eq!(v, listed(&["b10", "b9", "B2", "a1"]));
        // Equal numbers in name order.
        let mut v = listed(&["a1", "a01"]);
        super::sort_by_names(&mut v, super::Sort::Number, true, false);
        let os = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let kept = super::kept_order(os(&["c", "a", "gone", "b"]), &listed(&["a", "b", "c"]));
        assert_eq!(kept, Some(listed(&["c", "a", "b"])));
        assert_eq!(
            super::kept_order(os(&["c", "a"]), &listed(&["a", "b", "c"])),
            None
        );
        assert_eq!(v, listed(&["a01", "a1"]));
        let mut v = listed(&["b.png", "c.JPG", "a.mp4", "d.jpg", "e"]);
        super::sort_by_names(&mut v, super::Sort::Extension, true, false);
        assert_eq!(v, listed(&["e", "c.JPG", "d.jpg", "a.mp4", "b.png"]));
    }

    /// The names `names`, each with an identity of its own, as a listing gives them.
    fn listed(names: &[&str]) -> Vec<super::ListedName> {
        let identity = |n: &str| std::num::NonZeroU64::new(n.bytes().map(u64::from).sum());
        (names.iter()).map(|n| ((*n).into(), identity(n))).collect()
    }

    /// Returns rows of `names` in `order`, each sorted by the key at its place in `keys` in an
    /// order that looks at the files, as a listing's thread makes them, with no file looked at.
    fn rows_of(
        names: Vec<super::ListedName>,
        keys: &[super::SortKey],
        order: (super::Sort, bool, bool),
    ) -> super::Rows {
        super::Rows::sorted(names, order, |i, _| keys[i].clone())
    }

    /// Returns what each row of `rows` is sorted by, in their order.
    fn keys_of(rows: &super::Rows) -> Vec<super::SortKey> {
        (0..rows.len())
            .filter_map(|i| rows.key(i).cloned())
            .collect()
    }

    /// A rename that puts rows out of their order puts each of them back at its place by a
    /// binary search, with about 2·log2(n) comparisons for each renamed row, and gives the rows
    /// that `Rows::sorted` gives for the new names, in Name order with and without Case, in
    /// Number order, descending, and in an EXIF order where some files have no date. Before,
    /// the rename sorted every row again on the window thread, with an allocation in each
    /// comparison when Case was off.
    #[test]
    fn a_rename_puts_its_rows_back_by_a_binary_search() {
        use super::{ROWS_COMPARED, Sort, SortKey};
        let n = 10_000;
        let names: Vec<String> = (0..n)
            .map(|i| match i % 2 {
                0 => format!("img{i}.png"),
                _ => format!("IMG{i}.png"),
            })
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let renames = [("img0.png", "zzz.png"), ("img5000.png", "aaa.png")];
        let orders = [
            (Sort::Name, true, false),
            (Sort::Name, true, true),
            (Sort::Number, true, false),
            (Sort::Name, false, false),
            (Sort::ExifOriginal, true, true),
        ];
        for order in orders {
            // Every third file has no EXIF date, and the others have dates that run backwards.
            let key = |i: usize| match order.0 {
                Sort::ExifOriginal if i.is_multiple_of(3) => SortKey::Exif(true, None),
                Sort::ExifOriginal => SortKey::Exif(false, Some(format!("{:05}", n - i))),
                _ => SortKey::Name,
            };
            let keys: Vec<SortKey> = (0..n).map(key).collect();
            let mut rows = rows_of(listed(&names), &keys, order);
            let place = |rows: &super::Rows, name: &str| {
                rows.files().iter().position(|f| f == name).unwrap()
            };
            let at: Vec<(usize, std::ffi::OsString)> = (renames.iter())
                .map(|(old, new)| (place(&rows, old), (*new).into()))
                .collect();
            ROWS_COMPARED.set(0);
            let renamed = rows.rename(&at).expect("the renamed rows are out of place");
            let compared = ROWS_COMPARED.get();
            let bound = renames.len() * 2 * (n as f64).log2().ceil() as usize;
            assert!(compared <= bound, "{compared} comparisons, {order:?}");
            let mut new_names = listed(&names);
            for (old, new) in renames {
                let row = new_names.iter_mut().find(|(f, _)| f == old).unwrap();
                row.0 = new.into();
            }
            let expected = rows_of(new_names, &keys, order);
            assert_eq!(renamed.listed_names(), expected.listed_names(), "{order:?}");
            assert_eq!(keys_of(&renamed), keys_of(&expected), "{order:?}");
            assert_eq!(renamed.order(), Some(order));
            assert_eq!(rows.order(), None, "the rows left behind claim no order");
        }
    }

    /// The order kept keeps the identity the new listing gives each name, so a name that
    /// another file has taken since is told apart.
    #[test]
    fn the_order_kept_has_the_identities_listed() {
        let names = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let mut new = listed(&["a", "b", "d"]);
        new[0].1 = std::num::NonZeroU64::new(1000);
        let kept = super::kept_order(names(&["b", "d", "a"]), &new).unwrap();
        assert_eq!(kept[2], new[0]);
    }

    /// A part of new names goes into the list in its order and makes it grow, and the names of
    /// the list stay, as the walk may still find them: by name with and without Case,
    /// descending and by number, and in an order that looks at the files by what each row is
    /// sorted by, which the list's rows carry and the part's rows bring, so no file is looked
    /// at. A part with no new name and no folder read whole leaves the list as it is, and a list
    /// still empty, as before the first part of a tree, takes the part whole. Before, a part was
    /// merged by its bytes in every order.
    #[test]
    fn a_part_of_new_names_goes_into_the_list_in_its_order() {
        use super::{Sort, SortKey};
        let none = super::ReadWhole::default();
        let by = |by, ascending, case| (by, ascending, case);
        let merge = |old: &[&str], new: &[&str], order| {
            let old = rows_of(listed(old), &[], order);
            let (merged, _) = old.merged(rows_of(listed(new), &[], order), &none).unwrap();
            merged.listed_names()
        };
        let cases = [
            (
                by(Sort::Name, true, true),
                ["B", "d", "m"],
                ["A", "c", "z"],
                "A B c d m z",
            ),
            (
                by(Sort::Name, true, false),
                ["b", "D", "m"],
                ["A", "c", "Z"],
                "A b c D m Z",
            ),
            (
                by(Sort::Name, false, true),
                ["m", "d", "b"],
                ["z", "c", "a"],
                "z m d c b a",
            ),
            (
                by(Sort::Number, true, false),
                ["i2", "i9", "i20"],
                ["i3", "i10", "i30"],
                "i2 i3 i9 i10 i20 i30",
            ),
        ];
        for (order, old, new, merged) in cases {
            let part = merge(&old, &new, order);
            assert_eq!(part, listed(&merged.split(' ').collect::<Vec<_>>()));
        }
        // By date, by the keys of both: x first, y between b and c, and z after c, whose time
        // is the same, by its name. Descending, the other way round.
        let time = |t| SortKey::Time(Some(t));
        for ascending in [true, false] {
            let order = by(Sort::Date, ascending, true);
            let old = rows_of(listed(&["a", "b", "c"]), &[2, 4, 9].map(time), order);
            let new = rows_of(listed(&["x", "y", "z"]), &[1, 5, 9].map(time), order);
            let (mut want, mut keys) = (["x", "a", "b", "y", "c", "z"], [1, 2, 4, 5, 9, 9]);
            if !ascending {
                want.reverse();
                keys.reverse();
            }
            let (merged, _) = old.merged(new, &none).unwrap();
            assert_eq!(merged.listed_names(), listed(&want));
            assert_eq!(keys_of(&merged), keys.map(time));
        }
        let name = by(Sort::Name, true, true);
        assert_eq!(merge(&["b", "d", "m"], &[], name), listed(&["b", "d", "m"]));
        assert_eq!(merge(&[], &["a", "b"], name), listed(&["a", "b"]));
    }

    /// A part goes into the list only in the very order the list is in: a list in another
    /// order, by name, by another key or the other way round, or in no order, gives the part
    /// back, and keys of two orders are never compared. An empty list takes the part in its
    /// order. Where the rows of the list went (`PlacesMoved`) is where the merge put them, and
    /// a name left out has no place. Before, a list of one image keyed by date took the parts of
    /// an order by size by its date, and a list without keys took parts by name in silence.
    #[test]
    fn a_part_goes_into_a_list_of_its_own_order_or_not_at_all() {
        use super::{Sort, SortKey};
        let time = |t| SortKey::Time(Some(t));
        let size = |n| SortKey::Size(Some(n));
        let none = super::ReadWhole::default();
        let (by_date, by_size) = ((Sort::Date, true, true), (Sort::Size, true, true));
        let part = || rows_of(listed(&["n", "o"]), &[1, 3].map(size), by_size);
        let lists = [
            rows_of(listed(&["a"]), &[time(2)], by_date),
            rows_of(listed(&["a"]), &[time(2)], (Sort::Created, true, true)),
            rows_of(listed(&["a"]), &[size(2)], (Sort::Size, false, true)),
            rows_of(listed(&["a"]), &[], (Sort::Name, true, true)),
            super::Rows::unsorted(listed(&["a"])),
        ];
        for list in lists {
            let order = list.order();
            assert_eq!(list.merged(part(), &none).err(), Some(part()), "{order:?}");
        }
        let empty = super::Rows::default();
        assert_eq!(empty.merged(part(), &none).unwrap().0, part());
        let list = rows_of(listed(&["a"]), &[size(2)], by_size);
        let (merged, _) = list.merged(part(), &none).unwrap();
        assert_eq!(merged.listed_names(), listed(&["n", "a", "o"]));
        assert_eq!(keys_of(&merged), [1, 2, 3].map(size));
        // The places through the merge, with a folder read whole that lost a name: d/2.png goes.
        let hashes = super::NameHashes::of(&["d/1.png".into(), "d/2.png".into()]).sorted();
        let mut read = super::ReadWhole::default();
        let so_far = listed(&["d/1.png"]);
        let folder = super::FolderReadWhole {
            so_far: &so_far,
            first: 0,
            folder: std::path::Path::new("d"),
            dirs: &[],
            types: super::Types::NONE,
        };
        hashes.note_folder(&folder, &mut read);
        let by_name = (Sort::Name, true, true);
        let old = listed(&["a.png", "d/1.png", "d/2.png", "x.png"]);
        let new = listed(&["0.png", "b.png", "e.png", "y.png"]);
        let list = rows_of(old.clone(), &[], by_name);
        let (merged, places) = list.merged(rows_of(new, &[], by_name), &read).unwrap();
        for (i, name) in old.iter().enumerate() {
            let place = places.new_place(i);
            match name.0 == "d/2.png" {
                true => assert_eq!(place, None),
                false => assert_eq!(place.map(|k| &merged.files()[k]), Some(&name.0)),
            }
        }
    }

    /// A whole list that kept the list's order takes what each name is sorted by from the
    /// list's rows, walking both in step, and gives `None` when a name of it is not among them,
    /// as when its file left the list after the listing began. Before, the list then took the
    /// whole list without its keys, and the parts of the next listing of the tree waited for a
    /// look at every file of the list.
    #[test]
    fn a_whole_list_in_the_lists_order_takes_the_lists_keys() {
        use super::{Sort, SortKey};
        let exif = |d: &str| SortKey::Exif(false, Some(d.into()));
        let order = (Sort::ExifOriginal, true, true);
        let keys = ["2001", "2002", "2003"].map(exif);
        let list = rows_of(listed(&["c", "a", "b"]), &keys, order);
        assert_eq!(list.files(), ["c", "a", "b"]);
        let mut kept = listed(&["c", "b"]);
        kept[1].1 = std::num::NonZeroU64::new(7);
        let rows = list.in_step(kept.clone()).unwrap();
        assert_eq!(rows.listed_names(), kept);
        assert_eq!(rows.order(), Some(order));
        assert_eq!(keys_of(&rows), [exif("2001"), exif("2003")]);
        assert_eq!(list.in_step(listed(&["c", "d"])), None, "d left the list");
        assert_eq!(list.in_step(listed(&["b", "c"])), None, "another order");
    }

    /// The order that a part goes into the list by (`in_order`) is the order that `Rows::sorted`
    /// sorts the whole list in, in every order, ascending and descending, with and without
    /// Case, for files whose names, dates and sizes differ in other ways.
    #[test]
    fn a_part_goes_in_by_the_order_of_the_whole_list() {
        use super::{SORTS, in_order, sort_key};
        let dir = std::env::temp_dir().join(format!("glimt-test-in-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names = [
            "b.png",
            "B.png",
            "a10.jpg",
            "a9.JPG",
            "a09.png",
            "c.webp",
            "img2.png",
            "IMG10.png",
            "é.png",
            "z.gif",
        ];
        for (k, name) in names.iter().enumerate() {
            std::fs::write(dir.join(name), vec![0; (k * 7) % 4]).unwrap();
            let when =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + (k as u64 * 5) % 3);
            let file = std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap();
            file.set_modified(when).unwrap();
        }
        for (sort, _, _) in SORTS {
            for (ascending, case) in [(true, true), (true, false), (false, true), (false, false)] {
                let order = (sort, ascending, case);
                let rows = super::Rows::sorted(listed(&names), order, |_, name| {
                    sort_key(&dir, name, sort)
                });
                let files = rows.listed_names();
                let key = |f: &super::ListedName| sort_key(&dir, &f.0, sort);
                for pair in files.windows(2) {
                    let (a, b) = ((&key(&pair[0]), &pair[0].0), (&key(&pair[1]), &pair[1].0));
                    assert!(
                        in_order(a, b, order).is_lt(),
                        "{:?} before {:?} in {order:?}",
                        pair[0].0,
                        pair[1].0
                    );
                }
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
