//! What a start decides about a job log that a kill cut short, as pure functions without I/O:
//! from the log's records and what the start saw on the disk (`Observations`) to what is done
//! with the log's files. A start first gathers every look it needs into one value
//! (`journal::observe`), then decides here, and then carries the decision out in journal.rs. So
//! each decision is tested on values, without a disk, and no look is made halfway through one.
//!
//! Every answer that cannot be sure is a `Why`, by which the log stays for a later start, never
//! a guess that would forget or move the marks of a file.

use crate::files::file_io::FileFingerprint;
use crate::files::job_log::Record;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What a look at a name, not followed through a link, saw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Seen {
    /// Something is at the name, with this fingerprint.
    There(FileFingerprint),
    /// The system said that nothing is at the name (NotFound).
    Gone,
    /// The look failed, as on a share that stopped answering, which says nothing either way.
    CannotTell,
}

/// What the looks at a folder saw, through a link: whether its name is surely gone, whether it is
/// surely a folder, and its device and inode with what was seen of the disk on that device.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FolderSeen {
    pub(crate) is_gone: bool,
    pub(crate) is_dir: bool,
    pub(crate) key: KeySeen,
}

/// The device and inode that a look at a folder found.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum KeySeen {
    /// The folder's device and inode now, and what was seen of the disk on that device.
    Key {
        device: u64,
        inode: u64,
        disk: DiskNow,
    },
    /// The look answered, but its file system gave no device and inode.
    NoKey,
    /// The look failed, or nothing is there.
    #[default]
    Failed,
}

/// What a start saw of the disk that holds a folder, on the folder's device now.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DiskNow {
    /// Where the folder is in its file system (`file_io::path_in_file_system`), where known.
    pub(crate) place: Option<PathBuf>,
    /// The medium in the folder's block device (`file_io::medium_of_device`), where known.
    pub(crate) medium: Option<String>,
    /// The volume that the volumes read once for the log name for the folder's device.
    pub(crate) volume: Option<OsString>,
    /// Whether that volume, looked up afresh after the volumes were read, is on the folder's
    /// device still. A card swapped in the meantime under the same device number is not (#284).
    pub(crate) is_volume_still_on_device: bool,
    /// Whether the volumes could be read at all, which they cannot in a Flatpak's sandbox.
    pub(crate) are_volumes_readable: bool,
    /// Whether the device is a block device, a disk, card or stick, whose number the next one in
    /// the same reader or port gets.
    pub(crate) is_on_a_block_device: bool,
}

/// What the `folder`, `volume`, `medium` and `in-file-system` records of a log noted of a folder:
/// its device, its inode (0 where its file system keeps none), the volume it is on where its
/// device has a name in /dev/disk/by-uuid, the medium in its block device, and where it is in its
/// file system. Only `disk_of` and `folder_of` compare them with a folder now
/// (`source_rules::one_rule_tells_the_folder_a_log_noted`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FolderNoted {
    pub(crate) noted_device: u64,
    pub(crate) noted_inode: u64,
    pub(crate) noted_volume: Option<OsString>,
    pub(crate) noted_medium: Option<String>,
    /// The folder noted, whose path the place below compares with a folder above it.
    pub(crate) noted_folder: PathBuf,
    pub(crate) noted_in_file_system: Option<PathBuf>,
}

/// Everything a start saw on the disk for one log, gathered once before any decision. Only
/// `Observations::look` makes it, so that each folder's look holds for the whole time its names
/// were looked at.
#[derive(Debug, Default)]
pub(crate) struct Observations {
    /// The folders that the log's records noted.
    noted: HashMap<PathBuf, FolderNoted>,
    /// The folders looked at: those of the log's names, and above a folder that is gone, each
    /// folder up to the first that is not.
    folders: HashMap<PathBuf, FolderSeen>,
    /// The names looked at.
    names: HashMap<PathBuf, Seen>,
    /// The id of this boot, where it is known.
    boot: Option<String>,
}

impl Observations {
    /// Looks at everything a start needs to decide about a log that noted the folders `noted`,
    /// with `look_at_folder` and `look_at_name`. It looks first at the `folders` and at the
    /// folders of the `names`, and above a folder that is gone at each folder up to the first
    /// that is not (`is_gone`). Then it looks at the `names`, and then at each of those folders
    /// again. A folder whose two looks differ counts as one whose look failed. So a stick
    /// mounted during the looks does not count as there yet. A stick pulled out or a share
    /// ejected during them does not count as the folder that its names, which then answer
    /// NotFound, were deleted from.
    pub(crate) fn look<'a>(
        noted: HashMap<PathBuf, FolderNoted>,
        folders: impl Iterator<Item = &'a Path>,
        names: &[&'a Path],
        mut look_at_folder: impl FnMut(&Path) -> FolderSeen,
        look_at_name: impl Fn(&Path) -> Seen,
        boot: Option<String>,
    ) -> Self {
        let mut seen: HashMap<PathBuf, FolderSeen> = HashMap::new();
        for folder in folders.chain(names.iter().filter_map(|name| name.parent())) {
            let mut at = Some(folder);
            while let Some(folder) = at.filter(|folder| !seen.contains_key(*folder)) {
                let looked = look_at_folder(folder);
                at = folder.parent().filter(|_| looked.is_gone);
                seen.insert(folder.to_path_buf(), looked);
            }
        }
        let names = (names.iter())
            .map(|name| (name.to_path_buf(), look_at_name(name)))
            .collect();
        for (folder, first) in &mut seen {
            if look_at_folder(folder) != *first {
                *first = FolderSeen::default();
            }
        }
        Observations {
            noted,
            folders: seen,
            names,
            boot,
        }
    }

