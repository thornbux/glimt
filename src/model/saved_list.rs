//! The saved file lists (Preferences → Remember file lists for faster loading). The list of a
//! folder or a tree, as its last whole listing found it, is kept in
//! `$XDG_CACHE_HOME/glimt/lists/`, one file for each place listed. A folder opened again shows
//! its saved list at once, while it is read again as usual, and the list read then takes its
//! place. No file operation acts on a row of a saved list: the list holds its rows as entries
//! not yet checked (`List::has_unchecked_entries`) until the listing's own list comes.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::model::listing::{Rows, Sort, SortKey, Types};

/// What a saved list's file starts with, before its version.
const MAGIC: &[u8; 8] = b"GLMTLIST";

/// The version of the format that follows the magic. A file of another version is removed, and
/// its folder is read as usual.
const VERSION: u32 = 1;

/// The most bytes that a saved list's rows may take unpacked.
/// shortcut: a list larger than 256 MiB unpacked (about 2.5 million names of 100 bytes) is taken
/// as damaged, removed, and its folder read as usual; raise it if folders grow past that.
const MOST_UNPACKED: usize = 256 << 20;

/// How many times the saved lists were cleared since Glimt started. A save carries the count
/// from when its listing began (`SaveTicket`), and is not written when the lists were cleared
/// since, so that a listing that began before the setting was turned off leaves no list behind.
static TIMES_CLEARED: AtomicU64 = AtomicU64::new(0);

/// What makes the listings of a place differ, as the text that the file of its saved list holds
/// and is named by: the folder's real path, whether its tree is listed, the types shown (hidden
/// files among them) and the order. Only `ListKey::of` makes one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListKey {
    text: Vec<u8>,
    types: Types,
    recursive: bool,
    order: (Sort, bool, bool),
}

impl ListKey {
    /// Returns the key of the listing of the folder `real_dir`, given by its real path, with its
    /// tree when `recursive`, of the files of `types`, in `order` as (order, ascending, case).
    pub(crate) fn of(
        real_dir: &Path,
        recursive: bool,
        types: Types,
        order: (Sort, bool, bool),
    ) -> ListKey {
        let (by, ascending, case) = order;
        let word = |on: bool, yes: &str, no: &str| if on { yes } else { no }.to_owned();
        let sort = crate::model::listing::SORTS.iter().find(|s| s.0 == by);
        let fields = [
            word(recursive, "tree", "folder"),
            word(types.images, "images", "-"),
            word(types.raw, "raw", "-"),
            word(types.videos, "videos", "-"),
            word(types.hidden, "hidden", "-"),
            sort.map_or("", |s| s.1).to_owned(),
            word(ascending, "ascending", "descending"),
            word(case, "case", "no-case"),
        ];
        let mut text = real_dir.as_os_str().as_encoded_bytes().to_vec();
        for field in fields {
            text.push(0);
            text.extend(field.as_bytes());
        }
        ListKey {
            text,
            types,
            recursive,
            order,
        }
    }

    /// Returns the file that holds the saved list of this key, named by the MD5 of the key in
    /// hexadecimal, or `None` when there is no cache folder.
    pub(crate) fn file(&self) -> Option<PathBuf> {
        let hash = crate::images::thumbs::md5(&self.text);
        let name: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        Some(saved_lists_dir()?.join(name))
    }

    /// Returns whether `name`, read from a saved list, can be a row of a listing of this key: a
    /// name of the folder, or a path in its tree, made of plain parts only, of a type shown. A
    /// name that leads out of the folder ("..", "/") is refused, as the window would show and
    /// decode it.
    fn allows(&self, name: &OsString) -> bool {
        let bytes = name.as_encoded_bytes();
        let path = Path::new(name);
        let plain = (path.components()).all(|c| matches!(c, Component::Normal(_)));
        !bytes.is_empty()
            && !bytes.contains(&0)
            && plain
            && (self.recursive || !bytes.contains(&b'/'))
            && crate::model::listing::is_listed(path, self.types)
    }
}

