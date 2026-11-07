//! The release tests' hook (feature `test-hooks`): `glimt --remote where LABEL` tells where the
//! menu item, button or choice with that text was drawn last frame, so the tests click by text and
//! need no screen coordinates that every change to the look would move. Widgets the tests need
//! are drawn through `noted`, which costs nothing in a build without the feature.

use eframe::egui;
#[cfg(feature = "test-hooks")]
use std::sync::Mutex;

#[cfg(feature = "test-hooks")]
static THIS_FRAME: Mutex<Vec<(String, egui::Rect)>> = Mutex::new(Vec::new());
#[cfg(feature = "test-hooks")]
static LAST_FRAME: Mutex<Vec<(String, egui::Rect)>> = Mutex::new(Vec::new());

/// A text drawn this frame, where it was.
#[cfg(feature = "test-hooks")]
pub(crate) fn note_label(label: &str, rect: egui::Rect) {
    THIS_FRAME.lock().unwrap().push((label.to_owned(), rect));
}

/// At the start of a frame: last frame's notes are the ones asked about.
#[cfg(feature = "test-hooks")]
pub(crate) fn begin_frame() {
    *LAST_FRAME.lock().unwrap() = std::mem::take(&mut *THIS_FRAME.lock().unwrap());
}

/// The answer to `where LABEL`: "x y" of the text's middle, in the window's pixels (the
/// tests' window is at the top left, unscaled), or nothing when it was not on the screen.
#[cfg(feature = "test-hooks")]
pub(crate) fn label_position(label: &str) -> Vec<u8> {
    let last = LAST_FRAME.lock().unwrap();
    match last.iter().rev().find(|(l, _)| *l == label) {
        Some((_, rect)) => {
            format!("{} {}\n", rect.center().x as i32, rect.center().y as i32).into()
        }
        None => Vec::new(),
    }
}

/// The text of a widget as the release tests name it: the label without the icon before it.
pub(crate) fn noted(response: &egui::Response, text: &str) -> egui::Response {
    #[cfg(feature = "test-hooks")]
    note_label(
        text.trim_start_matches(|c: char| ('\u{e000}'..='\u{f8ff}').contains(&c) || c == ' '),
        // The part that takes a click, so a row cut off at a scrolled list's edge is clicked
        // where it shows, not in its middle under the edge.
        response.interact_rect,
    );
    #[cfg(not(feature = "test-hooks"))]
    let _ = text;
    response.clone()
}