    /// Returns what the look at the name `path` saw. A name that was not looked at counts as one
    /// whose look failed, which keeps the log rather than decide on nothing.
    pub(crate) fn name(&self, path: &Path) -> Seen {
        self.names.get(path).copied().unwrap_or(Seen::CannotTell)
    }

    /// Returns what the looks at the folder `path` saw. A folder that was not looked at counts as
    /// one whose looks failed.
    fn folder(&self, path: &Path) -> FolderSeen {
        self.folders.get(path).cloned().unwrap_or_default()
    }
}

/// Why a start cannot be sure, so that the log and its files' marks stay for a later start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Why {
    /// A look failed.
    LookFailed,
    /// A name or its folder is not there, as on a stick or share not mounted at this start.
    NotThere,
    /// The log noted no folder of the name, though it noted others.
    NotNoted,
    /// The folder is somewhere else in its file system than the log noted, as an empty mount
    /// point of a bind mount not mounted yet is, on the same disk (#270).
    NotInItsPlace,
    /// The folder is on another disk than the log noted, as another card at the same path is.
    AnotherDisk,
    /// shortcut: in the same boot, a card or stick that cannot be told from the one the job
    /// worked on (a card cloned with dd, a second ext4 stick in the Flatpak) is not taken for
    /// it, as another medium is in the reader now; the limit is the maintainer's (AGENTS.md,
    /// *Limits that are decided, not chased*), and a change to it is the maintainer's too.
    AnotherMediumInThisBoot,
    /// Nothing that was seen tells the disk noted from another one on the same device.
    NotToldApart,
}

/// A decision, or why it cannot be made yet.
pub(crate) type Decided<T> = Result<T, Why>;

/// Whether a folder now is on the disk, or is the folder, that a log noted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Disk {
    /// Surely the one noted.
    Same,
    /// shortcut: after a restart, a card whose volume cannot be read (in the Flatpak, or when its
    /// entry in /dev/disk/by-uuid cannot be read) and whose folder has no lasting inode is told
    /// by its device alone, as the medium's number holds only until the restart; the limit is
    /// the maintainer's (AGENTS.md, *Limits that are decided, not chased*). Without it such a log
    /// would stay for good.
    SameByDeviceAfterRestart,
    /// Surely another one.
    Another,
    /// Cannot be told now.
    Wait(Why),
}

impl Disk {
    /// Returns `Ok(true)` for the one noted, `Ok(false)` for surely another, and why not when it
    /// cannot be told.
    fn answer(self) -> Decided<bool> {
        match self {
            Disk::Same | Disk::SameByDeviceAfterRestart => Ok(true),
            Disk::Another => Ok(false),
            Disk::Wait(why) => Err(why),
        }
    }
}

/// Something a start tells the user about one file, as it decides.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tell {
    /// A Move's file at this new name is not the one that went: it changed since, or has no
    /// fingerprint to tell it by.
    ChangedAtNewName(PathBuf),
    /// A Move's file is no longer at this new name.
    GoneFromNewName(PathBuf),
    /// A photo that a delete for good was deleting is gone, and is found so for the first time:
    /// its marks and hidden sidecar wait for the next start.
    FoundGone(PathBuf),
    /// A photo found gone at an earlier start is still gone, so its delete is finished.
    StillGone(PathBuf),
    /// A photo found gone at an earlier start is there again, and keeps its marks.
    ThereAgain(PathBuf),
}

/// What the records of a delete's job log say, read at the start.
#[derive(Debug, Default)]
pub(crate) struct DeleteRecords {
    /// The kind of the log's job, "delete" or "trash".
    pub(crate) job: &'static str,
    /// The files the log names, in its order.
    pub(crate) names: Vec<PathBuf>,
    /// The fingerprint of each of `names`, taken before it went.
    pub(crate) known: Vec<Option<FileFingerprint>>,
    /// The collection shown during the delete.
    pub(crate) collection: Option<PathBuf>,
    /// The sidecars put aside, as (photo, sidecar, hidden name).
    pub(crate) aside: Vec<[PathBuf; 3]>,
    /// The photos kept while their sidecar may still be hidden.
    pub(crate) kept: HashSet<PathBuf>,
    /// The files written down as gone (`done`). A log written anew from one of Glimt 0.92 or
    /// older has a `done` for each name, as the old logs read a name that is gone as a file that
    /// went.
    pub(crate) done: HashSet<PathBuf>,
    /// The files of a delete for good without `done` that an earlier start found gone.
    pub(crate) found: HashSet<PathBuf>,
    /// The folders of the files, each as the log noted it.
    pub(crate) folders: HashMap<PathBuf, FolderNoted>,
    /// The logs that the log's `old-log` records name, which it was written anew from.
    pub(crate) old_logs: Vec<PathBuf>,
}

