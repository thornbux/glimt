---
name: glimt-release-test
description: Use when writing or changing a section of tools/release-tests/tests.sh, when a fix in Glimt's window code or on a real share needs a check a unit test cannot reach, when a release test is flaky, or when reading a release-test run. How a section is built so that it fails on the last release, waits for events, and runs in any part.
---

# Writing a release-test section

`tools/release-tests/run.sh [NEW] [OLD]` builds both versions in Docker and splits
`tests.sh` into parts. Each part runs in its own container, with its own X display, `/tmp` and
`HOME`. Only the main session runs it. A fixer writes the section, checks it with `bash -n`, and
names it in the report (see `glimt-fix` § 4). docs/RELEASING.md tells the rest.

## When a section is needed

Write one when only a running window or a real environment can show the failure. That covers
painting, keys, clicks, a kill in the middle of work, a full disk, a share that loses its answers
or stalls, real ffmpeg or gio, and the timings. When a unit test (`app_on(dir)`) reaches the fix,
write the unit test instead.

## The shape

```sh
section "what holds, in words a reader checks: the photo kept, its stars on it, told"
for v in new old; do
    as_version $v                 # HOME, XDG_* and BIN of that version; a fresh $H
    d=/tmp/my-case-$v             # the section's own files, made here, never by another section
    mkdir -p $d && blue_photo 640x480 $d/a.jpg
    config delete_to_trash=true   # the settings, written whole
    start $d
    until_listed a.jpg            # wait for the state (the file in the list), never sleep
    key ctrl+z                    # or: menu File "Open With", remote run …, click_text 1 "Label"
    until_file $d/a.jpg 10
    result="$(ls $d | tr '\n' ' ')"
    quit
    if [ $v = new ]; then expect "the photo is back" "$result" "a.jpg "
    else note "old: $result"; fi
done
```

- **A section stands alone.** `run.sh` may start a part at any section. Make everything the
  section needs inside it, or in a function in the head of `tests.sh` that makes it once
  (`print_pictures`). Count on nothing an earlier section made.
- **A helper function has a name of its own.** `run.sh` refuses a run when a section calls a
  function another section defines, and it reads `; saved $saved` as a call of `saved`. In 0.99 a
  new section's `saved()` stopped the whole run. Give the helper a name that names the section's
  thing (`thumbnails_saved`), or define it in the head when several sections need it.
- **Wait for events, not time.** Use `until_ok`, `until_listed`, `until_file`, `until_gone`,
  `until_current`, `until_notice` or `until_error_notice`, and `settled` before a screenshot is
  compared. A loaded machine runs six parts at once and outruns any fixed pause. A wait that
  times out returns false, and the `expect` after it says what is wrong.
- **Read the result from what the user has.** That means the files, the marks file (`$MARKS`), the
  answer of `--remote`, or a colour in a screenshot (`colour`, `matching`, `notice_pixels`). Find
  a widget by its text through `where LABEL` and `click_text`, and use screen coordinates only
  where a click's place is the point.

## It must fail on the last release

A check that passes on the old version shows nothing about the fix. The `else` branch prints
`note "old: …"`. Read that note in the run's log: it must differ from what the new branch
expects. In 0.99 the 8,000×8 video's section printed `old: 2 saved`, so the old release passed
too, and the check did not show the #211 fix.

Before you call a section done, say how it fails on the old version and why. The reason must be
in the code, not a guess. When the old version cannot run the case at all (a new option), say
so, and make the new branch's check strict on its own.

## Making the failure happen

A check of a class that lives in how the system behaves (`glimt-classes` § 2, § 4, § 7) must make
the failure happen for real:

- **`faults=yes`** on its own line in the section gives the part `--privileged`, which brings:
  - `/mnt/full`, a small tmpfs, and `/mnt/disk`, a small ext2, to fill;
  - `/mnt/lossy`, a FUSE pass-through of `/opt/lossy`. Its faults are below.
- **The faults of `/mnt/lossy`:**
  - `lost-*` names: the unlink removes the file but answers EIO, and stat is served from a cache
    for 30 s.
  - `bad-*` files: a read fails past 256 KiB while `/tmp/lossy-eio` is there.
  - A path that starts with a line of `/tmp/lossy-stall` stalls while that line is there.
  - A path that starts with a line of `/tmp/lossy-slow` is read slowly: half a second for a
    folder, and 0.3 s for each read of a file.
- **The two other shares over `/opt/lossy`:**
  - `/mnt/share` is mounted as sshfs, which Glimt takes for a share.
  - `/mnt/gvfs` is mounted as gvfsd-fuse and serves one call at a time, as gvfs does.
- **Show that the fault happened.** Read `/tmp/lossyfs.log`, `/tmp/sharefs.log` and the others:
  `expect "… the share slowed its reads"`. A run where the fault never fired passes for the wrong
  reason.
- **A kill in the middle (`kill -9`).** Make the work slow enough that the kill lands in it.
  Read the photos from `/mnt/lossy` under a `/tmp/lossy-slow` line, wait for a sign that the work
  has started (its log there, a first file moved), then kill. Check "killed in the middle" from
  the state at the kill, as #315 does. Without this the work ends before the kill, and the check
  tests the start of an empty log.

## Flaky sections and tests

A section or unit test that fails now and then is a bug of the test or of Glimt, never noise. Of
the three flaky tests of 0.99, one hid a real bug in Glimt. Find what it waited for by time, or
what two parts shared:

- a name under `/tmp`;
- the shared thumbnail cache;
- a step it raced.

Make it wait for that event. A longer sleep, a retry or a looser `expect` is no fix, and loosening
an `expect` weakens a check, which needs the maintainer's yes.

## Reading a run

- `PASS` and `FAIL` lines, then `N checks failed` and the run's exit status.
- **A `FAIL` in a section your release did not touch:** run that section's case against the old
  version before you blame the environment.
- **`note` lines:** old against new, and the timings. Only `within` (1.5 times the last release)
  stops a release on time or memory.
- **`-- not tested here, by hand: …`** lists what docs/RELEASING.md § By hand must cover.
- The release tests run on the very commit that gets the tag. A change after the run means a new
  run.