/// Whether a save of a saved list failed in a window since its setting was last turned on. Then
/// the window saves no list until the setting is turned on again or Glimt starts again, so that
/// a cache that cannot be written (a full home, a read-only `~/.cache`) is told once, and not
/// at every listing, each of which would also have a `--remote … --wait` answer "not done". Each
/// window has its own (`Folder::saves_stopped`), as one test's failed save stopped the saves of
/// the others' windows.
#[derive(Clone, Debug, Default)]
pub(crate) struct SavesStopped(std::sync::Arc<AtomicBool>);

impl SavesStopped {
    /// Stops the saves of the window, after a save failed.
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Lets the window save lists again, when the setting is turned on again.
    pub(crate) fn go_on(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    /// Returns whether a save failed since the saves last went on.
    fn is_stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// When a listing that may save its list began, counted by the times the saved lists were
/// cleared, and whether its window's saves stopped since. Only `SaveTicket::now` makes one.
#[derive(Clone, Debug)]
pub(crate) struct SaveTicket {
    times_cleared: u64,
    stopped: SavesStopped,
}

impl SaveTicket {
    /// Returns the ticket of a listing that begins now, in the window whose saves `stopped`
    /// tells of.
    pub(crate) fn now(stopped: &SavesStopped) -> SaveTicket {
        SaveTicket {
            times_cleared: TIMES_CLEARED.load(Ordering::SeqCst),
            stopped: stopped.clone(),
        }
    }

    /// Returns whether no clearing of the saved lists came after this ticket was made, and no
    /// save of its window failed.
    fn is_current(&self) -> bool {
        TIMES_CLEARED.load(Ordering::SeqCst) == self.times_cleared && !self.stopped.is_stopped()
    }
}

/// What a listing does with the saved list of its place, as the window decides when it starts.
#[derive(Clone, Debug)]
pub(crate) enum SavedListUse {
    /// Nothing: the setting is off, or the listing is not of one folder or tree read whole (a
    /// collection, several folders, a subfolder read again).
    Off,
    /// The listing saves its list when it ends whole.
    Save(SaveTicket),
    /// The listing first sends the saved list to the window, which shows it until the listing's
    /// own comes, and then saves its list when it ends whole. This is for a folder just opened,
    /// whose list is empty.
    ShowThenSave(SaveTicket),
}

impl SavedListUse {
    /// Returns the ticket of a listing that saves its list, or `None` when it saves none.
    pub(crate) fn ticket(&self) -> Option<SaveTicket> {
        match self {
            SavedListUse::Off => None,
            SavedListUse::Save(ticket) | SavedListUse::ShowThenSave(ticket) => Some(ticket.clone()),
        }
    }
}

/// The fields of a saved list's unpacked bytes that are not read yet.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    /// Takes the next `n` bytes. Returns `None` when fewer are left.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(taken)
    }

    /// Takes the next byte.
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    /// Takes the next 64-bit number.
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// Takes the next bytes after their length as a 32-bit number.
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = u32::from_le_bytes(self.take(4)?.try_into().ok()?);
        self.take(usize::try_from(n).ok()?)
    }

    /// Takes the next sort key, as `put_sort_key` wrote it.
    fn sort_key(&mut self) -> Option<SortKey> {
        let bool_of = |b: u8| match b {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        Some(match self.u8()? {
            0 => SortKey::Name,
            1 => SortKey::Time(None),
            2 => SortKey::Time(Some(self.u64()?)),
            3 => SortKey::Size(None),
            4 => SortKey::Size(Some(self.u64()?)),
            5 => SortKey::Exif(bool_of(self.u8()?)?, None),
            6 => {
                let no_date = bool_of(self.u8()?)?;
                let date = String::from_utf8(self.bytes()?.to_vec()).ok()?;
                SortKey::Exif(no_date, Some(date))
            }
            _ => return None,
        })
    }
}

