---
name: glimt-release
description: Use when the main session runs a Glimt release - planning it, starting fixer agents, merging their branches, auditing, bumping, running the release tests, tagging, deploying the website and cleaning up. The order of the steps, the checks between them, and what went wrong before.
---

# Running a Glimt release

docs/RELEASING.md says what a release is, and AGENTS.md the rules it follows. This is how the
main session runs one with agents, step by step. Only the maintainer starts a release, and a
bump, a tag, `main` and the website change only when the maintainer asks (memory: "User decides
releases").

## 1. Plan

- **One or two classes or features per minor release.** Name them, the issues they close and
  what waits. Hardening goes in only when an ordinary user meets it or a root fix closes it.
- **Write the plan in the mx doc** (`/opt/docker/documentation/glimt/next-fix-2026-11-05.md`,
  fetch it live first, another session may have changed it) as "X.Y.0 PÅGÅR".
- **Ask the maintainer's choices up front** (frozen words, behaviour changes, clippy.toml), so no
  agent stops halfway.

## 2. Branches and agents

- `git checkout -b release-X.Y.0 main`, push it.
- **A fixer brief** in the scratchpad (`aNNN/brief.md`), copied from the last one and updated:
  the release branch and its base, what the release is, the working rules, "read the skills in
  .claude/skills/", no new exceptions without asking, every commit green in `tools/check.sh`,
  "send your plan to main first", the report format, and the waiting rule (wait for check.sh's
  own notification, never a `pgrep -f` loop, which matches its own command line and never ends).
- **One agent per area that touches its own files**, in `isolation: worktree`, on Opus. Tell each
  which files the others own. About three builds at once is what the Docker VM takes.
- **Answer each plan** before the agent writes code. Look for a type with a hole, a check that
  would be loosened, a behaviour change the maintainer has not seen.

## 3. Merging

- Fast-forward when the branch was made from the current release branch; otherwise a merge
  commit. Tell the other agents when the release branch moves, so they merge it before they
  report.
- **Run `tools/check.sh` on the merge**, read its exit status directly, then `git diff --quiet`.
  An agent that says a commit passed "except fmt" or "only that test" has not shown the commit
  green: check that commit alone in its worktree (`git checkout --detach SHA`, check.sh).
- After merging: remove the agent's worktree (`git worktree unlock`, `remove --force`), its
  branches, and its build folder in the volume
  (`docker run --rm -v glimt-target:/t alpine rm -rf /t/agent-ID-*`).

## 4. Audit

`glimt-audit`. Round 1 on all the release's changes, later rounds on each round's fixes, until a
round is clean. Fix by class.

## 5. Release tests and CI, before the bump

- **Look at CI on the release branch** (`gh run list --branch release-X.Y.0`) and read every
  failure. In 0.101 the tag's workflow failed on a test that had already failed on the release
  branch of 0.100 unnoticed; it was a real bug. A test that fails now and then on CI is a bug.
- **Disk:** `tools/release-tests/run.sh` needs 25 GB free in the `glimt-target` volume. Free it
  by removing gone worktrees' build folders, or this checkout's own `debug/incremental`. Never
  `check.sh --clean` and never another project's images or volumes without asking.
- A run on an earlier commit tells early whether a large change behaves in the kill sections.

## 6. Bump, test, tag

1. With the maintainer's yes: the version in `Cargo.toml`, `## [X.Y.0] - date` and its link in
   `CHANGELOG.md`, `.TH` in `docs/glimt.1`, a `<release>` line in the metainfo, then
   `tools/check.sh "cargo update -p glimt --offline && python3 tools/sbom.py && python3 tools/flatpak-sources.py"`,
   one commit "Glimt X.Y.0". An empty CHANGELOG section gets a line that says what changed
   inside (a structural release).
2. `tools/check.sh`, push, then `gh workflow run release.yml --ref release-X.Y.0 -f dry_run=true -f tag=none`
   and `tools/release-tests/run.sh BUMP vLAST`, both on the bump commit.
3. With the maintainer's yes and everything green: `git checkout main && git merge --ff-only release-X.Y.0`,
   `git tag -a vX.Y.0 -m "Glimt X.Y.0"`, `git push --atomic origin main vX.Y.0`.
4. Wait for the tag's workflow (`gh run view`), not with `gh run watch`, which dies on a network
   hiccup. Then download `SHA256SUMS` and the `.deb`, `shasum -a 256 -c --ignore-missing`, and
   `gh attestation verify`.

**When the tag's workflow fails** before a release is published: find the cause first. If the fix
needs code, ask the maintainer whether to take the tag back (`git push origin :refs/tags/vX.Y.0`,
`git tag -d`) and tag the fixed commit with the same version, or make X.Y.1. Move the fix's
CHANGELOG line into the release's section, audit the fix, and run the release tests and the dry
run again on the new commit.

## 7. After the tag

- **glimt-web** (`~/claude/privat/glimt-web`), from the tag, not the working tree: `git archive
  vX.Y.0` into a scratch folder, then `tools/changelog.py`, `manual.py`, `cli.py` and
  `screenshots.py` with that folder, `npm run build`, one commit "Glimt X.Y.0". Generate again if
  the tag moved. Deploy as the memory "Glimt website" says (push to mx, merge, `docker compose up
  -d --build --no-deps glimt-web`) and check `news/` and `download/` through Traefik.
- **Issues:** "Fixes #N" closes them when `main` is pushed; close the others that no longer hold,
  with a comment.
- **The mx doc:** "X.Y.0 SLUPPET" with the tag, the contents, the audit rounds, the release-test
  numbers, the workflow, issues closed and filed, and what was not run (the Flatpak by hand).
  Keep a backup on mx (`/tmp/next-fix.bakN`).
- **Memory:** the release line in `project-glimt-0-60.md` and `MEMORY.md`, and a feedback memory
  for any new lesson.
- **Clean up:** the release branch locally and on GitHub, worktrees, agent branches, build
  folders, and any shell an agent left behind (`ps` for `until ! pgrep`).
