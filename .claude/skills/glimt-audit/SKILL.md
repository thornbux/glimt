---
name: glimt-audit
description: Use when auditing a Glimt release before its tag, when writing an audit brief for another agent, or when ranking audit findings. How a round is scoped, what an auditor reads and checks, how findings are ranked and reported, and when the audit is done.
---

# Auditing a Glimt release

AGENTS.md (*Releases*) says a release is tagged only when an audit of its own changes finds
nothing it must or should fix. The audit reads the diff without the author's conclusions, so it
runs in another agent. The main session writes the brief, starts the auditors, groups their
findings into classes and has them fixed, and audits the fixes again until a round is clean.
0.99 to 0.101 took 4, 8 and 6 rounds.

## The brief

Write it once per release in the scratchpad (`aNNN/audit-brief.md`) and point every round at it.
It holds:

- **Read-only.** No branch, commit, stash, cargo, check.sh or run.sh. Only `git diff`, `git show`,
  `git log` and `git grep` in the main checkout.
- **Read AGENTS.md in full first**, as it stands at the commit audited. Name the sections that
  changed in the release.
- **What the release is** in two lines: features, fixes, or structural work that keeps
  behaviour. A structural release says "behaviour unchanged except where a commit names a change".
- **The bar.** Rank each finding must, should or hardening by AGENTS.md *What must be fixed, and
  when*, with one sentence why, and name its class (`glimt-classes`). A finding needs a concrete
  case. Hardening is reported briefly: it becomes an issue and waits.
- **Known and decided, do not report again.** Everything the maintainer approved in the release
  (exceptions, removed checks, behaviour changes, frozen words), the decided limits in AGENTS.md,
  "all windows are one version", and the open issues left for later by number. Without this list
  each round reports the same things again.
- **Where to write** (`rN-X.md`) and how long the hand-back is (8 to 15 lines).

## Scoping a round

- **Round 1** reads everything since the last tag (`git diff vLAST..COMMIT`). Split it between
  two auditors by area (for example the job-log model and the rest). Three is more than a
  structural release needs.
- **Later rounds** read only the fixes since the round before (`git diff PREV..COMMIT`), with the
  round before's report as context. One auditor.
- **Give each round its focus questions.** Name the riskiest paths of the change. A general "look
  for bugs" finds less than "does a card pulled out during the name pass keep its marks".
- **A round on a comment or a CHANGELOG line** is still a round. Keep it short (scope one
  commit, four lines back), but run it.

## What an auditor checks

For each change:

1. **Behaviour.** Compare with the last tag (`git show vLAST:path`) case by case. A structural
   change that says "behaviour unchanged" is checked on every path: each caller, action,
   `--remote` command, background thread, the moments between a start and its answer, and a
   file a kill left under the last release.
2. **The class, not the form.** Does the fix close every way in, or only the one the finding
   named? A fix of one swallowed read on the save path, then another, then another, is the sign
   that the root (one read into one value) is missing.
3. **Types with holes.** A `pub` field, a `Default`, a constructor any module can call, or a
   `bool` taken at a boundary and passed on lets the wrong state be built again. Check that the
   type is really the only way.
4. **The test.** Does it test what its name says? Would it fail without the fix (read it, or say
   that only a run can tell)? Was an existing test or check loosened, or a check removed without
   the maintainer's yes?
5. **AGENTS.md's rules.** The window thread, a swallowed write, a new exception or `#[allow]`, a
   missing po text, a comment that says what the code does not, a function over 120 lines, items
   out of order, one thing with two names.
6. **The CHANGELOG** (in the last round): each line says what the release does, and no
   user-visible change is missing.

An auditor reports a finding that does not hold as such, and says what it did not check (it
builds and runs nothing).

## After a round

- **Group the findings into classes** before anything is fixed (AGENTS.md *Fix the root*). The
  same class seen in two rounds means the root is missing: fix it in the shared place or a type,
  not one more symptom (in 0.100 the save path needed `decode::Source` after three rounds of
  single reads).
- **Small wording fixes** (a comment, a CHANGELOG line, a translation) the main session makes
  itself, with `check.sh`, and audits in the next round.
- **Hardening** becomes a GitHub issue labelled `hardening` with its case and why it waits.
- **A finding against a decided limit** is not a finding, unless the limit itself loses a file
  or a mark.

## When the audit is done

A round that finds nothing to fix by the bar, on the commit that will be tagged or one whose only
later change is the version bump. Then the release tests and the dry run run on the bump commit
(`glimt-release`).