/// How a start sorts the files of a delete's log (`sort_deleted`).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct DeleteSorted {
    /// The files that went: their marks are forgotten.
    pub(crate) gone: Vec<PathBuf>,
    /// The files still there as the log knew them: they take back their sidecars.
    pub(crate) stayed: Vec<PathBuf>,
    /// The files not there as the delete left them: a kept photo moved away, or another file.
    pub(crate) not_as_left: Vec<PathBuf>,
    /// The files that this start cannot tell about, whose steps stay in the log.
    pub(crate) untold: HashSet<PathBuf>,
    /// The photos of a delete for good found gone for the first time at this start.
    pub(crate) found_now: HashSet<PathBuf>,
    /// What is told, in the log's order.
    pub(crate) tells: Vec<Tell>,
}

/// What an Undo from the trash had put back when Glimt was killed (`back_from_the_trash`).
#[derive(Debug, PartialEq)]
pub(crate) struct BackFromTrash {
    /// Each file back, with its marks and stars.
    pub(crate) marks: Vec<(PathBuf, (u8, u8))>,
    /// The places of the files back in the collection shown.
    pub(crate) places: Vec<(usize, PathBuf)>,
    /// The collection shown during the Undo.
    pub(crate) collection: Option<PathBuf>,
}

/// What a Rename Folder or a Move Folder to Trash had done when Glimt was killed.
#[derive(Debug, PartialEq)]
pub(crate) enum FolderDone {
    /// It had not changed the folder yet.
    NothingToFollow,
    /// The folder (first) has its new name (second).
    Renamed(PathBuf, PathBuf),
    /// The folder went to the trash, and its files leave the collection shown.
    Trashed {
        folder: PathBuf,
        collection: Option<PathBuf>,
    },
}

/// Returns the folders that the `folder` records among `records` noted, each with the volume, the
/// medium and the place in its file system that the `volume`, `medium` and `in-file-system`
/// records of the same folder noted.
pub(crate) fn folders_noted(records: &[Record]) -> HashMap<PathBuf, FolderNoted> {
    let mut folders: HashMap<PathBuf, FolderNoted> = (records.iter())
        .filter_map(|record| match record {
            Record::Folder {
                path,
                device,
                inode,
            } => Some((
                path.clone(),
                FolderNoted {
                    noted_device: *device,
                    noted_inode: *inode,
                    noted_volume: None,
                    noted_medium: None,
                    noted_folder: path.clone(),
                    noted_in_file_system: None,
                },
            )),
            _ => None,
        })
        .collect();
    for record in records {
        match record {
            Record::Volume { path, volume } => {
                if let Some(noted) = folders.get_mut(path) {
                    noted.noted_volume = Some(volume.clone());
                }
            }
            Record::Medium { path, medium } => {
                if let Some(noted) = folders.get_mut(path) {
                    noted.noted_medium = Some(medium.clone());
                }
            }
            Record::InFileSystem {
                path,
                in_file_system,
            } => {
                if let Some(noted) = folders.get_mut(path) {
                    noted.noted_in_file_system = Some(in_file_system.clone());
                }
            }
            _ => {}
        }
    }
    folders
}

/// Returns whether `folder`, on the device `device` now and with the inode `inode`, is on the
/// disk or card that its records `noted` saw, as `now` shows the disk and in the boot `boot`:
/// the one rule by which a start tells a volume. `inode` is `None` for a folder above the one
/// noted, whose own inode was never noted. Returns `Another` only for surely another disk, and
/// `Wait` where it cannot be told, so that the log and the marks stay for a later start, never
/// forgotten on a guess.
///
/// A noted volume decides, not the device, as a stick plugged in again may get another device
/// number and another card in the same reader gets the same one. It is surely the same only
/// when its block device, looked up afresh, is the folder's now. A volume not found now, or
/// volumes that cannot be read now, leave it to the medium noted. In a later boot, where no
/// volume can be read at all, the folder's lasting inode or its device tells it, as without a
/// volume (#278).
///
/// Within the boot that noted it, a medium that is known now and is another one decides before
/// the volume and the inode: it is surely another card, or the one noted plugged in again, which
/// cannot be told apart. A card cloned with dd has the volume of the one noted (#272), and every
/// ext4 stick's top folder has the inode 2 (#314), so either alone took another card at the same
/// path for the one noted.
///
/// A folder whose place in its file system is not the one noted, or above it, is not told
/// either: a bind mount of another folder of the same disk, not mounted yet, leaves an empty
/// mount point there with the volume, the medium and the device noted (#270).
///
/// Without a volume, another device cannot tell. On the noted device, the folder's own lasting
/// inode, when it is the one noted, tells the disk. Without it, the device number tells it only
/// off a block device, as a block device's number is given to the next card in the same reader
/// (#179, #266, #270). There the medium noted tells it within one boot (#305, #306). A medium
/// noted in an earlier boot, or none noted, cannot tell, and where then no volume can be read
/// either and no lasting inode tells the folder, the device tells it, as in Glimt 0.97.
pub(crate) fn disk_of(
    folder: &Path,
    noted: &FolderNoted,
    (device, inode): (u64, Option<u64>),
    now: &DiskNow,
    boot: Option<&str>,
) -> Disk {
    if let Some(noted_place) = &noted.noted_in_file_system
        && let Ok(below) = noted.noted_folder.strip_prefix(folder)
        && (now.place.as_ref()).is_some_and(|place| place.join(below) != *noted_place)
    {
        return Disk::Wait(Why::NotInItsPlace);
    }
    let noted_boot = (noted.noted_medium.as_deref()).and_then(|m| m.rsplit_once(':'));
    let is_of_another_boot = noted_boot.is_none_or(|(noted_boot, _)| Some(noted_boot) != boot);
    let medium_now = now.medium.as_deref();
    let is_another_medium = !is_of_another_boot
        && (noted.noted_medium.as_deref())
            .is_some_and(|noted| medium_now.is_some_and(|m| m != noted));
    if let (Some(volume), Some(found)) = (&noted.noted_volume, &now.volume) {
        if found != volume {
            return Disk::Another;
        }
        if is_another_medium {
            return Disk::Wait(Why::AnotherMediumInThisBoot);
        }
        if now.is_volume_still_on_device {
            return Disk::Same;
        }
    }
    let is_the_inode_noted = noted.noted_inode != 0 && inode == Some(noted.noted_inode);
    let is_the_medium_noted =
        (noted.noted_medium.as_deref()).is_some_and(|n| medium_now == Some(n));
    let has_no_inode_to_tell = noted.noted_inode == 0 || inode.is_none();
    let is_after_a_restart_without_volumes = is_of_another_boot && !now.are_volumes_readable;
    let told = match noted.noted_volume {
        // A disk whose volume was noted is told by its medium within the boot, as its folders'
        // inodes may be another stick's at the same port, and in a later boot as one without
        // a volume is, where no volume can be read now.
        Some(_) if is_the_medium_noted => Some(Disk::Same),
        Some(_) if is_after_a_restart_without_volumes && is_the_inode_noted => Some(Disk::Same),
        Some(_) if is_after_a_restart_without_volumes && has_no_inode_to_tell => {
            Some(Disk::SameByDeviceAfterRestart)
        }
        Some(_) => None,
        None if is_the_inode_noted || !now.is_on_a_block_device || is_the_medium_noted => {
            Some(Disk::Same)
        }
        None if has_no_inode_to_tell && is_after_a_restart_without_volumes => {
            Some(Disk::SameByDeviceAfterRestart)
        }
        None => None,
    };
    if is_another_medium {
        return Disk::Wait(Why::AnotherMediumInThisBoot);
    }
    match told {
        Some(told) if device == noted.noted_device => told,
        _ => Disk::Wait(Why::NotToldApart),
    }
}

