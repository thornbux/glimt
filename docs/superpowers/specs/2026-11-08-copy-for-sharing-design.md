# Copy for Sharing: copies of many images without the metadata chosen (Glimt 0.75.0)

## Why

The user publishes several images at a time on the Internet and wants to leave out some or all
of what the files say about them: where (GPS), with what (camera, lens, serial numbers), when,
by whom, with which program, and the keywords and comments. Which tags go varies from one time
to the next. Today only File → Save As can drop metadata, one image at a time, all or nothing,
and it encodes the pixels again (a JPEG loses quality).

## What it does

1. The user selects images (the selection, `App::targets`, as every action on the selection) and
   chooses File → Copy for Sharing… (*my call:* the name and the place in the File menu; the
   action has no default key).
2. A dialog asks for a folder (the file chooser, `Pick`, through `ask_about_targets`/`Asked`
   as the other actions on the selection) and what to leave out.
3. A job with progress and Stop (`start_job`) writes a copy of each image into that folder.
   The originals are never changed.

### Choosing what to leave out

- At the top, a button "Remove All": ticks every tag except the two that change how the image
  looks (below).
- Then the groups, each with a check box and a fold-out of its tags, each tag with its own
  check box (ticked = left out):
  - Place (the GPS IFD and XMP's exif:GPS*)
  - Camera and lens, with serial numbers (Make, Model, LensModel, BodySerialNumber,
    LensSerialNumber, …)
  - Date and time (DateTime*, OffsetTime*, SubSecTime*, xmp:CreateDate, …)
  - Photographer and copyright (Artist, Copyright, dc:creator, dc:rights, …)
  - Software (Software, ProcessingSoftware, xmp:CreatorTool, …)
  - Keywords, comment and alt text (dc:subject, dc:description, the IPTC alt text and extended
    description, UserComment, ImageDescription)
  - The preview inside the file (the EXIF thumbnail, IFD1)
- Below the groups, "More tags": every other tag found in the selected files (read in the
  background, as the EXIF reader does), so the user can leave out more.
- The dialog starts with the choice made last time (kept in Glimt's state, not a setting
  the user must edit; *my call:* no named presets).
- Always kept, also by Remove All, and shown as kept with the reason: the orientation (without
  it a phone photo shows on its side) and the colour profile (ICC; without it the colours can
  change). The user decided this.

### The copies

- JPEG, PNG and WebP. The pixels are copied byte for byte: only the metadata segments (JPEG
  APP1 EXIF and XMP, APP13 IPTC, COM; PNG eXIf, tEXt/zTXt/iTXt; WebP EXIF and XMP chunks) are
  rewritten or left out. No new encoding, no loss of quality.
- Other formats are skipped, and a notice at the end names how many and why.
- Glimt's own sidecar (`.xmp` beside the file: keywords, comment, alt text, stars) is never
  copied as a file, but what it says goes into the copy's own XMP, so alt text written in Glimt
  for publishing reaches the published copy. Its properties are tags in the dialog like any
  other and can be left out. The user decided this.
- Same name as the original in the chosen folder. A name already there is never written over
  (`create_file`/`put_file`, never over a file); *my call:* such a file is skipped and counted
  in the end notice, not renamed, so the folder holds no surprise names.
- No Undo (Copy has none today, `file_ops::Undo` holds only Trash, Move and Rename); the end
  notice says where the copies are.

### How a copy is built (my calls, after the builder check)

- **Allow-list, not deny-list.** The copy is written fresh: the image data byte for byte, the
  parts that decide how it looks (JPEG's JFIF APP0 and Adobe APP14 colour transform, the ICC
  profile in APP2 / iCCP / ICCP, PNG's gAMA, cHRM, sRGB, pHYs), a new EXIF block holding only
  the tags kept, and a new XMP packet holding only the properties kept. Everything else is not
  carried over unless it is a tag the user kept.
- **New EXIF, not an edited one.** The kept tags are written into a new block, so a dropped
  tag's value bytes cannot linger in the data area. The orientation is always written.
- **The MakerNote** (the maker's private block: serial numbers, lens, dates, sometimes GPS) is
  one tag in the Camera and lens group, and is removed by Remove All.
- **Blocks that are not single tags** are listed under "More tags" as one entry each and are
  left out by Remove All: the extra images of an MPF (a phone's second image or HDR gain map,
  each with its own EXIF), content credentials (C2PA, APP11), Photoshop resources other than
  IPTC (with their thumbnail), and the "Raw profile" PNG text chunks.
- **IPTC IIM** (APP13) is not rewritten in place: its datasets are shown as tags, and those
  kept are written into the copy's XMP under their standard IPTC Core / Dublin Core names
  (creator, rights, keywords, caption, …). The copy has no IIM block. Modern readers take XMP.

## Goals, non-goals, anti-goals

- Goal: in one go, a folder of copies that say only what the user chose, in the same quality.
- Non-goals: resizing or another format (Save As does that, one image); changing the originals;
  other formats than JPEG, PNG and WebP; presets with names.
- Anti-goals (a failure even if it works): an original changed in any way; a tag the user chose
  to leave out still in the copy anywhere (EXIF, XMP in the file, the IPTC block, a PNG text
  chunk, the EXIF thumbnail); a copy that looks different from the original (turned, other
  colours); a file in the folder written over.

## Constraints (from AGENTS.md)

- What a copy keeps of the metadata is decided in one place: `save::Derived` / `exif::derived` /
  `derived_xmp` (`source_rules::derived_files_take_metadata_in_one_place`). The tags chosen go
  through there as a new kind of derived file, not a second filter beside it.
- Each change on disk through `file_io` (`create_file`), never a raw write (clippy refuses it).
- A new file operation joins the contract in file_ops.rs's tests (`contract_ops`, `IN_CONTRACT`).
- Every new UI text in all 14 po files; the manual in help.rs; a check in tests.sh that fails on
  0.74; the website's pages on it at the release.
- A parser of file contents (the segment and chunk walkers) gets a `survives_broken_*` test.
- Speed: work in the background; the UI thread only draws the dialog.
- Names by the naming rule in AGENTS.md.

## Left to the builder

The dialog's layout within these rules, the tag lists of each group in detail, how "More
tags" reads the selected files, where the last choice is kept, and the code structure.

## After audit round 1 (2026-11-08)

- A block that is not a single tag (MPF extra images, data after the image, MakerNote,
  Photoshop resources, C2PA, other APPn segments and chunks, XMLPacket, IPTC-NAA, PNG raw
  profiles, unnamed EXIF tags) goes as soon as any tag of a group it can carry is left out
  (`shared_copy::BLOCKS`, `UNKNOWN_BLOCKS`); one it is unsure of counts as carrying every group.
  The JFIF thumbnail is a Preview; the Interop directory keeps only its index and version.
- XMP properties are named by their namespace, and any name with gps, latitude, longitude or
  altitude in it is in Place.
- The tags are listed from the head of each file (up to its image data), and a format other
  than JPEG, PNG and WebP is known from its first bytes, so a large selection lists quickly.

## After audit round 2 (2026-11-08)

- A single tag can belong to several groups (`Group::groups_of`: word rules on the whole name,
  with `GROUP_EXCEPTIONS` beside them) and goes when any of them is ticked; GPS dates are Date
  and Place, raw file names Date and Photographer.
- An Ultra HDR gain map (the MPF image with hdrgm XMP) is kept without its EXIF or other tags,
  with only its hdrgm XMP and a new MPF index, so the copy looks the same on an HDR screen.
- Content credentials (C2PA) are never carried: every copy changes the bytes they sign.

## After audit round 3 (2026-11-08)

- A tag's groups come from an allow-list, as a block's do: a tag known by its name
  (`KNOWN_NAMES`, EXIF's names also as exif:, exifEX: and tiff:), its namespace
  (`KNOWN_NAMESPACES`) or as a block gets the groups its words or `GROUP_EXCEPTIONS` give.
  Any other tag may say anything, so it is in every group: it goes when any group is ticked.
  The dialog lists it under More tags only, where unticking it keeps it, and a note says so.
- A gain map marked only by ISO 21496-1's APP2 segment is kept as Ultra HDR's is. Other forms,
  such as Apple's, go with the extra images.
- A copy without a motion photo's video keeps none of its GCamera MotionPhoto or MicroVideo
  flags.

## After audit round 4 (2026-11-08)

- No namespace is known whole, as any of them can hold a blob of other tags (Google's maker
  notes in GCamera, darktable's history, a Lightroom mask's name). `KNOWN_NAMES` lists each
  property of aux, PLUS, MediaPro, DJI, GCamera, GPano, xmpRights, crs and darktable that is
  known, from exiftool's tables; every other property of them is in every group.
- A history that names the files it was made from is Photographer as well as Date and
  Software, as `xmpMM:DerivedFrom` is: digiKam's `ImageHistory` and Photoshop's `History`.
