# Releasing Glimt

The steps of a release, and how the packages are built and checked. The rules a release follows
(the two tracks, the audit before the tag, the release tests on the tagged commit) are in
[AGENTS.md](../AGENTS.md), under *Releases*. Only the maintainer decides that a release is made:
no version, tag or push for a release happens without the maintainer asking for it.

## The steps

A release gathers its work on a release branch, `release-X.Y.Z`. Each feature or fix is a
branch made from it and merged back into it; `main` gets the release branch only at the tag.

1. **The version.** The version in `Cargo.toml` and in the header of `docs/glimt.1`, a section
   in `CHANGELOG.md` with its link (`source_rules::every_release_in_the_changelog_has_its_link`),
   and a `<release>` line in `assets/io.github.thornbux.Glimt.metainfo.xml`. New command-line
   options go in the man page too. The release workflow checks each. After the bump, run
   `cargo update -p glimt --offline`, `python3 tools/sbom.py` and `python3 tools/flatpak-sources.py`
   (through `tools/check.sh "…"` on a computer without cargo), so `Cargo.lock`, `sbom.cdx.json`
   and `flatpak/cargo-sources.json` follow the version; CI refuses a commit where they do not.
2. **The Flatpak image.** Dependabot does not move a container's digest, so take the tag's
   current digest (the command is in `release.yml`), which brings the image's fixes. These two
   steps come first, so the audit covers them.
3. **The audit.** An audit of everything since the last tag finds nothing it must or should
   fix (AGENTS.md, *What must be fixed, and when*). Its fixes are audited again, until a round
   finds nothing.
4. **The release tests.** `tools/release-tests/run.sh` on the very commit that gets the tag
   (below), a full run, never `QUICK=1`.
5. **The dry run.** Start the Release workflow by hand on the release branch, which builds and
   smoke-tests every package and publishes nothing. The branch must be on GitHub first
   (`git push origin release-X.Y.Z`; delete it there afterwards):
   `gh workflow run release.yml --ref release-X.Y.Z -f dry_run=true -f tag=none`.
6. **By hand.** The checks below that no script makes.
7. **The tag.** `main` is fast-forwarded to the tested commit
   (`git merge --ff-only release-X.Y.Z`). If it cannot be, the release branch is first rebased
   onto `main` or merged with it, and steps 3 to 6 run again on the new commit. The tag
   `vX.Y.Z` is an annotated tag (`git tag -a vX.Y.Z -m "Glimt X.Y.Z"`) on that same commit, and
   `main` and the tag are pushed together (`git push --atomic origin main vX.Y.Z`), so the
   workflow never sees the tag before `main` holds it. The release workflow builds and
   publishes the packages (below).
8. **After the tag.** Check that the Release workflow ended green, with every package,
   `SHA256SUMS` and the attestations, and the Flatpak bundle.
9. **Flathub.** A pull request to Flathub's repository (below). The website (glimt-web) is
   updated when the maintainer asks.

## The release tests

Run `tools/release-tests/run.sh` on the release branch, on the very commit that gets the tag.
A change after the run means a new run. A release on the same day as the last one gets the
whole suite too, never a patch release on trust.

The run builds the branch and the last release with every feature in Docker (`rust:1`), and
runs `tools/release-tests/tests.sh` there. Each run has a virtual X display, an ordinary user, a
separate `HOME` and XDG folders, generated pictures, and a script named `zenity` early in `PATH`
that answers for the file dialog. The checks run in as many containers at once as Docker's
processors and memory allow (two processors and 2.5 GB each, 1 to 6; `PARTS=N` to choose).
Each container has its own display, `/tmp` and `HOME`, and the timings run alone after them. A
part may start at any section, so a section makes what it needs itself (a function in the head
of `tests.sh` for files that several sections use).

Each check prints PASS or FAIL. Timings and memory are printed for both versions, to compare.
The rule for the timings and memory that fail a run is 1.5 times the last release (AGENTS.md,
*Goals that decide every change*): the time to the first image and RssAnon in a folder of
500,000 files, and the time to sort 3,000 JPEGs by EXIF date. Each version runs twice, in turn
with the other, and the best of its two runs counts, because one run can be slow by chance.
`within` in `tests.sh` then passes the new value when it is at most 1.5 times the old one plus
a slack of 100 ms or 20 MB, because a few hundred milliseconds vary by more than half when
something else runs on the machine. A version whose run gave no number fails the check. The
median frame with all 500,000 files selected must also take at most 10 ms of the window
thread's processor time. The other timings are only printed. `QUICK=1` leaves out the large
folder and the EXIF timings. Screenshots of the steps are in
`~/.cache/glimt-release-tests/out`. Among the checks:

- **Saving turned:** a PNG, a lossless WebP and a lossy WebP with EXIF orientation 6, turned and
  saved with Auto Rotate on and off: EXIF kept, orientation 1, pixels as the view showed them.