/// Returns whether `folder`, on the device and with the inode of `key` now, is the folder that
/// its records `noted` saw: the one rule by which a start tells it. It must be on the disk noted
/// (`disk_of`), and have the inode noted where one was (not 0), so that a folder made again at
/// the name is not taken for it. Returns `Another` for another disk and for another folder: one
/// with another lasting inode on the noted volume or device, made at the name after the job.
pub(crate) fn folder_of(
    folder: &Path,
    noted: &FolderNoted,
    key: (u64, Option<u64>),
    now: &DiskNow,
    boot: Option<&str>,
) -> Disk {
    let (device, inode) = key;
    let is_another_inode = inode.is_some_and(|i| noted.noted_inode != 0 && i != noted.noted_inode);
    match disk_of(folder, noted, key, now, boot) {
        Disk::Same | Disk::SameByDeviceAfterRestart if is_another_inode => Disk::Another,
        // On the noted device, another lasting inode is surely another folder, whichever disk
        // holds it.
        Disk::Wait(_) if device == noted.noted_device && is_another_inode => Disk::Another,
        disk => disk,
    }
}

/// Returns whether the folder of `path` is another folder than the one that the log noted there,
/// made at its name after the job, or on another disk at the same path (`folder_of`). A folder
/// the log has no record of, one that is surely gone, and one whose file system gives no inode,
/// are not counted as another. Returns why not when the look at the folder failed, or the disk
/// cannot be told.
pub(crate) fn is_another_folder(obs: &Observations, path: &Path) -> Decided<bool> {
    let Some((folder, noted)) = path.parent().and_then(|f| Some((f, obs.noted.get(f)?))) else {
        return Ok(false);
    };
    let seen = obs.folder(folder);
    match &seen.key {
        KeySeen::Failed if seen.is_gone => Ok(false),
        KeySeen::Failed => Err(Why::LookFailed),
        KeySeen::NoKey => Ok(false),
        KeySeen::Key {
            device,
            inode,
            disk,
        } => {
            let key = (*device, Some(*inode));
            Ok(!folder_of(folder, noted, key, disk, obs.boot.as_deref()).answer()?)
        }
    }
}

/// Returns whether nothing is at `path` now, the one rule by which a start reads a path that a
/// job log names. Returns `Ok(true)` when its name is surely not there while its folder surely
/// is, on the disk the log noted, and `Ok(false)` when something is at its name. A folder that is
/// gone counts as deleted, and its files as gone, when the nearest folder above it is on the
/// disk the log noted (`removed_from_its_device`). Returns why not when the looks cannot tell:
/// the folder is not there, as on a stick or a share not back yet at this start, or it is not
/// surely on the disk the log noted, or the log noted none of it, or a look failed. A log that
/// noted no folder at all is of Glimt 0.92, or written anew from one, and only the folder's
/// being there is asked of it.
///
/// The folder is looked at before the name and again after it (`Observations::look`), so that a
/// stick mounted or pulled out between the looks counts as not there.
pub(crate) fn is_gone(obs: &Observations, path: &Path) -> Decided<bool> {
    if let Seen::There(_) = obs.name(path) {
        return Ok(false);
    }
    if !obs.noted.is_empty() {
        let folder = path.parent().ok_or(Why::NotNoted)?;
        let noted = obs.noted.get(folder).ok_or(Why::NotNoted)?;
        let seen = obs.folder(folder);
        if seen.is_gone {
            return removed_from_its_device(obs, folder, noted);
        }
        let KeySeen::Key {
            device,
            inode,
            disk,
        } = &seen.key
        else {
            return Err(Why::LookFailed);
        };
        let key = (*device, Some(*inode));
        if !disk_of(folder, noted, key, disk, obs.boot.as_deref()).answer()? {
            return Err(Why::AnotherDisk);
        }
    }
    let has_folder = path
        .parent()
        .is_some_and(|folder| obs.folder(folder).is_dir);
    match has_folder && obs.name(path) == Seen::Gone {
        true => Ok(true),
        false => Err(Why::NotThere),
    }
}

