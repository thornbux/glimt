---
name: glimt-classes
description: Use when fixing, reviewing or auditing Glimt code, or when planning a change to file operations, job logs, shares, decoding, parsing, notices or source_rules. A catalogue of the bug classes Glimt has had, each with the one place that closes it, its check, and what audits found again.
---

# Glimt's bug classes

Audits of 0.53 to 0.99 found the same classes again and again. A fix that closes only the form an
audit listed leaves the class open (AGENTS.md, *Fix the root, once*). Before a change, find each
class it touches below. Use that class's one place and check its "ways in".

Each entry gives:

- **Place:** the shared code that closes the class.
- **Check:** the test or guard that holds it.
- **Seen again:** how it came back after a fix.

## Files on disk

### 1. Gone means only NotFound
A share that dropped for a moment answers a look with an error. Taking that answer for "gone"
loses marks or a sidecar.
- **Place:**
  - `file_io::is_surely_gone`, `is_surely_there`, `is_surely_file`, `is_surely_dir`, `is_link`;
  - `is_followed_to` for a file a record says was moved.
- **Check:** `source_rules::gone_means_not_found`.
- **Seen again:** `metadata(…).is_ok()`, `let Ok(m) = … else`, a look kept in a variable and asked
  its kind later.

### 2. A failed step may have been done (a cached or lost answer)
A share can lose its answer after doing the step, or show a removed name from its cache. Never
remove a name on the belief that another name still holds the file.
- **Place:**
  - Each step returns a `file_io::StepError`. Its number and kind only file_io sees.
  - The questions about a failure are `may_have_been_done`, `found_nothing`, `is_interrupted`
    and `done_after_all`.
  - A second name goes only after `save::same_file_by_opening`.
- **Check:**
  - `steps_are_judged_done_in_one_place`;
  - `steps_judged_by_looks`, whose list `LOOK_ON_ITS_OWN` names each entry by file and name;
  - `no_name_is_removed_on_a_cached_look`.