impl crate::App {
    /// Turns Remember file lists for faster loading on or off. Turned off, every saved list is
    /// removed. Turned on, the window saves lists again after a save that failed.
    pub(crate) fn set_remember_file_lists(&mut self, on: bool) {
        self.config.remember_file_lists = on;
        match on {
            true => self.folder.saves_stopped().go_on(),
            false => clear_saved_lists(),
        }
    }
}

/// Returns `$XDG_CACHE_HOME/glimt/lists`, the folder of the saved lists, or `None` when there
/// is no cache folder.
pub(crate) fn saved_lists_dir() -> Option<PathBuf> {
    crate::files::config::xdg_file("XDG_CACHE_HOME", ".cache", "lists")
}

/// Removes every saved list, in the thread that writes Glimt's own files, after the saves
/// queued there before. A save of a listing that began before is not written
/// (`SaveTicket`). A folder that cannot be removed is told.
pub(crate) fn clear_saved_lists() {
    TIMES_CLEARED.fetch_add(1, Ordering::SeqCst);
    let Some(dir) = saved_lists_dir() else {
        return;
    };
    crate::files::file_io::write_later(move || {
        crate::files::file_io::remove_own_or_tell(&dir);
    });
}

/// Returns the saved list of `key`, read in the listing thread, or `None` when there is none.
/// A file that cannot be read as the list of this key (another version, damaged, too large, rows
/// out of their order or a name that is not the folder's) is removed, and its folder is read as
/// usual. The file is untrusted, as any file is: it is read with a size cap, its rows are
/// checked, and every action waits for the listing's own list.
pub(crate) fn read(key: &ListKey) -> Option<Rows> {
    let path = key.file()?;
    let rows = match crate::images::decode::read_limited(&path) {
        Ok(bytes) => parse(&bytes, key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => None,
    };
    if rows.is_none() {
        crate::files::file_io::write_later(move || {
            crate::files::file_io::remove_own_or_tell(&path);
        });
    }
    rows
}

/// Returns the rows of a saved list's file `bytes` when it is the list of `key`, or `None` when
/// it is of another version or key, damaged, or its rows are not in the key's order.
fn parse(bytes: &[u8], key: &ListKey) -> Option<Rows> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let (version, packed) = rest.split_first_chunk::<4>()?;
    if u32::from_le_bytes(*version) != VERSION {
        return None;
    }
    let plain =
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(packed, MOST_UNPACKED).ok()?;
    let mut fields = Fields(&plain);
    if fields.bytes()? != key.text.as_slice() {
        return None;
    }
    let count = fields.u64()?;
    // Each row takes at least four bytes, so a larger count cannot be.
    if count > (fields.0.len() / 4) as u64 {
        return None;
    }
    // The rows grow as they are read, with no room made from the count, which is the file's
    // word: a name takes six times its four bytes there, and a small file that unpacks to
    // 256 MiB of zeros would make room for 1.6 GB before its first row is refused.
    let looks = key.order.0.looks_at_files();
    let (mut names, mut keys) = (Vec::new(), Vec::new());
    for _ in 0..count {
        let name = OsString::from_vec(fields.bytes()?.to_vec());
        if !key.allows(&name) {
            return None;
        }
        names.push(name);
        if looks {
            keys.push(fields.sort_key()?);
        }
    }
    if !fields.0.is_empty() {
        return None;
    }
    Rows::from_saved(names, key.order, keys)
}

/// Returns `rows`, the whole list that a listing of `key` found, as the unpacked bytes of its
/// saved list, or `None` when the rows are in another order than the key's. The listing thread
/// makes them before it sends the rows, and packs and saves them after (`save`).
pub(crate) fn unpacked(key: &ListKey, rows: &Rows) -> Option<Vec<u8>> {
    if rows.order() != Some(key.order) {
        return None;
    }
    let mut out = Vec::new();
    put_bytes(&mut out, &key.text);
    out.extend((rows.len() as u64).to_le_bytes());
    for (i, name) in rows.files().iter().enumerate() {
        put_bytes(&mut out, name.as_encoded_bytes());
        if let Some(sort_key) = rows.key(i) {
            put_sort_key(&mut out, sort_key);
        }
    }
    Some(out)
}