/// Tells whether `folder`, which is surely gone, was removed from the disk it was noted on, as
/// `noted` says. Returns `Ok(true)` when the nearest folder above it that surely is there is
/// surely on that disk (`disk_of`), as then the folder was deleted, not taken away with a stick
/// or share. Returns why not when that folder is on another disk, as an empty mount point is, or
/// when it cannot be told or a look failed.
fn removed_from_its_device(
    obs: &Observations,
    folder: &Path,
    noted: &FolderNoted,
) -> Decided<bool> {
    let mut above = folder.parent();
    while let Some(ancestor) = above {
        let seen = obs.folder(ancestor);
        if seen.is_dir {
            let KeySeen::Key { device, disk, .. } = &seen.key else {
                return Err(Why::LookFailed);
            };
            let disk = disk_of(ancestor, noted, (*device, None), disk, obs.boot.as_deref());
            return disk.answer()?.then_some(true).ok_or(Why::AnotherDisk);
        }
        if !seen.is_gone {
            return Err(Why::LookFailed);
        }
        above = ancestor.parent();
    }
    Err(Why::NotThere)
}

/// Returns whether the file at `path` has the size and time of the fingerprint `known`, which a
/// delete's log took before the file was to go. The device is left out, as after a reboot.
/// Returns false when there is no fingerprint or the look failed.
pub(crate) fn is_as_known(obs: &Observations, path: &Path, known: Option<FileFingerprint>) -> bool {
    match (obs.name(path), known) {
        (Seen::There((_, size, time)), Some((_, was, at))) => (size, time) == (was, at),
        _ => false,
    }
}

/// Returns the pairs of a Move's log, `pairs`, whose files went before Glimt was killed: those
/// whose old name is surely not there any more and whose new one is and holds the file that
/// went (`is_the_file_that_went`, by the fingerprints `known`, one for each pair). A file that
/// had not gone yet is still at its old name, and a pair cut off by the kill has names that are
/// not there, so both are left out. A file at the old name in another folder than the log noted
/// there, as on another card at the same mount path, says nothing about the file that went: only
/// its new name tells. Returns also what is told, also when it cannot decide.
///
/// Returns why not when a file may have left its old name and its new one cannot be told
/// (`is_gone`), as on a stick not mounted at this start, and when its new name holds a file while
/// its old name cannot be told (`has_left`), so that the log stays for a later start.
pub(crate) fn moves_that_went(
    pairs: &[(PathBuf, PathBuf)],
    known: &[Option<FileFingerprint>],
    obs: &Observations,
) -> (Decided<Vec<(PathBuf, PathBuf)>>, Vec<Tell>) {
    let mut tells = Vec::new();
    let mut decide = || {
        let mut went = Vec::new();
        for (step, (from, to)) in pairs.iter().enumerate() {
            let left = has_left(obs, from);
            let is_still_there = left == Ok(false) && !is_another_folder(obs, from)?;
            if is_still_there || is_gone(obs, to)? {
                continue;
            }
            left?;
            if is_the_file_that_went(obs, to, known.get(step).copied().flatten(), &mut tells)? {
                went.push((from.clone(), to.clone()));
            }
        }
        Ok(went)
    };
    let went = decide();
    (went, tells)
}

/// Returns whether a file left the old name `from` of a Move's step, which a failed look cannot
/// tell. A name in a folder that is gone counts as left, as a folder emptied by a Move is often
/// removed after it, and only the new name tells where the file went.
fn has_left(obs: &Observations, from: &Path) -> Decided<bool> {
    match obs.name(from) {
        Seen::There(_) => Ok(false),
        Seen::Gone => Ok(true),
        Seen::CannotTell => Err(Why::LookFailed),
    }
}

/// Returns whether the file at `to` is the file that a Move's step moved there, by the step's
/// fingerprint `known`. Another program may have put another photo there since, so the marks and
/// the sidecar of the file that went would else go to a later photo of that name. A file is told
/// by its size and time, which a move to another disk keeps to the resolution of that disk's
/// file system, so the times are compared as any file system keeps them. A pair left alone is
/// added to `tells`. Returns why not when the look at `to` failed.
fn is_the_file_that_went(
    obs: &Observations,
    to: &Path,
    known: Option<FileFingerprint>,
    tells: &mut Vec<Tell>,
) -> Decided<bool> {
    use crate::files::file_io::same_time_as_kept;
    let tell = match (obs.name(to), known) {
        (Seen::CannotTell, _) => return Err(Why::LookFailed),
        (Seen::There((_, size, time)), Some((_, was, at)))
            if size == was && same_time_as_kept(at, time) =>
        {
            return Ok(true);
        }
        (Seen::Gone, Some(_)) => Tell::GoneFromNewName(to.to_path_buf()),
        _ => Tell::ChangedAtNewName(to.to_path_buf()),
    };
    tells.push(tell);
    Ok(false)
}