- **Seen again in 0.99:**
  - EINTR from a share at the moment of Stop was taken for the Stop. It is now
    `StoppedByTheUser`, a type of its own.
  - NFSv3 answered EEXIST for a link it made (#307).
  - After a failed `gio trash`, every file was judged as one, so Undo lost the files that went.
    Release tests caught it. Each file is now judged by its own fact (`trash::left_by_a_program`).
    A look counts only on a disk known to be local, and on a share the file may have gone.

### 3. Still the file that was picked
Another program or window can give the name to another file between the pick and the act.
- **Place:**
  - `ListedFile::check`, in the thread that acts;
  - `PickedFiles` and `still_the_one_picked`;
  - `replace_what_was_read`;
  - `write_picked_name`.
- **Check:** `destructive_acts_check_the_file_is_still_the_one_picked`.
- **Seen again:** Check Images selected its broken files by name (#202). Now each file carries
  a fingerprint.

### 4. A kill between two steps
A file, its sidecar, its marks and its collection entries must end up together. That holds
after a kill, at the next start, and after an Undo cut short.
- **Place:**
  - Every file operation starts a job log (`JobLog::start` through `journal::start_…`).
  - The start's repair (`repair_cut_short`, `leave_names`) and the window's Undo follow one rule
    for what belongs to a photo (`journal::unplanned_sidecar_follows`,
    `unplanned_sidecars_follow_their_photos`).
  - Marks set during the job are recorded (`MarksAtNewNames`) and merged with the photo's own
    marks (`with_changes_made_meanwhile`). They never replace them.
- **Check:**
  - `every_operation_in_the_contract_keeps_a_job_log`;
  - the contract tests in file_ops.rs;
  - the release-test sections that kill with `kill -9` in the middle of the work. They read from
    `/mnt/lossy` with lossy-slow, so the kill lands midway.
- **Seen again in 0.99:**
  - The repair ran the sidecar rule but `leave_names` did not.
  - A mark set during the job wiped the photo's stars.
  - A rule held in two places, the start and the Undo, is this class's usual root.

### 5. Which volume a log belongs to
- **Place:** `job_decisions::disk_of` (looks in `journal::disk_now`) and the records `medium` and `in-file-system`.
- **Seen again:**
  - cards cloned with `dd`;
  - two ext4 sticks at inode 2;
  - a FAT volume that is never found again;
  - a bind mount not mounted.

  A card plugged in again within the same boot waits for the next boot, by the maintainer's
  choice.

## Threads and the window

### 6. The window thread never waits for the disk
- **Place:** background threads that report over `Receiving` / `AnswerSender`.
- **Check:** `the_window_thread_never_waits_for_the_disk`.

### 7. Background reads on a share go through the gate
gvfs serves one call at a time, so a reader nobody waits for must wait its turn.
- **Place:** `listing::ReadsFirst` and `before_work_nobody_waits_for`.
- **Check:** `background_reads_on_a_share_go_through_the_gate`, which reads each thread's work in
  order, helpers included.
- **Seen again in 0.99:**
  - Making a `ReadsFirst` counted as waiting at the gate.
  - A helper read before it waited.
  - The watcher's first modification times and the tools menu read before the gate.
  - A read of `/proc/self/mountinfo` is no read of the user's files.

### 8. An answer is taken even without a frame
- **Place:** `receive_answers`, and `AnswerSender`, which keeps the busy pointer on until the
  answer is handled.
- **Check:** `answers_keep_the_busy_pointer`, plus the guard on `poll_background`.
- **Seen again in 0.99:**
  - Watcher changes were taken only in a drawn frame, so they piled up while the window was
    minimized.
  - After the fix, a listing forgot to clear the new state and read the whole tree again.

  When a fix adds state, every place that resets the old state must reset the new one too.

## What the user is told

### 9. A failure is told, never swallowed or hidden
- **Place:**
  - `save_failed`, `tell_failed_write`, `NoticeSender::send_failed_write`;
  - `Failure` with `give_error`;
  - `Notice::keep_until_drawn`, so that a failed write stays under later notices.
- **Check:**
  - `write_results_are_not_dropped`;
  - `failures_stay_failures`;
  - `notices_are_given_through_their_setter`.
- **Seen again in 0.99:**
  - An ordinary notice in the same frame as a failed write was dropped (#301).
  - A close's notice stayed above a failure that forgot the close. A flaky test found it.

### 10. Names from the disk are shown escaped
A control or direction character in a file name can mislead the user, for example in a delete
question.
- **Place:** `format::escaped_name`, through `name_shown` and `file_io::shown_path`.
- **Check:**
  - `names_are_shown_through_one_function`;
  - `names_given_to_the_user_raw`, which reads one shared list of the places where text
    reaches the user.
- **Seen again:**
  - `to_string_lossy` given straight to a notice, a print, `speak_*` or a title.
  - A path passed to a program as its lossy text.
- **For 1.x:** a type for text shown to the user.

## Input from outside

### 11. Every file, name, sidecar and socket message is untrusted
- **Place:**
  - `decode::read_limited`, `read_text`, `read_line_limited`;
  - a path checked to stay where it should;
  - a program given its input as arguments.
- **Check:** each parser's `survives_broken_*` test, and clippy's `disallowed-methods`.

### 12. XMP is read where its Description says
- **Place:** `xmp::top_level_properties` and `not_element_length`. A prefix is read as bound
  where its property is written.
- **Check:**
  - `xmp_properties_are_found_at_the_top_level`;
  - `packet_walks_pass_over_markup_that_is_no_element`.
- **Seen again in 0.99:**
  - prefixes looked up in the whole packet (#173);
  - an Alt item without a language next to `x-default` (#181);
  - a tag named like a group word, `group:x` (#215).

### 13. A value on a line of Glimt's own files
- **Place:** `config::fits_on_a_line`; a name read from a file has its control characters
  replaced.
- **Check:** `every_file_of_lines_keeps_line_breaks_out`.

### 14. A decode is counted before its pixels exist
- **Place:**
  - `decode::from_decoder`, `from_memory`, `scaled`;
  - `DecodeInput`;
  - `fits_decode_limit`.

  `from_decoder` refuses a `Cursor`, and bytes in memory go through `from_memory`.
- **Seen again in 0.99:** ffmpeg was asked for a side of 0 (#211). The filter now works out both
  sides itself; this was checked against a real ffmpeg.

## The rules' own checks

### 15. A guard that sees one spelling
A `source_rules` guard matches text, so another spelling, a helper, an import or a name in
another file slips past it. This was the class of three audit rounds in 0.99.
- **Fix:** first ask whether a type can hold the rule instead (see `glimt-fix` § 2). If it
  cannot, build on the shared matchers (`methods_named`, `calls_reaching`, `calls_by_name`, the
  list of places that reach the user). Follow helpers to any depth, excuse by file and name, and
  plant a case that fails.
- **Never:** a guard that finds less than before.

### 16. Tests that test nothing, or test time
- **A release-test section that passes on the last release:** the 8,000×8 video of 0.99 passed
  on 0.98 too. Read its `old:` note.
- **Helper functions of `tests.sh` that two sections share:** `run.sh` refuses the run.
- **A test that waits for a time instead of an event:** of the three flaky tests of 0.99, two
  were races of the test and one hid a real bug.

## An action acts on the files the user picked
`Action::target`, `App::targets` (`None` stops the action) and `select_for_action`. A new view of
the list gets everything AGENTS.md lists under "A new way to show the list".
