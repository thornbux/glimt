# Copy for Sharing: build plan (Glimt 0.75.0)

The design is `docs/superpowers/specs/2026-11-08-copy-for-sharing-design.md`. This plan says how
it is built, in which files, with which tests, and in which commits. Every commit passes
`tools/check.sh` (fmt, clippy and the tests, without and with every feature) on its own.

## Commits

1. **The design** (`docs/superpowers/specs/…`), as approved.
2. **This plan.**
3. **The feature**: the writers, the file operation, the action, the dialog and every new UI
   text in the 15 languages. It is one commit because the writers alone would be dead code,
   which `clippy -D warnings` refuses, and the action alone would do nothing.
4. **The manual**: the Help Manual's section in `src/ui/help.rs` (English, as the manual is).
5. **The release test**: a section in `tools/release-tests/tests.sh`.

## Step 3 in detail

### The writers: `src/images/shared_copy.rs` (new)

- `Group` (Place, Camera and lens, Date and time, Photographer and copyright, Software,
  Keywords/comment/alt text, The preview inside the file, More tags) and `Group::of(tag)`,
  from one table `TAG_GROUPS` and a few prefixes (`GPS…`, `exif:GPS…`, `aux:`, `xmpRights:`).
- `LeftOut`: the user's choice, tag names and whole groups, with `parse`/`text` for the state
  file, `is_left_out`, `set_tag`, `set_group`, `remove_all` and `kept(found)`.
- The walkers, each giving the file's parts in order, each part carried as it is, tagged, or
  dropped because it is written anew:
  - `walk_jpeg`: every segment up to EOI, the scans with their entropy data
    (`entropy_end`), and what follows EOI. APP0 JFIF, APP2 ICC, APP14 Adobe and every non-APP
    segment are always kept; APP1 EXIF and XMP are dropped and written anew; APP13 Photoshop
    is read for its IPTC IIM (`iim_properties`, through `photoshop_resources`) and written
    again without its IPTC, EXIF and XMP resources; COM, MPF, C2PA, JFXX and other APPn are
    tags.
  - `walk_png`: every chunk up to IEND. The chunks that are the image or decide its look are
    always kept; eXIf and the XMP iTXt are written anew; text chunks are tags by keyword;
    `Raw profile type exif/APP1/xmp/iptc` are always dropped, other raw profiles are one tag.
  - `walk_webp`: every RIFF chunk. The image's and ICCP are kept, EXIF and XMP written anew,
    others are tags.
