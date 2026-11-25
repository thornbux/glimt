---
name: glimt-fix
description: Use when fixing a bug, an audit finding or a GitHub issue in Glimt, or when writing a fix brief for another agent. The steps a fix in Glimt goes through, from the plan to the report, so that it closes its class once and brings no new bug.
---

# Fixing something in Glimt

AGENTS.md holds the rules; this is the order to apply them in. Every fix of 0.95 to 0.99 that
broke something skipped one of these steps. Read `glimt-classes` alongside this.

## 1. Before any code: the plan

Send the plan and the list of files you will touch first, before you write code (to the main
session, or in your first message to the user). The plan names:

- **The class.** Find it in `glimt-classes`. If the finding is in no class there, name the
  reason it could happen (a rule held in two places, a way around the one function, a state
  the types allow). A fix of the listed form only leaves the class open.
- **Every way in.** List each path that reaches the code you change: its callers (`git grep`),
  the actions and `--remote` commands that end there, the background threads, the moments
  between a start and its answer, the start's repair after a kill, and Undo. Check the fix
  against each, and write the list in the plan.
- **Where the fix goes.** The one shared place of the class, never the call site the finding
  named.
- **The check.** It must fail without the fix: a unit test, or a section in
  `tools/release-tests/tests.sh` for what only a running window or a real share can show.

Check first that the finding still holds against the branch. Report one that does not; do not
"fix" it.

## 2. Fix at the root, in a type or a contract

In this order of preference:

1. **A type that makes the wrong state impossible.**
   - An enum, not bool flags. `exif_index_failed: bool` next to a running index is two flags for
     one state.
   - A newtype that is checked when the data comes in.
   - An error type that only one place can make. For example `StoppedByTheUser`, which no
     share's EINTR can take, and `StepError`, whose error number only file_io sees.
2. **A function's contract.** The one function for the job takes what it needs and returns
   what the caller must handle. For example, `escaped_name` taking the path, instead of ten
   callers each writing `file_name().unwrap_or_default()`.
3. **A `source_rules` guard**, only where no type can say it. One example is text for the user,
   which every egui call takes as any `String`. A guard uses the shared matchers
   (`methods_named`, `calls_reaching`, the list of places that reach the user). It follows
   helpers to any depth and excuses an entry by its file and name. A new finder is shown failing
   on a planted case. Three audit rounds of 0.99 found guards that saw one spelling only.

Never hide a cause behind an `if` check, `unwrap_or` or `unwrap_or_default`. Ask instead what
the `None` or the error means, and give that meaning a type or handle it. Keep a pure core
without I/O where the logic has edge cases, so it is tested without a disk.

Never weaken a check, and never add an exception, a setting, an option, a dependency, `unsafe`
or `#[allow]` without the maintainer's yes. Stop and ask instead. Nothing is frozen before 1.0:
a new frozen line at the end of `src/frozen-1.0.txt` is allowed when the maintainer agrees, and a
log left by the last release must still be put right.

## 3. The test

- Add a test only for new logic, a new API or a failure that really happened. Give each one a
  line that says which risk it covers. When behaviour changes on purpose, change the existing
  test instead of adding one.
- Show that it fails without the fix: take the fix out, run the test alone through
  `tools/check.sh "cargo test -q --locked --profile quick <name>"`, see it fail, and put the fix
  back. "Fails by reasoning" is not shown, so say so in the report.
- A test waits for the event it needs (a file there, an answer in a channel, a step done), never
  for a time. A sleep or a longer timeout is no fix of a flaky test. Of the three flaky tests of
  0.99, one hid a real bug.
- Write a release-test section with `glimt-classes` § Tests in mind. It must fail on the last
  release, so put `note "old: …"` in its `else` branch and check that the old value differs.
  Give each helper function a name that no other section uses, since `run.sh` refuses a shared
  one. Check the section with `bash -n`.

## 4. Before the commit

- Run `tools/check.sh` in your worktree. Read its exit status directly, never through a pipe,
  then run `git diff --quiet` (check.sh runs cargo fmt, and a format change must be amended in).
  Done means green cargo test, clippy `--all-targets` and fmt, in both builds.
- One fix or class per commit. Each commit passes on its own. "Fixes #N" goes in the message.
- Add each new UI text in all 14 `po/*.po` files: `no_nb.po` plain, the others with `#, fuzzy`.
  Then run `python3 tools/pot.py`.
- Give a user-visible fix a CHANGELOG line under `[Unreleased]`, in a commit of its own.
- Do no clean-up outside your task. Clean-up before a release is a round of its own.
- Fixers do not run `tools/release-tests/run.sh` (it belongs to the main session). Name, in your
  report, every release-test section that runs the code you changed, so that the next full run
  is read for them. In 0.99 a trash fix passed its unit tests and broke three release checks of
  the trash.

## 5. The report

Keep it to at most 20 lines:

- the branch;
- the commits, with their check results;
- how each test was shown to fail;
- the ways in you checked;
- the findings that did not hold;
- the decisions the maintainer must confirm;
- what was left out and why;
- the release-test sections to read.

A check not run is reported as not run. Go through the list of § 2 to § 4 point by point.