- **Saves at once:** `a.png` and `a.webp` of 6000×4000 turned one after the other: both whole.
- **Subfolders:** with Include Subfolders on and auto refresh off, copy and move to the folder
  and to subfolders; the list must follow.
- **A failed delete** in a read-only subfolder: the file is in the list again.
- **Thumbnails:** with Auto Rotate off, nothing new in `$XDG_CACHE_HOME/thumbnails`.
- **Rename Many failing halfway:** names that swap and one too long (over 255 bytes); every name
  kept, no `.glimt-rename-*` left. A read-only file is no test, as Linux renames it.
- **Find Duplicates:** the checked files go to the trash, but nothing does once the list changed.
- **Pan view:** after another sort order each date keeps its image.
- **Printing** 80 images to a PDF: 80 pages, and memory that does not grow with them.
- **Real files:** each of `samples.txt` (RAW of eight makes, DICOM, FITS, NIfTI, a slide, HEIC,
  AVIF, JPEG XL …), downloaded once by `run.sh` and checked by sha256, is shown.
- **Copy for Sharing of real photos** of `samples.txt` (a Pixel's Ultra HDR, an iPhone's HDR, a
  DJI drone's, a Lightroom export, content credentials), with Remove All and with Place: no
  place, serial number, maker note, credentials or keyword left, the gain maps and pixels kept.
- **A failing environment,** made to fail on purpose: a full disk (a 16 MB tmpfs) during a Move,
  a Copy, a turned save and a sidecar write; a share (a FUSE pass-through) that removes a file
  but answers its unlink with an error while its cache still finds the name, under a Move and a
  Rename; a JPEG still being written when it is turned with Save Orientation on; `kill -9`
  during a Move and a Rename Many, then a new start; a read-only folder or sidecar under a
  keyword save that Ctrl+Q waits for; the trash failing partway (its disk full, a small ext2,
  as gio refuses a tmpfs, and gio failing on some files), then Undo; the marks file, a
  collection and Rename Many's plan on a full disk; a photo and its sidecar split by a full
  disk under a Move and a Copy; a Move to another disk whose unlink of the source is refused;
  a read that fails (EIO, from the share) under a Copy, a Move, Copy for Sharing and Save As;
  Copy for Sharing, Crop and Save As onto a full disk; `kill -9` during a turn's save, a
  sidecar write and a Lightroom import. No photo, sidecar or star lost or put on another file,
  and the failure told. Each makes sure that its fault did happen. The full disks and the share
  need `--privileged`, which `run.sh` gives the parts with those checks (`faults=yes`).
- **Large folder** (500,000 hard links) and **sorting 3,000 JPEGs by EXIF date:** "first image"
  time and RssAnon of both versions.

## Known vulnerabilities and updates

`cargo audit` runs in the release workflow, which stops on a vulnerable crate. Warnings, such
as an unmaintained crate, do not stop it. Between releases, `.github/workflows/audit.yml` runs
it every Monday and when `Cargo.lock` changes, and Dependabot (`.github/dependabot.yml`)
proposes updates of the crates and the pinned actions. `.github/workflows/update.yml` opens a
pull request every Tuesday with `cargo update` (also of the crates that Cargo.toml does not
name) and the files made from Cargo.lock. It starts CI on that pull request, and says when the
Flatpak image's digest has moved.

## By hand

These need a desktop, so no script checks them:

- The Flatpak, with two windows (Ctrl+N): `--remote next` goes to the focused one,
  `--remote-all next` to both, and `--remote-all quit` closes both.
- In the Flatpak, Delete and Ctrl+Z (the trash portal), also of a read-only file, which the
  portal refuses, so Glimt moves it to the trash by hand.
- File → Open File (the file chooser portal).
- Print through the System print dialog (the print portal); its Print to File shows the PDF.
- Full screen: an Inhibit call to the portal, and a Close call after it, seen in
  `dbus-monitor`.
- Open With → Choose Application… (the chooser, cancelled).
- A video and a HEIC (the runtime's ffmpeg).
- File → Set as Wallpaper (the wallpaper portal).
- A tool: without the host permission it says how to give it, and with the permission it runs.
- Orca reads a notice that is shown twice aloud twice (an action refused twice, such as a
  setting changed under `--lock-settings`).

The release workflow's smoke test, `tools/package-smoke.sh`, checks before each package is
published that the AppImage and the Flatpak bundle draw an SVG, and that the AppImage also
draws it in a New Window after the first window closed.

## Packages

A tag `vX.Y.Z` pushed to GitHub runs `.github/workflows/release.yml`. It builds against glibc
2.28 with cargo-zigbuild and publishes a `.tar.gz`, a `.deb`, an `.rpm` and an AppImage. Both
package formats are described in `Cargo.toml` (`[package.metadata.deb]` and
`[package.metadata.generate-rpm]`). A new installed file goes in both lists
(`source_rules::packages_install_the_same_files`), in the Flatpak manifest, and in the tar.gz
and AppImage steps. To build and check the packages locally, after
`cargo build --release --features full`:

```sh
cargo deb --no-build -o /tmp/pkg/          # cargo install cargo-deb
dpkg-deb -I /tmp/pkg/*.deb; dpkg-deb -c /tmp/pkg/*.deb
cargo generate-rpm -o /tmp/pkg/            # cargo install cargo-generate-rpm
```

- The packages' dependencies are written by hand, for glibc 2.28. `dpkg-shlibdeps` rounds glibc
  up to 2.34, and cargo-generate-rpm's own list has `[WEAK]` symbol versions that dnf cannot
  resolve. OpenGL (EGL) and xkbcommon are named too: the window loads them as it opens, so no
  tool finds them.
- `examples/tools` and `examples/formats` are installed as examples, not as active tools: in
  `/usr/share/doc/glimt/examples/` in the `.deb` and `.rpm` (scripts 755, so they work when
  copied), and as `examples/` in the tar.gz. Glimt reads `/usr/share/glimt/{tools,formats}`
  (and the old `{plugins,decoders}`), but nothing is put there. Each example needs programs
  the package cannot count on (Ollama, spd-say, exiftool, ImageMagick, Python modules), and it
  would sit in the menu for everyone and fail. A user copies the ones wanted to
  `~/.local/share/glimt/{tools,formats}` (Edit → Tools → Open the Tools Folder). The examples
  are not in the AppImage or the Flatpak, as no README is: their files are out of the user's
  reach. A new example script is installed by the globs already there.
- A local build links against the build machine's glibc. Only the release binary runs on 2.28.
- To test an `.rpm` without rpm tools, use a container with its storage on `/tmp`:
  `podman --root /tmp/pod --runroot /tmp/podrun --storage-driver vfs run --rm -v /tmp/pkg:/pkg:Z
  registry.fedoraproject.org/fedora:latest bash -c 'dnf -y install /pkg/*.rpm && glimt --help'`
  (also `docker.io/library/almalinux:8` for glibc 2.28). Remove the storage afterwards with
  `podman unshare rm -rf /tmp/pod /tmp/podrun`.

Each release also gets `SHA256SUMS` of its files and a build provenance for each file, signed
by GitHub through `actions/attest-build-provenance`; `gh attestation verify FILE -R
thornbux/glimt` checks it. A new file to publish goes in both, and the Flatpak's line is added
by its own job.

### The AppImage

The AppImage holds the same binary, its desktop file, icons, man page and licenses. OpenGL,
Wayland and X11 come from the system, as for the tar.gz. `appimagetool` and the type2 runtime
(which needs no libfuse2) are pinned releases with their sha256 in `release.yml`; change both
together. Inside the AppImage, `$APPIMAGE` names the AppImage file itself, and New Window starts
that file, because the binary's mount goes away when the first window closes. It does so only
when the binary is inside `$APPDIR`, because every program started from an AppImage inherits
its `$APPIMAGE`.

### The Flatpak

The release workflow also builds a Flatpak bundle from `flatpak/io.github.thornbux.Glimt.yml`
in Flathub's `freedesktop-26.08` image, pinned by its digest (change both with
`runtime-version`), and adds it to the release in a job of its own. It builds without the
network, as Flathub does: the crates are sources in `flatpak/cargo-sources.json`, so a
`Cargo.lock` changed without it fails there. To build it locally: its runtime, SDK and Rust
extension take about 4 GB, so they can live on `/tmp` through `FLATPAK_USER_DIR`:

```sh
export FLATPAK_USER_DIR=/tmp/fp/flatpak
flatpak --user remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
    --state-dir=/tmp/fp/state /tmp/fp/build flatpak/io.github.thornbux.Glimt.yml
flatpak run io.github.thornbux.Glimt
```

- Glimt's own external programs go through `command()` or `stoppable_command()` in
  `src/files/external.rs`. In the Flatpak these run the runtime's programs, in the sandbox
  (Flathub allows no `flatpak-spawn --host`). The user's own programs, and those only the host
  has, go through `host_command()` or `stoppable_host_command()`. These run on the host after
  `flatpak override --user --talk-name=org.freedesktop.Flatpak io.github.thornbux.Glimt`, and
  give an error that says so without it.
- To run it in a virtual X display, add `--nosocket=wayland --nosocket=fallback-x11
  --socket=x11`, and `--filesystem=/tmp/...` for test files: `/tmp` is private in the sandbox.

### Flathub

Flathub builds from its own repository, `flathub/io.github.thornbux.Glimt`, not from this one.
After each release tag, `python3 tools/flathub-manifest.py vX.Y.Z <its checkout>` writes there
the manifest with Glimt from the tag (and its commit) and `cargo-sources.json`, for a pull
request. Flathub builds it and publishes once it is merged. Check it with Flathub's linter:

```sh
docker run --rm -v <dir>:/m ghcr.io/flathub/flatpak-builder-lint manifest /m/io.github.thornbux.Glimt.yml
```

On a local build (`builddir`), the linter's only complaints are the screenshots and icons not
mirrored, which Flathub's builder does.