/// Returns what the records of a delete's job log, of kind `job`, say. The parser leaves out a
/// record about a step that the log does not have before it, so each number is a step's.
pub(crate) fn delete_records(job: &'static str, records: &[Record]) -> DeleteRecords {
    let mut read = DeleteRecords {
        job,
        ..Default::default()
    };
    for record in records {
        match record {
            Record::Step {
                from, fingerprint, ..
            } => {
                read.names.push(from.clone());
                read.known.push(*fingerprint);
            }
            Record::Shown(collection) => read.collection = Some(collection.clone()),
            Record::Aside {
                number,
                sidecar,
                hidden,
            } => (read.aside).push([read.names[*number].clone(), sidecar.clone(), hidden.clone()]),
            Record::Kept(number) => {
                read.kept.insert(read.names[*number].clone());
            }
            Record::Done(number) => {
                read.done.insert(read.names[*number].clone());
            }
            Record::Found(number) => {
                read.found.insert(read.names[*number].clone());
            }
            Record::OldLog(path) => read.old_logs.push(path.clone()),
            _ => {}
        }
    }
    read.folders = folders_noted(records);
    read
}

/// Sorts the files of the delete whose log says `r`: those that went, whose name is surely not
/// there any more in a folder that surely is (`is_gone`), those that stayed as the log knew them,
/// those not there as the delete left them, and those this start cannot tell about. Another card
/// at the same mount path holds none of the log's files, and a folder that cannot be told may be
/// such a card. A photo that the delete kept never counts as one that went, because the user may
/// have moved it away after the delete.
///
/// A delete for good whose step has no `done` may have been cut by the kill before its photo
/// went or after. When its photo is surely gone, the start that first finds it keeps it untold,
/// and it is found now (`found`), told once. A later start finishes the step: when the photo is
/// still surely gone, it went, and when it is there, it stayed. Each is told once.
pub(crate) fn sort_deleted(r: &DeleteRecords, obs: &Observations) -> DeleteSorted {
    let mut sorted = DeleteSorted::default();
    let is_unfinished = |name: &PathBuf| r.job == "delete" && !r.done.contains(name);
    for (name, known) in r.names.iter().zip(&r.known) {
        let was_found = r.found.contains(name);
        if is_another_folder(obs, name) != Ok(false) {
            sorted.untold.insert(name.clone());
            continue;
        }
        match is_gone(obs, name) {
            Ok(true) if r.kept.contains(name) => sorted.not_as_left.push(name.clone()),
            Ok(true) if is_unfinished(name) && !was_found => {
                sorted.tells.push(Tell::FoundGone(name.clone()));
                sorted.untold.insert(name.clone());
                sorted.found_now.insert(name.clone());
            }
            Ok(true) => {
                if is_unfinished(name) {
                    sorted.tells.push(Tell::StillGone(name.clone()));
                }
                sorted.gone.push(name.clone());
            }
            Ok(false) if is_as_known(obs, name, *known) => {
                if is_unfinished(name) && was_found {
                    sorted.tells.push(Tell::ThereAgain(name.clone()));
                }
                sorted.stayed.push(name.clone());
            }
            Ok(false) => sorted.not_as_left.push(name.clone()),
            Err(_) => {
                sorted.untold.insert(name.clone());
            }
        }
    }
    sorted
}

/// Returns what an Undo from the trash whose log says `records` had put back when Glimt was
/// killed: the files whose step is done, or that are in a folder whose step is done, each with
/// its marks and stars, and their places in the collection shown. A step without its `done`
/// counts as done too when its path holds a file with the fingerprint it had in the trash, which
/// came back in the moment between its rename and its record (the rule of a Move). Any other step
/// not done keeps no marks: its file is still in the trash, or another file has its name.
/// Returns why not when a step's path cannot be told (`is_gone`).
pub(crate) fn back_from_the_trash(
    records: &[Record],
    obs: &Observations,
) -> Decided<BackFromTrash> {
    let (mut paths, mut done, mut marks, mut places) =
        (Vec::new(), HashSet::new(), Vec::new(), Vec::new());
    let mut coming = Vec::new();
    let mut collection = None;
    for record in records {
        match record {
            Record::Shown(c) => collection = Some(c.clone()),
            Record::Step { from, .. } => paths.push(from.clone()),
            Record::Done(number) => {
                done.insert(paths[*number].clone());
            }
            Record::Marks {
                number,
                bits,
                stars,
            } => marks.push((*number, (*bits, *stars))),
            Record::Place { number, index } => places.push((*index, *number)),
            Record::Written {
                number,
                fingerprint,
            } => coming.push((*number, *fingerprint)),
            _ => {}
        }
    }
    for (number, fingerprint) in coming {
        let path = &paths[number];
        if !done.contains(path) && !is_gone(obs, path)? && is_as_known(obs, path, Some(fingerprint))
        {
            done.insert(path.clone());
        }
    }
    let is_back = |path: &PathBuf| path.ancestors().any(|a| done.contains(a));
    let places = (places.into_iter())
        .map(|(index, number)| (index, paths[number].clone()))
        .filter(|(_, path)| is_back(path))
        .collect();
    let marks = (marks.into_iter())
        .map(|(number, value)| (paths[number].clone(), value))
        .filter(|(path, _)| is_back(path))
        .collect();
    Ok(BackFromTrash {
        marks,
        places,
        collection,
    })
}

