# One log for the jobs that change files: a design note

Status: a proposal for the maintainer, rewritten after AGENTS.md (489c700a) made the files
Glimt owns frozen from 1.0, not from 0.76. No code has been written. It answers the open items of
#248, #235 (Undo from the trash), #224, #176, #179 and #225 point 1, which all wait for a
decision on the format of what Glimt writes down before it changes files.

## 1. What there is now

Three jobs write down what they do before they do it, each in a format of its own, in a folder
of its own under `$XDG_STATE_HOME/glimt/`:

| Job | Folder | The file | Beside it |
|---|---|---|---|
| Rename Many | `renames/` | `counted\0`, then the pairs `from\0to\0`, then one `\0` for each rename done | `files/NAME`: one fingerprint line for each step |
| Move | `moves/` | the pairs `from\0to\0` | `files/NAME`: one fingerprint line for each pair |
| Delete, Move to Trash | `deletes/` | the names `path\0` | `files/NAME`, `collections/NAME`, `aside/NAME`, `kept/NAME` |

Each file is named `INSTANCE-NANOS`, is locked with a flock while its job runs
(`open_if_unlocked`), and is read at the next start by every window that finds it unlocked
(`cut_short_plans`, `moves_cut_short`, `deletes_cut_short`). Every name above is pinned in
`src/frozen-1.0.txt`, with `lock_kind … flock` lines for each.

The formats have no header and no version. A reader knows a file's meaning only by the folder it
is in. That is why each addition since 0.81 went into a new folder beside the logs (`files/`,
`collections/`, `aside/`, `kept/`): an older Glimt reads every file beside the logs as a log, and
a new kind of line inside a log would be read by it as a path.

## 2. What the open issues need from a log

- **#248, Rename Folder and Move Folder to Trash.** A record that stands for a whole folder: its
  old and new path. At the next start, the marks and collection entries of every file under the
  old path follow it (a prefix of the path), or are forgotten for a folder that went to the
  trash. An older Glimt must never read that record as one file, because it would miss the files
  inside.
- **#235, Undo from the trash.** A Move to Trash takes the files' marks out of the marks file,
  and Undo keeps them, and the files' places in the collection shown, only in memory. A kill
  during the Undo loses them. The log needs: each file's original name, where it is in the trash
  (the trash's top by its own name and the name of its `.trashinfo`, never a `/proc/self/fd/N/…`
  path), its fingerprint, its marks and stars, and its place in the collection. With these, the
  next start can give the marks back to each file that is back at its name, by its fingerprint.
  The same records also make an Undo possible after a restart, if the maintainer wants that
  (decision 4).
- **#224, an older Glimt reads a kept delete log as a kill's.** A log that a later start must
  read differently must not be found by an older Glimt at all. Nothing written inside the
  existing folders can do this, because 0.85 reads every file there.
- **#225 point 1, a kill between a failed rename and the kept line.** The log must tell "this
  photo was deleted" from "this photo's delete never got that far". Today a name in the log
  means "may have gone", and a name not at its place at the start means "gone". The log needs a
  record written after each step is done, so that a start keeps the hidden sidecar of a step it
  has no record of, and says so, rather than removing it.
- **#179, another card at the same mount path.** The log needs the (device, inode) of each folder
  its files are in, as seen when the log was written. A start that finds another folder at that
  path keeps the marks and keeps the log for a later start.
- **#176, `done_after_all` takes a NotFound as done.** The fix lives in `file_io`, not in the log,
  but the log gives it what it lacks: the fingerprint of the file at the moment of the step.
  A Move then counts as done only when the new name holds a file with that fingerprint. For a
  delete for good, nothing is left to compare, so a NotFound after a lost answer keeps the
  sidecars aside (a `kept` state) rather than deleting them. The log narrows the class; it does
  not close the delete case on its own.

## 3. The plan in short

Before 1.0 a format Glimt owns may be taken away, as long as a log that a kill left behind is put
right once by the release that drops its format (AGENTS.md, *Frozen in 1.x*). So:

- **One new format** in a new folder `$XDG_STATE_HOME/glimt/jobs/`, one log for each job, with a
  header, a version and typed records. Every later need is a new record kind, not a new folder.