/// Returns the file of a saved list whose unpacked bytes are `plain`: the magic, the version and
/// the bytes packed with zlib.
fn packed(plain: &[u8]) -> Vec<u8> {
    let mut file = MAGIC.to_vec();
    file.extend(VERSION.to_le_bytes());
    file.extend(miniz_oxide::deflate::compress_to_vec_zlib(plain, 1));
    file
}

/// Packs `plain`, the unpacked saved list of `key`, in the listing thread, and writes it in the
/// thread that writes Glimt's own files, unless the saved lists were cleared after the listing
/// began or a save of the window failed (`ticket`). The folder is made only the user's (0700),
/// and so is the file (0600), each from the start. A failed write is told, and stops the
/// window's saves (`SavesStopped`).
/// shortcut: no limit on how many lists are kept or how large they grow; Clear saved lists, or
/// the setting turned off, removes them all. Remove the oldest past a total if caches grow.
pub(crate) fn save(key: &ListKey, plain: &[u8], ticket: SaveTicket) {
    let Some(path) = key.file() else {
        return;
    };
    if !ticket.is_current() {
        return;
    }
    let file = packed(plain);
    crate::files::file_io::write_later(move || save_at(&path, &file, &ticket));
}

/// Writes the saved list `file` at `path`, in the thread that writes Glimt's own files, unless
/// `ticket` is no longer current. A failed write is told, and stops the window's saves.
fn save_at(path: &Path, file: &[u8], ticket: &SaveTicket) {
    if !ticket.is_current() {
        return;
    }
    if let Err(e) = write_saved(path, file) {
        ticket.stopped.stop();
        crate::files::file_io::save_failed(crate::tf!(
            "Could not save {name}: {e}",
            name = crate::files::file_io::shown_path(path),
            e
        ));
    }
}

/// Writes `file` at `path`, in a folder made only the user's when it is not there.
fn write_saved(path: &Path, file: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    crate::files::file_io::replace_private_file(path, file)
}

/// Adds `bytes` to `out` after their length as a 32-bit number.
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
}