/// Returns what a Rename Folder or a Move Folder to Trash whose log says `records` had done when
/// Glimt was killed. The folder counts as still where it was when its old path holds the folder
/// noted (`holds_the_folder`). A renamed folder counts as renamed only when its old path is
/// surely free and its new one holds it. A new path that cannot be told, as on a stick not
/// mounted at this start, keeps the log. A trashed folder counts as trashed when its old path
/// holds another folder, or is free in a folder that surely is there (`is_gone`).
pub(crate) fn folder_done(records: &[Record], obs: &Observations) -> Decided<FolderDone> {
    let (mut tree, mut collection) = (None, None);
    for record in records {
        match record {
            Record::Tree { from, to } => tree = Some((from.clone(), to.clone())),
            Record::Shown(c) => collection = Some(c.clone()),
            _ => {}
        }
    }
    let Some((from, to)) = tree else {
        return Ok(FolderDone::NothingToFollow);
    };
    let noted = obs.noted.get(&from);
    if holds_the_folder(obs, noted, &from)? {
        return Ok(FolderDone::NothingToFollow);
    }
    Ok(match to {
        Some(_) if !is_gone(obs, &from)? => FolderDone::NothingToFollow,
        Some(to) if holds_the_folder(obs, noted, &to)? => FolderDone::Renamed(from, to),
        Some(to) => {
            // A stick not mounted at this start would else lose the marks of every file under
            // the folder.
            is_gone(obs, &to)?;
            FolderDone::NothingToFollow
        }
        None => {
            // A free path counts only in a folder that surely is there. A stick or share not
            // mounted at this start would else have every file under the folder taken for
            // trashed.
            is_gone(obs, &from)?;
            FolderDone::Trashed {
                folder: from,
                collection,
            }
        }
    })
}

/// Returns whether the folder that the log noted, `noted`, is at `path` (`folder_of`). A folder of
/// another card at the same path, or of a disk that cannot be told, cannot tell, and neither can a
/// failed look at the name or at the folder. Where the log noted nothing, or the file system gives
/// no inode, any folder counts.
fn holds_the_folder(obs: &Observations, noted: Option<&FolderNoted>, path: &Path) -> Decided<bool> {
    match obs.name(path) {
        Seen::CannotTell => return Err(Why::LookFailed),
        Seen::Gone => return Ok(false),
        Seen::There(_) => {}
    }
    // The device, the inode and the disk all come from the one look at the folder, so that a
    // stick pulled out between two looks cannot pair one folder's inode with another's disk.
    let seen = obs.folder(path);
    match (&seen.key, noted) {
        (KeySeen::Failed, _) if !seen.is_gone => Err(Why::LookFailed),
        (
            KeySeen::Key {
                device,
                inode,
                disk,
            },
            Some(noted),
        ) if seen.is_dir => {
            let key = (*device, Some(*inode));
            let boot = obs.boot.as_deref();
            // Another card at the path does not tell where the folder noted went.
            if !disk_of(path, noted, key, disk, boot).answer()? {
                return Err(Why::AnotherDisk);
            }
            folder_of(path, noted, key, disk, boot).answer()
        }
        _ => Ok(seen.is_dir),
    }
}

