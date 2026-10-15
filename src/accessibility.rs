//! Screen readers such as Orca (`cargo build --features accessibility`): eframe's AccessKit
//! gives them the menus, buttons, lists and dialogs; here the shown image gets a text, read
//! again each time the user browses. Built without the feature, none of this is in Glimt.

use crate::App;
use eframe::egui;

impl App {
    /// The shown image for a screen reader: its name, place in the list, stars, keywords and
    /// comment (which a plugin can fill with a description). The image area keeps the focus
    /// while nothing else has it, so browsing reads out the next image.
    pub fn speak_image(&mut self, ui: &egui::Ui, response: &egui::Response) {
        self.image_id = Some(response.id);
        let text = if self.files.is_empty() {
            "No images".to_owned()
        } else {
            // Read in the background, as for the EXIF panel: here once for each image.
            self.want_exif();
            let path = self.path(self.cur);
            let place = self.place().trim_matches(['[', ']']).replace('/', " of ");
            let mut text = format!("{}, {place}", self.files[self.cur].to_string_lossy());
            if let Some(e) = self.errors.get(&self.cur) {
                text += &format!(", could not be shown: {e}");
            }
            #[cfg(feature = "marks")]
            if let stars @ 1.. = self.marks.get(&path).1 {
                text += &format!(", {}", crate::count(stars as usize, "star"));
            }
            if let Some(meta) = self.meta.as_ref().filter(|m| m.path == path) {
                if !meta.saved.keywords.is_empty() {
                    text += &format!(", keywords: {}", meta.saved.keywords.join(", "));
                }
                if !meta.saved.comment.is_empty() {
                    text += &format!(". {}", meta.saved.comment);
                }
            }
            text
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &text));
        if ui.memory(|m| m.focused().is_none()) {
            response.request_focus();
        }
        if response.has_focus() {
            // The arrow keys browse, as without a screen reader; Tab still moves to the panels.
            let keys = egui::EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                ..Default::default()
            };
            ui.memory_mut(|m| m.set_focus_lock_filter(response.id, keys));
        }
    }
}