- `tags_of(path)` (for the dialog's lists) and `copy(path, kept)` (for the job) both go
  through the same walk and `save::shared_metadata`, so what the dialog lists is what the
  writer decides on.
- The copy is assembled from the kept parts, with the new EXIF and XMP after SOI (and JFIF),
  after IHDR, or as WebP chunks (`save::webp_with_chunk`). Kept MPF images get their offsets
  moved by what the copy's head changed (`exif::shift_mp_offsets`).

### What a copy keeps: `src/images/save.rs`, `src/metadata/exif.rs`, `src/metadata/xmp.rs`

- `save::Derived::Shared`, a new kind of derived file, and `save::shared_metadata` beside
  `derived_metadata`: the EXIF block (`exif::derived(path, Derived::Shared)`), and the XMP
  properties of the image, its sidecar (which wins) and its IPTC IIM (which fills in),
  merged. `SharedMetadata::tag_names` lists them; `SharedMetadata::written(keeps)` gives the
  new EXIF block (`exif::shared_block`) and the new XMP packet (`xmp::packet`).
- `exif::shared_block`: a new TIFF block of IFD0, Exif, GPS and Interop with only the kept
  tags, values copied in the block's own byte order, the orientation and colour space always,
  the Interop directory always, IFD1 with its JPEG thumbnail only when "EXIF thumbnail" is kept.
  `exif::shared_tag_names` lists the tags it can keep. `tag_name` learns the Windows XP
  tags, ProcessingSoftware, HostComputer and EXIF 3.0's name tags.
- `xmp::properties`: the top-level properties of every rdf:Description (attributes and
  elements) and the namespaces declared; `xmp::packet`: a new packet of chosen properties.
- `source_rules::derived_files_take_metadata_in_one_place` gets the new homes.

### The file operation and the dialog: `src/features/copy_for_sharing.rs` (new)

- `Action::CopyForSharing` (`keys-1.0.txt`, label "Copy for Sharing…", icon, `Target::Selection`,
  File menu after Copy…, not for scripts, in the tests' `ALL`).
- `App::start_copy_for_sharing`: the targets' paths, and a thread that reads the last choice
  and the tags of the images (`tags_of`), counting as it goes.
- `App::sharing_dialog`: a modal with Remove All, each group with a checkbox and Show/Hide of
  its tags, each with a checkbox, More tags last, the note on what is always kept, and
  Copy… / Cancel (Esc). Copy… waits for the reading, saves the choice and opens the file
  chooser (`Pick::CopyForSharing`). Drawn with `menus::checkbox`, `button`, `button_enabled`
  and `small_button`; egui's own widgets speak to a screen reader, nothing is drawn by hand.
- `App::copy_for_sharing`: a job (`start_job`, holding the originals) that writes each copy
  with `file_io::create_file` (never over a file) and ends with one notice: how many were
  copied where, how many were not JPEG, PNG or WebP, and how many names were taken.
- `config::sharing_path`: `$XDG_STATE_HOME/glimt/sharing`, the left-out names one to a line,
  written through `file_io::edit_shared` in `write_later`.
- The dialog lives in `Dialogs` (`open_sharing`, `take_sharing`), as `App` has no room for a
  field (`app_stays_small`), and counts in `modal_open`.

### Tests (step 3)

- `shared_copy` tests, one for each container (JPEG, PNG, WebP): a file with a GPS position, a
  serial number, a keyword in the sidecar and one in the image, copied with GPS and the
  serial number left out: neither value's bytes are anywhere in the copy, a kept tag is still
  there, and the image data is byte for byte the original's (and decodes to the same pixels).
- `survives_broken_jpeg_segments`, `survives_broken_png_chunks_for_sharing`,
  `survives_broken_webp_chunks_for_sharing`, `survives_broken_photoshop_resources`,
  `survives_broken_xmp_properties`, `survives_broken_exif_for_sharing`.
- `LeftOut`: groups, single tags and Remove All, and its text read back.
- The contract: an entry `copy for sharing` in `contract_ops()` (the copy in `sub`, no
  sidecar there, the original and its sidecar as they were, no stars on the copy) and
  `("copy_for_sharing", "copy for sharing")` in `IN_CONTRACT`.

## Builder decisions

1. One feature commit (step 3), because the parts alone would be dead code or an action that
   does nothing, and every commit must pass clippy.
2. The dialog takes the targets' paths when it opens, as Copy… (`Pick::CopyTo`) does, rather
   than `Asked`: the copies are made from those files wherever the list moves them, and the
   originals are held back by the job, so names need not be looked up again.
3. The dialog's state is kept in `Dialogs`, because `App` is at its 42 fields.
4. The last choice is kept in `$XDG_STATE_HOME/glimt/sharing` (a file Glimt owns, added; the
   backup takes it with the state folder), written when Copy… is pressed, so other windows
   and the next start get it.
5. The choice is a set of left-out names and whole groups. A tag of a group ticked whole
   starts ticked even if it was never seen before; a tag never seen in a group not ticked
   starts unticked. What is copied is an allow-list made when Copy… is pressed: the tags shown
   and not ticked. A tag in a file that the dialog did not show is left out.
6. Copy… waits until the tags of every selected image are read, so the allow-list is whole.
7. Tag names are shown as files name them (`Make`, `dc:subject`, `GPSLatitude`) and are not
   translated; the names of whole blocks ("Extra images (MPF)", "EXIF thumbnail") stay English
   with them. The group names, buttons and notices are translated.
8. Always kept besides the orientation and the ICC profile: the EXIF colour space and the
   Interop directory, which say Adobe RGB for a camera's photo without a profile, and so are
   part of "the colour profile" the user decided to keep.
9. IPTC IIM datasets with a standard XMP name (title, keywords, creator, rights, caption,
   headline, credit, source, city, state, country, country code, sublocation, date created,
   creator's job title, instructions, transmission reference, caption writer) are carried into
   XMP; the others have no XMP name and are not carried. The sidecar's properties win over the
   image's own XMP of the same name, and IIM only fills in what XMP lacks.
10. A kept MakerNote is copied as it is. Makes whose maker notes point at offsets from the TIFF
    header may lose them, as they do in every tool that writes a new EXIF block
    (`shortcut:` comment).
11. Not carried, never: Extended XMP (a second APP1 of a long packet), a compressed XMP chunk in a
    PNG, the IPTC in a PNG's raw profile, and data after a PNG's IEND. Each leaves less in the
    copy, never more (`shortcut:` comments).
12. A file whose name is taken in the folder is skipped and counted; no name is made up.
13. A file that is not JPEG, PNG or WebP by its contents is skipped and counted.
14. The File menu has the action after Copy…; it has no default key (Geeqie has no such action,
    so no row in `DIFFERENCES`).

## What is checked by hand

- The dialog's look, by a screenshot in the release test.
