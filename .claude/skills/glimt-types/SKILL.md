---
name: glimt-types
description: Use when designing or changing Rust types, function signatures, error types or state in Glimt, when a fix tempts an if-check, unwrap_or or a bool flag, or when deciding between a type and a source_rules guard. Glimt's own patterns for making wrong states impossible, with examples from the code.
---

# Types that shut out wrong states

The maintainer's rule is: fix the root in types, the data model or a function's contract, never
by an `if` or `unwrap_or` that hides the cause. A type is checked by the compiler on every call.
A `source_rules` guard reads text, and three audit rounds of 0.99 found guards that saw only one
spelling. So reach for a type first. A check that a type now holds is removed only with the
maintainer's yes, in a commit that says why the type holds it.

## The patterns Glimt already uses

Read the one nearest to your case before you invent a new one.

| Pattern | Example | What it shuts out |
|---|---|---|
| A value only one module can make | `job_log::Sure` (made only by `AtStart`); `file_io::StoppedByTheUser` (made only by `copy_until_stopped`) | A job log removed on a guess; a share's EINTR taken for the user's Stop |
| An error that carries its own judgement | `file_io::StepError`: the number and kind stay inside file_io, and callers ask `may_have_been_done`, `found_nothing` or `is_name_taken` | A caller deciding "gone" from an error number |
| An enum for each outcome | `file_io::StepLeft`, `trash::Restoring`, `threads::Told` (a notice or a failed write) | A failed write given as an ordinary notice; a step's result read as a bool |
| A checked handle to a user's file | `file_io::ListedFile` (`check` right before the act), `PickedFiles` (fingerprints taken while the user waits) | Acting on a name that another file has taken since |
| A type for a failure | `view::Failure`: only `give_error` and `send_error` take it | An error shown as an ordinary notice |
| A budget held by a value | `decode::DecodeInput`: bytes counted before they are read | A decode that holds more memory than the budget knows |
| A channel that knows its sender | `threads::Receiving<T>` (never a bare `Option<Receiver>`); `AnswerSender<T>` keeps the busy pointer until the answer is handled; `Working` | A thread gone without telling; `--wait` answering before the work is done |
| A proof carried with the value | `ReportedUndo::New(Undo, UndoOnTheWay)`, `RenameOutcome.undo: Option<(Vec<Moved>, UndoOnTheWay)>`, `set_new_undo(undo, token)` | An Undo set without Ctrl+Z having waited for it (replaced a source_rules check in 0.101) |
| An enum in place of a flag | `Removal { Trash, ForGood }`, `file_io::Transfer { Copy, Move }`, `FilesMoved { Nothing, ByTheJob, FromARecord }`, `JobWrites`, `IfMissing`, `Opening` | A wrong `false` that deletes for good or takes a copy's marks away |
| A kind taken from the look, not from the caller | `PickedFiles` (`Picked { File, Folder }` from `meta.is_dir()`), `Journal::middle_of_swap` from the plan's moves | A caller that states the kind wrong |
| One read into one value | `decode::Source` (a save reads its file once; the steps take `&Source`, not a path) | A later read of the same file that fails and is swallowed |
| A result that tells none from failed | `xmp::read_embedded -> Result<Option<String>, String>` | "No keywords" and "could not read the keywords" as one state |
| A pure core fed by one gathering of looks | `job_decisions` with `journal::observe` (`Observations` made only by `Observations::look`) | A decision made from looks taken at different moments |

## Rules

1. **An enum, not bool flags.** Two bools that together mean one state are an enum with one
   variant for each real state.
   - `exif_index_failed: bool` next to a running index means not started, reading, done or died.
     That is one enum.
   - A `bool` parameter (`is_copy`, `is_from_a_record`) is an enum of two named variants, so the
     call site says what it means.
   - A bool that a later fix must remember to reset is a sign of a missing variant. In 0.99
     `change_not_told` was added, one place reset only the old flag, and the whole tree was read
     again.
2. **Check data when it comes in.** A path, a name, a record or a socket message is turned into
   a newtype once, at the edge, by a function that checks it. After that, the code takes the
   newtype, not a `String` or a `PathBuf`. The check then cannot be forgotten further in.
3. **`Option` and `Result` mean something.** Before `unwrap_or`, `unwrap_or_default` or `.ok()`,
   say what `None` or the error means in this place. When "unknown" and "empty" both turn into
   the default, the type is missing a case. In 0.99 the photo's own marks were
   `own.unwrap_or_default()`, which made "no marks" and "marks not known" one state.
4. **A function that every caller wraps the same way takes the wrapped value.** Ten calls of
   `escaped_name(path.file_name().unwrap_or_default())` mean `escaped_name` should take the path
   and own the rule about a path without a name.
5. **A pure core without I/O.** Logic with edge cases (merging marks, judging a failed step,
   reading a record, sorting) takes plain values and returns plain values. The thread around it
   does the looks and the writes. The core is then tested without a disk, and its edge cases are
   cheap to list.
6. **A question has one place to answer it.** The answer goes in a method of the type that holds
   the fact (`StepError::may_have_been_done`), not in each caller. A new question is a new
   method there.
7. **Make illegal calls not compile.** When an act needs a proof (a fresh check, a sure answer,
   a held lock), take the proof as a parameter of a type only the proving function makes.
   `Sure`, `ListedFile` and `StoppedByTheUser` do this.

## When a guard is still right

Use a `source_rules` guard only when no type can say the rule: house style, a lock held across a
wait (Rust cannot forbid it), whether a text means an error. Text for the user looked like such a
case in 0.99, as every egui widget takes any `String`; 0.102 made it `ShownText` with clippy.toml
refusing egui's own text calls. The 0.100 inventory (`a100/inventory-rules.md`) lists which
guards a type could replace. A guard follows `glimt-classes` § 15. Name the type it stands in for in its doc comment, so
the next reader knows what would replace it.

## In a review

Each of these needs a reason or a type:

- a new `bool` field or parameter;
- a new `unwrap_or`, `unwrap_or_default` or `.ok()`;
- a new `String` or `PathBuf` that carries a checked value;
- a new guard that a type could replace.

Rank a missing type by the bar in AGENTS.md: debt that makes the next bug of a known class likely
is *should fix*.

Cargo.toml's `[lints]` could also hold `clippy::fn_params_excessive_bools` and
`clippy::struct_excessive_bools`. They are not on: adding a lint is the maintainer's choice.