- **0.93 writes only the new format.** Nothing writes two logs that could disagree.
- **0.93 puts the old logs right once.** At its start it reads every unlocked log in `renames/`,
  `moves/` and `deletes/` with today's code, unchanged: it finishes or rolls back each as today's
  start does, keeping every mark and file, and removes the log and its lists. A log it cannot
  finish (a `kept` one, a stick not plugged in) it writes anew as a `jobs/` log and then removes
  the old one (section 5). That also closes #224: no log stays where 0.85 reads it.
- **The old reader goes later.** The code that reads the three old formats, and their tests and
  frozen lines, stay in 0.93 and 0.94 and go in 0.95 (decision 1 in section 10). A user who skips from
  0.92 or older straight to 0.95 or later, and whose last Glimt was killed during a Rename Many,
  Move or delete, must start 0.93 or 0.94 once first. 0.95 does not read the old folders, but if
  it finds one that is not empty it tells once which release to start, and leaves it alone.
  Nobody else has anything to do.

## 4. The format

One file for each job: `jobs/INSTANCE-NANOS`, named as the plans are now. Everything the job
needs is in that one file, with no lists beside it, so a log is written, synced and removed as
one thing, and there are no orphan lists to clean up (`remove_orphan_lists`).

A path can hold any byte but NUL, so every field ends in a NUL byte, and an empty field (two
NULs in a row) ends a record. No path is empty, so the end is never ambiguous. A record is:

```
KIND\0 FIELD\0 FIELD\0 … \0
```

The first record is the header, and a file without it is no log:

```
glimt-job\0 1\0 KIND-OF-JOB\0 UNIX-NANOS\0 INSTANCE\0 \0
```

`1` is the format's version. A reader takes a log of a version it knows. One of a higher version
it leaves alone and keeps on disk, and tells the user once that a newer Glimt left work to finish
(decision 6).
A record kind it does not know, in a version it knows, it passes over, so a later Glimt can add
kinds within version 1 when an older reader may safely ignore them. A record that changes the
meaning of others raises the version.

The kinds of job: `rename`, `move`, `delete`, `trash`, `undo-move`, `undo-trash`,
`rename-folder`, `trash-folder`.

The record kinds:

| Record | Fields | Written | For |
|---|---|---|---|
| `folder` | path, device, inode (`0` where the file system keeps none, FAT and exFAT) | before the first step in that folder, also the folder a file or folder goes to | #179, an empty mount point |
| `shown` | the collection's path | at the start | the collection shown, now `collections/` |
| `step` | number, from, to or `-`, fingerprint or `-` | before the step | every job |
| `done` | number | after the step is done | #225, Rename Many's count |
| `tree` | from, to or `-` | before a folder is renamed or trashed | #248 |
| `trashed` | number, trash top or `home`, `.trashinfo` name | after the file is in the trash | #235 |
| `marks` | number, bits, stars | before the marks file forgets them | #235 |
| `place` | number, index in the collection | with `marks` | #235 |
| `aside` | number, sidecar, hidden name | synced before the sidecar is renamed | now `aside/` |
| `kept` | number | when a step must wait for a later start | now `kept/` |
| `old-log` | the path of the log it was written from | when a log is written anew | section 5 |
| `found` | number | when a start first finds a delete for good's step without `done` gone | decision 5 |
| `written` | number, fingerprint | when Glimt writes a step's file anew in its place after the step was written down | Rename Many's `note_written_anew` |

An `old-log` record is part of version 1. It names the log that this one was written from: an
old log of 0.92 or older (decision 2), or a job log that a start wrote anew with only its steps
left. A start removes the named log unread when a kill left it beside the new one. It does not
change how the new log's own steps are read. A step written from an old log gets a `done`
record, because in the old format a name meant a file that may have gone, and a name surely
gone counted as deleted. So a step of an old log is read by the rules of 0.92, and the rule of
decision 5 for a step without `done` never applies to it.

A `found` record says that a start found the step of a delete for good, which has no `done`,
with its photo gone, kept its marks and its hidden sidecar, and told so. It is written, synced,
in the log written anew for the later start. A later start that finds the photo still surely
gone (NotFound) finishes the step: it forgets the marks, deletes the hidden sidecar, and tells
once. One that finds the photo there as the log knew it gives the sidecar its name back, never
over a file, keeps the marks, and tells once. A look that fails keeps everything.

A `step`'s number ties the later records to it. A fingerprint is the text of `known_line`
without its line break, so one parser reads it in old lists and new logs.

### What is written, and when it reaches the disk

The rule is the one each job follows now. A kill of Glimt keeps everything written, as the kernel
has it, and only a power cut needs a sync, so a sync is paid only where a power cut would leave
a wrong result:

- **The header and the `folder` and `shown` records** are written and synced, with the `jobs/`
  folder, before the first step, as a plan is now. A log that is not on disk is no reason to
  stop a Move or a delete (a full disk is cleared by them), so for those jobs a failed write is
  told and the job goes on, as now. Rename Many and the folder jobs do not start without it.
- **`step` and `done`** are written as each step goes, in one write for each chunk of steps, and
  not synced, except the middle of a swap in Rename Many, which is synced before and after as
  `count_middle_of_swap_done` does now.
- **`aside`** is synced, with the folders that hold it, before the sidecar is renamed, as now.
- **`marks` and `place`** are written before the marks writer takes them out of the file, and the
  log is synced before that write. Losing them to a power cut would lose the marks for good.
- **The log is removed** only after the marks file and the collection have been written with the
  result (`remove_after_the_writes`), and for Rename Many after the touched folders are synced.

### What the next start does after a kill

A log no window holds locked was cut short. For each step, the start looks at the records it has:

- **`step` without `done`:** the step may or may not have been done. Rename Many repairs by names
  and by its count, as now. A Move takes the file as moved only if the new name holds the file
  with the step's fingerprint. A delete for good keeps the hidden sidecar and its photo's marks, keeps the
  log, and tells (decision 5, and #225 point 1: a kill before the kept record no longer looks
  like a delete that happened).
- **`step` with `done`:** the result is followed, as a finished job would have: marks follow or
  are forgotten, the collection follows, a hidden sidecar of a deleted photo goes.