/// Adds `key` to `out`: a tag, and the number or date it holds.
fn put_sort_key(out: &mut Vec<u8>, key: &SortKey) {
    match key {
        SortKey::Name => out.push(0),
        SortKey::Time(None) => out.push(1),
        SortKey::Time(Some(t)) => {
            out.push(2);
            out.extend(t.to_le_bytes());
        }
        SortKey::Size(None) => out.push(3),
        SortKey::Size(Some(s)) => {
            out.push(4);
            out.extend(s.to_le_bytes());
        }
        SortKey::Exif(no_date, None) => out.extend([5, u8::from(*no_date)]),
        SortKey::Exif(no_date, Some(date)) => {
            out.extend([6, u8::from(*no_date)]);
            put_bytes(out, date.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ListKey, SaveTicket, SavesStopped, packed, parse, put_bytes, save_at, unpacked};
    use crate::model::listing::{Rows, Sort, SortKey, Types};
    use std::ffi::OsString;
    use std::path::Path;

    /// The types of a folder of images, without hidden files.
    fn images() -> Types {
        Types {
            images: true,
            raw: false,
            videos: false,
            hidden: false,
        }
    }

    /// The key of a tree listed in `order`.
    fn tree_key(order: (Sort, bool, bool)) -> ListKey {
        ListKey::of(Path::new("/photos"), true, images(), order)
    }

    /// Returns rows of `names` sorted in `order`, each name given its key from `keys`.
    fn rows_of(names: &[&str], order: (Sort, bool, bool), keys: &[SortKey]) -> Rows {
        let listed = (names.iter()).map(|n| (OsString::from(n), None)).collect();
        Rows::sorted(listed, order, |i, _| {
            keys.get(i).cloned().unwrap_or_default()
        })
    }

    /// Returns the file of a saved list of `key` that holds `names` in the order given, in a
    /// name order.
    fn file_of(key: &ListKey, names: &[&str]) -> Vec<u8> {
        let mut plain = Vec::new();
        put_bytes(&mut plain, &key.text);
        plain.extend((names.len() as u64).to_le_bytes());
        for name in names {
            put_bytes(&mut plain, name.as_bytes());
        }
        packed(&plain)
    }

    /// A saved list read back is the list saved, in each kind of order, with what each row is
    /// sorted by, so that the list shown at once is in the order it claims.
    #[test]
    fn a_saved_list_reads_back_as_it_was_saved() {
        let names = ["b.png", "a.jpg", "sub/c.jpg"];
        let date = |d: &str| SortKey::Exif(false, Some(d.to_owned()));
        let cases = [
            (Sort::Name, Vec::new()),
            (Sort::Number, Vec::new()),
            (
                Sort::Date,
                vec![
                    SortKey::Time(Some(3)),
                    SortKey::Time(None),
                    SortKey::Time(Some(1)),
                ],
            ),
            (
                Sort::Size,
                vec![
                    SortKey::Size(Some(9)),
                    SortKey::Size(Some(2)),
                    SortKey::Size(None),
                ],
            ),
            (
                Sort::ExifOriginal,
                vec![
                    date("2021:01:01"),
                    SortKey::Exif(true, None),
                    date("2020:05:05"),
                ],
            ),
        ];
        for (by, keys) in cases {
            for ascending in [true, false] {
                let order = (by, ascending, false);
                let rows = rows_of(&names, order, &keys);
                let key = tree_key(order);
                let file = packed(&unpacked(&key, &rows).unwrap());
                let back = parse(&file, &key).unwrap();
                assert_eq!(back.files(), rows.files(), "{order:?}");
                assert_eq!(back.order(), Some(order));
                for i in 0..rows.len() {
                    assert_eq!(back.key(i), rows.key(i), "{order:?}");
                    assert_eq!(back.identity(i), None);
                }
            }
        }
        // Rows in another order than the key's are not saved.
        let by_name = rows_of(&names, (Sort::Name, true, true), &[]);
        assert_eq!(
            unpacked(&tree_key((Sort::Name, false, true)), &by_name),
            None
        );
    }

    /// A saved list's file is untrusted: damaged, as a packed file or with its rows damaged
    /// inside a whole stream, it is refused or read, never a panic.
    #[test]
    fn survives_broken_saved_list() {
        let order = (Sort::ExifDigitized, true, true);
        let date = SortKey::Exif(false, Some("2020:01:01 10:00:00".to_owned()));
        let keys = [date, SortKey::Exif(true, None)];
        let key = tree_key(order);
        let plain = unpacked(&key, &rows_of(&["a.jpg", "sub/b.jpg"], order, &keys)).unwrap();
        let file = packed(&plain);
        for broken in crate::support::test_support::broken(&file) {
            let _ = parse(&broken, &key);
        }
        for broken in crate::support::test_support::broken(&plain) {
            let _ = parse(&packed(&broken), &key);
        }
    }

    /// A file that cannot be the saved list of the place asked for is refused: rows out of
    /// their order or twice, a name that leads out of the folder or is not an image, another
    /// key, another version. Rows read from a file never claim an order they are not in, and a
    /// planted name is not shown.
    #[test]
    fn a_saved_list_that_cannot_be_the_places_is_refused() {
        let key = tree_key((Sort::Name, true, true));
        assert!(parse(&file_of(&key, &["a.jpg", "sub/b.jpg"]), &key).is_some());
        let refused: [&[&str]; 7] = [
            &["b.jpg", "a.jpg"],
            &["a.jpg", "a.jpg"],
            &["../a.jpg"],
            &["/etc/a.jpg"],
            &["sub/../a.jpg"],
            &["a.txt"],
            &[""],
        ];
        for names in refused {
            assert!(parse(&file_of(&key, names), &key).is_none(), "{names:?}");
        }
        let folder = ListKey::of(
            Path::new("/photos"),
            false,
            images(),
            (Sort::Name, true, true),
        );
        assert!(parse(&file_of(&folder, &["sub/b.jpg"]), &folder).is_none());
        let other = tree_key((Sort::Name, false, true));
        assert!(parse(&file_of(&key, &["a.jpg"]), &other).is_none());
        let mut next_version = file_of(&key, &["a.jpg"]);
        next_version[8] = 2;
        assert!(parse(&next_version, &key).is_none());
        // A count larger than the rows a small file holds, also one that unpacks to 16 MiB of
        // zeros, is refused, and no room is made for it.
        for (count, zeros) in [(u64::MAX, 0), ((16 << 20) / 4, 16 << 20)] {
            let mut plain = Vec::new();
            put_bytes(&mut plain, &key.text);
            plain.extend(u64::to_le_bytes(count));
            plain.resize(plain.len() + zeros, 0);
            assert!(parse(&packed(&plain), &key).is_none(), "{count}");
        }
        // In an order by date, each row needs a date's key.
        let by_date = tree_key((Sort::Date, true, true));
        assert!(parse(&file_of(&by_date, &["a.jpg"]), &by_date).is_none());
    }

    /// A save that fails stops the window's saves, so it is told once, and the setting turned
    /// on again lets them go on. Risk: a cache that cannot be written told at every listing,
    /// each of which also had a `--remote open … --wait` answer "not done".
    #[test]
    fn a_failed_save_is_told_once_until_the_setting_is_on_again() {
        let blocker =
            std::env::temp_dir().join(format!("glimt-test-saves-stopped-{}", std::process::id()));
        std::fs::write(&blocker, "a file where the folder of the lists would be").unwrap();
        let path = blocker.join("lists").join("list");
        let stopped = SavesStopped::default();
        // Another test's clearing of the lists between the ticket and the save skips the save,
        // so it is tried until one fails.
        let fails = || {
            (0..10).any(|_| {
                save_at(&path, b"list", &SaveTicket::now(&stopped));
                stopped.is_stopped()
            })
        };
        assert!(fails());
        save_at(&path, b"list", &SaveTicket::now(&stopped));
        assert_eq!(crate::files::file_io::take_saves_failed().len(), 1);
        stopped.go_on();
        assert!(fails());
        assert_eq!(crate::files::file_io::take_saves_failed().len(), 1);
        std::fs::remove_file(blocker).unwrap();
    }

    /// Each choice that makes a listing differ gives its saved list a file of its own: another
    /// folder, its tree, the types shown (hidden files among them) and each part of the order.
    #[test]
    fn the_key_tells_each_kind_of_listing_apart() {
        let order = (Sort::Name, true, true);
        let of = |dir: &str, tree: bool, types: Types, order| {
            ListKey::of(Path::new(dir), tree, types, order)
                .file()
                .unwrap()
        };
        let hidden = Types {
            hidden: true,
            ..images()
        };
        let raw = Types {
            raw: true,
            ..images()
        };
        let files = [
            of("/photos", false, images(), order),
            of("/photos/2020", false, images(), order),
            of("/photos", true, images(), order),
            of("/photos", false, hidden, order),
            of("/photos", false, raw, order),
            of("/photos", false, images(), (Sort::Date, true, true)),
            of("/photos", false, images(), (Sort::Name, false, true)),
            of("/photos", false, images(), (Sort::Name, true, false)),
        ];
        let distinct: std::collections::HashSet<_> = files.iter().collect();
        assert_eq!(distinct.len(), files.len());
        assert_eq!(
            of("/photos", false, images(), order),
            files[0],
            "the same key"
        );
        let dir = super::saved_lists_dir().unwrap();
        assert!(files.iter().all(|f| f.parent() == Some(dir.as_path())));
    }
}