/// Returns whether a start can put right the Rename Many whose plan renames `moves`: every name
/// can be told about (`is_gone`). A name in a folder not mounted, or at an empty mount point,
/// keeps the plan, and the user is asked again later. A plan of Glimt 0.92, which noted no
/// folders, is read by the names alone.
pub(crate) fn plan_can_be_put_right(
    moves: &[(PathBuf, PathBuf)],
    obs: &Observations,
) -> Decided<()> {
    if obs.noted.is_empty() {
        return Ok(());
    }
    for name in moves.iter().flat_map(|(a, b)| [a, b]) {
        is_gone(obs, name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fingerprint of size `size` and time `time` seconds.
    fn print(size: u64, time: i64) -> FileFingerprint {
        (Some((1, 9)), size, (time, 0))
    }

    /// The observations of a log that noted the folder /c on device 5, inode `inode`, with
    /// `noted_volume` and `noted_medium`, where /c is seen as `key` and the names as `names`.
    fn on_card(
        noted: (u64, Option<&str>, Option<&str>),
        key: KeySeen,
        names: &[(&str, Seen)],
    ) -> Observations {
        let (inode, volume, medium) = noted;
        let folder = PathBuf::from("/c");
        let noted = FolderNoted {
            noted_device: 5,
            noted_inode: inode,
            noted_volume: volume.map(OsString::from),
            noted_medium: medium.map(String::from),
            noted_folder: folder.clone(),
            noted_in_file_system: None,
        };
        let seen = FolderSeen {
            is_gone: key == KeySeen::Failed,
            is_dir: key != KeySeen::Failed,
            key,
        };
        let root = FolderSeen {
            is_gone: false,
            is_dir: true,
            key: KeySeen::Key {
                device: 5,
                inode: 2,
                disk: DiskNow::default(),
            },
        };
        Observations {
            noted: HashMap::from([(folder.clone(), noted)]),
            folders: HashMap::from([(folder, seen), (PathBuf::from("/"), root)]),
            names: (names.iter())
                .map(|(n, s)| (PathBuf::from(n), *s))
                .collect(),
            boot: Some("b007".into()),
        }
    }

    /// The key of /c on `device` with `inode`, on a disk seen as `disk`.
    fn at(device: u64, inode: u64, disk: DiskNow) -> KeySeen {
        KeySeen::Key {
            device,
            inode,
            disk,
        }
    }

    /// A block device's disk now, with `volume` found and still on its device, and `medium`.
    fn card(volume: Option<&str>, medium: Option<&str>, are_volumes_readable: bool) -> DiskNow {
        DiskNow {
            volume: volume.map(OsString::from),
            is_volume_still_on_device: volume.is_some(),
            medium: medium.map(String::from),
            are_volumes_readable,
            is_on_a_block_device: true,
            ..Default::default()
        }
    }

    /// Each row: what the log noted of /c, what is seen of /c and of /c/a.png, and whether the
    /// start takes /c/a.png for gone, with the risk the row covers.
    #[test]
    fn a_start_decides_whether_a_name_is_gone() {
        let gone = &[("/c/a.png", Seen::Gone)];
        let rows: Vec<(&str, Observations, Decided<bool>)> = vec![
            (
                "a file there is never taken for gone, whatever the disk",
                on_card(
                    (7, None, None),
                    KeySeen::Failed,
                    &[("/c/a.png", Seen::There(print(1, 1)))],
                ),
                Ok(false),
            ),
            (
                "a name gone on the card noted, by its volume, lets its marks go",
                on_card(
                    (0, Some("V1"), None),
                    at(5, 3, card(Some("V1"), None, true)),
                    gone,
                ),
                Ok(true),
            ),
            (
                "another card at the same path (another volume) keeps the marks",
                on_card(
                    (0, Some("V1"), None),
                    at(5, 3, card(Some("V2"), None, true)),
                    gone,
                ),
                Err(Why::AnotherDisk),
            ),
            (
                "a clone with the volume noted but another medium in this boot waits (limit)",
                on_card(
                    (0, Some("V1"), Some("b007:1")),
                    at(5, 3, card(Some("V1"), Some("b007:2"), true)),
                    gone,
                ),
                Err(Why::AnotherMediumInThisBoot),
            ),
            (
                "an ext4 stick at inode 2 in the sandbox with another medium waits (#314)",
                on_card(
                    (2, None, Some("b007:1")),
                    at(5, 2, card(None, Some("b007:2"), false)),
                    gone,
                ),
                Err(Why::AnotherMediumInThisBoot),
            ),
            (
                "after a restart, no volumes and no lasting inode: the device alone tells (limit)",
                on_card(
                    (0, None, Some("b001:1")),
                    at(5, 3, card(None, None, false)),
                    gone,
                ),
                Ok(true),
            ),
            (
                "after a restart on another device number, the device does not tell",
                on_card(
                    (0, None, Some("b001:1")),
                    at(6, 3, card(None, None, false)),
                    gone,
                ),
                Err(Why::NotToldApart),
            ),
            (
                "a block device's number alone, in this boot, is not the card noted (#266)",
                on_card((0, None, None), at(5, 3, card(None, None, true)), gone),
                Err(Why::NotToldApart),
            ),
            (
                "a share's device number tells it (it is its own)",
                on_card((0, None, None), at(5, 3, DiskNow::default()), gone),
                Ok(true),
            ),
            (
                "a failed look at the name keeps the marks",
                on_card(
                    (0, None, None),
                    at(5, 3, DiskNow::default()),
                    &[("/c/a.png", Seen::CannotTell)],
                ),
                Err(Why::NotThere),
            ),
            (
                "a folder gone from a disk still there counts as deleted with its files",
                on_card((0, None, None), KeySeen::Failed, gone),
                Ok(true),
            ),
        ];
        for (risk, obs, expected) in rows {
            assert_eq!(is_gone(&obs, Path::new("/c/a.png")), expected, "{risk}");
        }
    }

    /// Each row: a Move's step from /c/a.png to /d/a.png as seen, and whether the file went.
    #[test]
    fn a_start_decides_which_moves_went() {
        let share = at(5, 3, DiskNow::default());
        let step = [(PathBuf::from("/c/a.png"), PathBuf::from("/d/a.png"))];
        let rows: Vec<(&str, Seen, Seen, Decided<usize>, usize)> = vec![
            (
                "a file still at its old name did not go",
                Seen::There(print(1, 1)),
                Seen::Gone,
                Ok(0),
                0,
            ),
            (
                "a file at its new name with its size and time went",
                Seen::Gone,
                Seen::There(print(4, 2)),
                Ok(1),
                0,
            ),
            (
                "another file at the new name does not take the marks, and is told",
                Seen::Gone,
                Seen::There(print(5, 2)),
                Ok(0),
                1,
            ),
            (
                "an old name that cannot be looked at keeps the log",
                Seen::CannotTell,
                Seen::There(print(4, 2)),
                Err(Why::LookFailed),
                0,
            ),
        ];
        for (risk, from, to, expected, told) in rows {
            let mut obs = on_card((0, None, None), share.clone(), &[("/c/a.png", from)]);
            obs.names.insert(PathBuf::from("/d/a.png"), to);
            obs.folders
                .insert(PathBuf::from("/d"), obs.folders[Path::new("/c")].clone());
            let (went, tells) = moves_that_went(&step, &[Some(print(4, 2))], &obs);
            assert_eq!(
                (went.map(|w| w.len()), tells.len()),
                (expected, told),
                "{risk}"
            );
        }
    }
}
