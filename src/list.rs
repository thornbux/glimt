//! The model of what is open: the file list with everything that holds a place in it
//! (`List`), the decoded images (`Slot`), and what belongs to the image shown (`Viewing`).

use crate::crop;
use crate::decode::Hist;
use crate::file_list::RubberBand;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;

/// What belongs to the image shown, beside its place in the list (`List.cur`): the page of a
/// TIFF, the frame of an animation stepped to, the turn not saved yet and the file it belongs
/// to, the part being cropped. Made anew whole when the image is left (`leave_image`), as
/// `List` is when the list is read again, so nothing of one image stays for the next, also a
/// field added later; the zoom, which Keep Zoom carries over, is not here.
#[derive(Default)]
pub(crate) struct Viewing {
    /// The page of a multi-page TIFF shown (Ctrl+Page Down/Up), from 0.
    pub(crate) page: usize,
    /// The frame of an animation stepped to with Ctrl+Page Up/Down; `None` while it plays.
    pub(crate) frame: Option<usize>,
    /// Orientation of the shown image, for viewing only: quarter turns clockwise, applied
    /// after mirroring when the flag is set. Reset for each image, as in Geeqie.
    pub(crate) orient: (u8, bool),
    /// The file `orient` belongs to, set when it is turned: saved into that file even when the
    /// list changed meanwhile and another file is at `cur`.
    pub(crate) turned: PathBuf,
    /// Edit → Crop: the part of the shown image being chosen.
    pub(crate) crop: Option<crop::Crop>,
}

/// An image decoded for the view: the shown one or a neighbour read ahead, at the size shown or
/// (`full`) at full resolution.
pub(crate) struct Slot {
    pub(crate) tex: egui::TextureHandle,
    /// The file the texture was made from; decides whether it can be kept when the file list
    /// changes.
    pub(crate) path: PathBuf,
    pub(crate) full: bool,
    /// Original size after EXIF rotation.
    pub(crate) orig: [usize; 2],
    /// Frames of an animated GIF or WebP with their delays in ms; empty for a still image.
    pub(crate) anim: Vec<(egui::TextureHandle, u32)>,
    /// Of the decoded image (the first frame of an animation).
    pub(crate) hist: Box<Hist>,
    /// Pages of a multi-page TIFF; 1 for other files.
    pub(crate) pages: usize,
    /// The box it was decoded into (`decode_box`): decoded again only for a larger one.
    pub(crate) decoded_for: [u32; 2],
}

impl Slot {
    /// Whether any pixel of the (first) frame is less than opaque.
    pub(crate) fn transparent(&self) -> bool {
        self.hist[4][..255].iter().any(|&n| n > 0)
    }

    /// The frame to show `t` after the image was opened; asks for a repaint when the next is due.
    pub(crate) fn frame(
        &self,
        t: std::time::Duration,
        ctx: &egui::Context,
    ) -> &egui::TextureHandle {
        let (k, left) = self.frame_at(t);
        if let Some(left) = left {
            ctx.request_repaint_after(std::time::Duration::from_millis(left as u64));
        }
        self.anim.get(k).map_or(&self.tex, |f| &f.0)
    }

    /// The frame shown `t` after the start, and the milliseconds it still has.
    pub(crate) fn frame_at(&self, t: std::time::Duration) -> (usize, Option<u32>) {
        let total: u32 = self.anim.iter().map(|f| f.1).sum();
        let mut at = (t.as_millis() % total.max(1) as u128) as u32;
        for (k, (_, ms)) in self.anim.iter().enumerate() {
            if at < *ms {
                return (k, Some(ms - at));
            }
            at -= ms;
        }
        (0, None)
    }
}

/// The file list and everything that holds a place in it: made anew when the list is another
/// (`replace_files`), so nothing indexed into one list can outlive it, whatever field comes
/// next. What follows a file from one list to the next (the shown image, the selection, the
/// panes, the decoded slots) is carried over by name there, in the one place.
#[derive(Default)]
pub(crate) struct List {
    /// The images in `dir`, file name only (the full path is built on demand by `path`): saves
    /// memory in large folders, where the path would otherwise be repeated for every file.
    pub(crate) files: Vec<OsString>,
    pub(crate) cur: usize,
    /// Selected files (Ctrl/Shift+click in the file list) that Delete acts on.
    pub(crate) selected: std::collections::BTreeSet<usize>,
    /// Where a Shift selection started (file index).
    pub(crate) anchor: Option<usize>,
    /// Where the Shift selection made by the keys last ended (file index), while nothing else
    /// changed the selection or the search since: the selection is then that range.
    pub(crate) range_end: Option<usize>,
    /// The images of a layout's panes after the first, which shows `cur`.
    pub(crate) panes: Vec<usize>,
    pub(crate) slots: HashMap<usize, Slot>,
    pub(crate) decode_errors: HashMap<usize, String>,
    /// Decodes asked for, with the number of the job: only its result is taken, so one asked
    /// for before the list, the page, the look or the file changed is dropped; and the box it
    /// was asked for in.
    pub(crate) decodes_pending: HashMap<(usize, bool), (u64, PathBuf, [u32; 2])>,
    pub(crate) matches: Option<Vec<usize>>,
    /// Total size of the search matches, summed in the background like `total_size`.
    pub(crate) match_size: u64,
    /// A rectangle being dragged in the icon grid to select.
    pub(crate) rubber_band: Option<RubberBand>,
    /// Images of a collection being dragged in the file list, and the place they would go.
    pub(crate) dragging: Option<(HashSet<usize>, Option<usize>)>,
    pub(crate) last_slide: usize,
    /// The files whose cells pan view draws this frame: their thumbnails are made and kept,
    /// also while the file list shows others.
    pub(crate) pan_thumbs: Vec<usize>,
    /// File list size and date columns: (bytes, mtime) by file name.
    pub(crate) details: HashMap<OsString, (u64, i64)>,
    /// Goes up whenever the file list changes, so what was worked out from its indices can tell
    /// that it is old, also when the list kept its length (another order).
    pub(crate) version: usize,
}

/// A file of the list and its row among the search matches (`None`: not among them).
pub(crate) type FileRow = (usize, Option<usize>);