- **A `folder` record whose device and inode differ** from the folder at that path now: every
  step in it is left for a later start, and the log stays (#179).
- **A step in a folder of another device than its `folder` record noted, or with no `folder`
  record in a log that has some:** the start cannot tell, as an empty mount point (`/mnt/nas`
  while its share is not mounted) is a folder too. The step is left for a later start, and the
  log stays. A stick plugged in again with another device number waits the same way: its marks
  are kept, not lost. A log with no `folder` record at all (Glimt 0.92's, or one written anew
  from it) is read as before. A Rename Many plan notes the folders it renames in too, and a
  name there that the start cannot tell about keeps the plan, with its files counted as left.
- **`tree`:** the marks and collection entries under the old path follow the new one, or are
  forgotten for a trashed folder, once the start sees the folder at its new place by its inode
  (#248).
- **`undo-trash` with `trashed` and `marks`:** each file that is back at its original name with
  its fingerprint gets its marks and its place back. One still in the trash keeps its records,
  and the log stays, so a later start can finish (#235). Undo itself is not offered after a
  restart for now (decision 4).
- **A log the start cannot finish** (a stick not plugged in, a `kept` step) is written anew with
  only its unfinished steps, as `untold_names` does now, so a later start does not look again at
  every file.

## 5. Locks, and two windows of two versions

**The lock names and kinds.** Nothing pins them before 1.0, so the new format may pick its own.
It needs no lock file of its own: each log is its own lock, a flock taken through
`open_if_unlocked` while its job runs, as the plans and logs are locked now. The `files/.lock`
that guards the lists beside the old logs (`journal::lists_lock`) has nothing to guard, because a
new log has no lists. So the new format keeps the kind (flock, which is the only kind that works
here) and takes no names from the old ones. The only new pinned names are the folder and the
kind of its lock.

**0.93 reading an old log** takes the old locks, the log's flock and `files/.lock`, exactly as
0.92 does. So a 0.92 and a 0.93 window never both repair one old log.

**A 0.92 and a 0.93 window at once.** This happens for the moments of an upgrade, or when a
user starts an older build by hand:

- A 0.92 window that runs a job writes an old log, locked. 0.93 passes over a locked log, and
  repairs it at its next start only when it was cut short. So 0.93 must look in the old folders
  at every start, not only the first, until the release that drops the reader.
- A 0.92 window started after 0.93 never opens `jobs/`. A kill of a 0.93 job is then repaired
  only when a 0.93 or later starts. Until then 0.92 shows the marks at the old names, as every
  version before 0.81 did after a kill. No file and no mark is lost: the log waits on disk.
- 0.93's rewrite of an old log into `jobs/` is done under the old log's lock. The new log is
  synced before the old one goes. A kill in between leaves both, and the next start reads them
  as one, because the new log names the old one in its `old-log` record and the old one is
  dropped.
- Two 0.93 windows: one lock for each log, and one start repairs it.

This is acceptable before 1.0. The worst case is a repair that waits for the next new start, and
the old reader stays for two releases, so an upgrade in the normal order loses nothing.

## 6. Untrusted content, limits and scale

A log is Glimt's own file, but it is in a folder another program could write, so it is read as
untrusted text:

- It is opened only through `open_if_unlocked`, only when it is a regular file (no pipe, no link),
  and read with a bound. The bound is `MAX_DECODE_BYTES` (512 MB) as now, but the parser reads
  records as a stream from a buffered reader, so it never holds the whole file.
- A record cut off at the end (no closing empty field) is left out, as `parse_names` leaves out a
  last name with no NUL, because its path may end at a `/`.
- Every path must be absolute and have no `..` part. A step whose number is not one the log has
  is passed over. A field that does not parse makes its record be passed over, never the start
  stop.
- A `trashed` record names a trash by its top's path and a `.trashinfo` name with no `/`, and the
  trash is opened through `open_folder_not_through_link` as now, so a log cannot send a restore
  outside a trash.
- `survives_broken_job_logs` feeds the parser `broken(sample)` of a log with every record kind,
  and the weekly fuzz run takes it too. The old parsers keep their tests.

At 500,000 files, a log holds about 150 bytes for each step, so 75 MB, and a Move to Trash with
marks about 100 MB. That is within the bound, and it is the size the three logs and their lists
have together now. Records are written in one write for each chunk (the deletes' chunk of 100),
and the start reads each log once, as a stream, and looks at each file once. Nothing is O(n²): a
step's later records find it by its number in a `Vec`, not by a search.

## 7. The frozen file

Taken out of `src/frozen-1.0.txt` with the maintainer's go, in 0.95 together with the reader
(decision 1 in section 10), not in 0.93, since 0.93 still reads and locks them:

- `file` lines of `renames`, `renames/files`, `renames/files/.lock`, `moves`, `moves/files`,
  `deletes`, `deletes/files`, `deletes/collections`, `deletes/aside` and `deletes/kept`;
- the `list of …` lines of the three, and the `collection of`, `aside of` and `kept of` lines;
- `lock_kind plan flock`, `lock_kind move log flock` and `lock_kind delete log flock`.
  (`lock_kind lock file flock` stays: it is `file_io::open_and_lock`, used by other files.)

Added at the end in 0.93:

- `file $XDG_STATE_HOME/glimt/jobs`;
- `lock_kind job log flock`;
- a new line kind, `job_record`, for the header's word `glimt-job`, the version `1`, each kind of
  job and each record kind, so that a test fails when one is renamed (decision 7).

## 8. How it would be done

0.93, each a branch of its own:

1. The format, its writer, its parser and its fuzz test, with no job using it yet.
2. Move and Delete write the new log. The start reads old and new, and puts the old right once.
3. Rename Many writes the new log.
4. #225 point 1, #179 and #176, each from the records it needs.
5. #248, the folder jobs, and #235, the records that keep the trash Undo's marks.

0.95: the old reader, its tests and its frozen lines go, and the start tells once about an old
folder that is not empty.

## 9. Decided

1. One new format in `jobs/`. The old formats go.
2. The new Glimt writes only the new format.
3. 0.93 puts every old log right once at start, then removes it; the old reader goes before 1.0.
4. No Undo across a restart for now. The records cover a kill during an Undo (#235).
5. A delete for good with a `step` and no `done`: keep the hidden sidecar and the marks, keep
   the log, and tell.
6. A log of a higher version: leave it, and tell once.
7. The record kinds are pinned by a new line kind, `job_record`.
8. #176 is only partly closed for a delete for good: a NotFound after a lost answer keeps the
   sidecars aside. Nothing on disk is left to tell a delete from another program's move.

## 10. Decided by the maintainer (2026-11-19)

1. **Which release drops the old reader?** Decided: 0.95, so that two releases (0.93 and
   0.94) put old logs right and a user who updates now and then still meets one of them. Any
   0.x before 1.0 works; later costs only the old code's upkeep.
2. **Does 0.93 rewrite a log it cannot finish into `jobs/` (section 3), or leave it in the old
   folder for 0.94 to try again?** Decided: rewrite. It closes #224, and 0.95 then has no
   old log left that it must still read.
3. **Does 0.95 remove an old folder it finds empty?** Decided: yes, an empty folder of
   Glimt's own with `file_io::remove_own`, and one that is not empty is left and told about.
