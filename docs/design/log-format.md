# One log for the jobs that change files: a design note

Status: a proposal for the maintainer. No code has been written. It answers the open items of
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
  inside, and that would change a frozen format's meaning.
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

## 3. The alternatives weighed

**A. Keep the three formats, and add a fourth folder for the folder jobs and the trash Undo.**
The smallest change for #248 and #235. It leaves #224 open (old kept logs stay where 0.85 reads
them), it leaves #225 and #179 needing yet another folder beside the delete logs each, and every
new need after that is one more folder. Four formats are four parsers to fuzz and four sets of
start-up rules that can drift apart, which is the "rule held in two places" class.

**B. A versioned header on each existing format.** An older Glimt does not know the header. In a
plan or a log it reads the header as a path and acts on it: a Move log's header would be the
first `from`, and a delete log's header a name whose marks are forgotten. So a header in the
existing folders changes a frozen meaning. Rejected.

**C. One new format in one new folder, read beside the old ones forever. Recommended.**
A new folder `$XDG_STATE_HOME/glimt/jobs/` holds one log for each job, of any kind, in one
format with a header and typed records. An older Glimt never looks in that folder, so it can
neither misread a new log (#224's class) nor be harmed by one. The new Glimt still reads
`renames/`, `moves/` and `deletes/` with the code it has now, forever, and writes there no more.
Every later need is a new record kind, not a new folder.

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
it leaves alone and keeps on disk, and tells the user once that a newer Glimt left work to finish.
A record kind it does not know, in a version it knows, it passes over, so a later Glimt can add
kinds within version 1 when an older reader may safely ignore them. A record that changes the
meaning of others raises the version.

The kinds of job: `rename`, `move`, `delete`, `trash`, `undo-move`, `undo-trash`,
`rename-folder`, `trash-folder`.

The record kinds:

| Record | Fields | Written | For |
|---|---|---|---|
| `folder` | path, device, inode | before the first step in that folder | #179 |
| `shown` | the collection's path | at the start | the collection shown, now `collections/` |
| `step` | number, from, to or `-`, fingerprint or `-` | before the step | every job |
| `done` | number | after the step is done | #225, Rename Many's count |
| `tree` | from, to or `-` | before a folder is renamed or trashed | #248 |
| `trashed` | number, trash top or `home`, `.trashinfo` name | after the file is in the trash | #235 |
| `marks` | number, bits, stars | before the marks file forgets them | #235 |
| `place` | number, index in the collection | with `marks` | #235 |
| `aside` | number, sidecar, hidden name | synced before the sidecar is renamed | now `aside/` |
| `kept` | number | when a step must wait for a later start | now `kept/` |

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
  with the step's fingerprint. A delete keeps a hidden sidecar and its photo's marks, keeps the
  log, and tells (this is #225 point 1: a kill before the kept record no longer looks like a
  delete that happened).
- **`step` with `done`:** the result is followed, as a finished job would have: marks follow or
  are forgotten, the collection follows, a hidden sidecar of a deleted photo goes.
- **A `folder` record whose device and inode differ** from the folder at that path now: every
  step in it is left for a later start, and the log stays (#179).
- **`tree`:** the marks and collection entries under the old path follow the new one, or are
  forgotten for a trashed folder, once the start sees the folder at its new place by its inode
  (#248).
- **`undo-trash` with `trashed` and `marks`:** each file that is back at its original name with
  its fingerprint gets its marks and its place back. One still in the trash keeps its records,
  and the log stays, so a later Undo or start can finish (#235).
- **A log the start cannot finish** (a stick not plugged in, a `kept` step) is written anew with
  only its unfinished steps, as `untold_names` does now, so a later start does not look again at
  every file.

## 5. Two windows, two versions

- **The lock.** Each log is locked with a flock through `open_if_unlocked`, as now, and a new
  `lock_kind job log flock` line is pinned. The lock names and kinds of `renames/`, `moves/` and
  `deletes/` stay as they are, and the new Glimt takes the same locks when it reads an old log,
  so an old and a new window never both repair one log.
- **An old window and a new log.** An older Glimt never opens `jobs/`. It leaves a new log alone,
  which is safe: the worst case is that a kill's repair waits until a new Glimt starts. While it
  waits, an older window may show marks at the old names, as all versions before 0.81 did.
- **A new window and an old log.** Read with today's code, unchanged, forever. Its tests stay.
- **Old kept logs (#224).** A new start that finds a delete log with a `kept/` file may write it
  anew as a `jobs/` log and remove the old one, so that a later 0.85 no longer finds it
  (decision 3). The rewrite is done under the old log's lock, the new log is synced before the
  old one goes, and a kill in between leaves both, which the next start reads as one: a `jobs/`
  log names the old log it came from in a `from-log` record.
- **Two new windows.** Nothing new: one lock for each log, one start repairs it.

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

## 7. What stays frozen, and what is added

Nothing frozen is taken away or changed in meaning:

- `renames/`, `moves/`, `deletes/` and their `files/`, `collections/`, `aside/` and `kept/`
  folders, their `list`, `collection`, `aside`, `kept` and `lock_kind` lines, stay in
  `src/frozen-1.0.txt` and are still read.

Added at the end of `src/frozen-1.0.txt`:

- `file $XDG_STATE_HOME/glimt/jobs`
- `lock_kind job log flock`
- the header's first field `glimt-job` and version `1`, and the record kinds, each as a line,
  so a test fails when one is renamed. A new pinned line kind (`job_record …`) is needed for
  this.

## 8. How it would be done

One minor release, in this order, each a branch of its own:

1. The format, its writer, its parser and its fuzz test, with no job using it yet.
2. Move and Delete write the new log. The start reads both old and new.
3. Rename Many writes the new log.
4. #225 point 1, #179 and #176, each from the records it needs.
5. #248, the folder jobs.
6. #235, the trash Undo, and if wanted (decision 4), Undo after a restart.
7. If wanted (decision 3), the old kept logs rewritten into `jobs/`.

## 9. Decisions only the maintainer can make

1. **One new format in a new folder `jobs/` (C), or another folder for each need (A)?**
   Recommended: C. It closes #224's class for every later log and stops the growth of folders.
2. **Does the new Glimt stop writing the old formats?** Recommended: yes. An older window then
   leaves a kill's repair to the next new start, which is safe, and nothing writes two logs that
   could disagree.
3. **Are old kept delete logs rewritten into `jobs/` at the start (#224)?** Recommended: yes,
   once, under the old log's lock, so that a 0.85 started later can no longer misread them.
4. **Is Undo kept across a restart?** The log makes it possible for Move and Move to Trash, but it
   changes behaviour users know (Undo is now lost when Glimt closes, as in Geeqie, which has no
   Undo). Recommended: no for now. The records cover a kill during the Undo, which is #235. A
   later release can offer it from the same records.
5. **What does a step with no `done` mean for a delete for good (#225 point 1)?** Recommended:
   keep the hidden sidecar and the photo's marks, keep the log, and tell. The cost is a told,
   hidden sidecar the user may have to give back by hand, after a kill at one exact moment.
6. **A log of a higher version than this Glimt knows:** leave it and tell once (recommended), or
   leave it in silence.
7. **A new kind of line in `src/frozen-1.0.txt` (`job_record`)** for the record kinds.
   Recommended: yes, as the record kinds are what a later Glimt must keep reading.
8. **#176 for a delete for good:** accept that the log narrows it (the sidecars are kept aside
   on a NotFound after a lost answer) rather than closing it. Recommended: yes; nothing on disk is
   left to tell a delete from a move by another program at the same instant.
