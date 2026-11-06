//! Screen readers such as Orca (`cargo build --features accessibility`): eframe's AccessKit
//! gives them the menus, buttons, lists and dialogs; here the shown image gets a text, read
//! again each time the user browses. Built without the feature, none of this is in Glimt.

use crate::App;
use eframe::egui;

impl App {
    /// The shown image for a screen reader: its IPTC alt text, name, place in the list, stars,
    /// keywords, comment and extended description (which a plugin can fill). The image area
    /// keeps the focus while nothing else has it and no dialog is open, so browsing reads out
    /// the next image.
    pub fn speak_image(&mut self, ui: &egui::Ui, response: &egui::Response) {
        self.image_id = Some(response.id);
        let text = if self.list.files().is_empty() {
            crate::support::lang::tr("No images").to_owned()
        } else {
            // Read in the background, as for the EXIF panel: here once for each image.
            self.want_exif(ui.ctx());
            let path = self.path(self.list.cur());
            let place = self.spoken_place();
            // The name the list shows, not the whole path a collection's entry holds.
            let mut text = format!("{}, {place}", self.name(self.list.cur()));
            if let Some(e) = self.list.decode_errors().get(&self.list.cur()) {
                text += &crate::tf!(", could not be shown: {e}", e);
            }
            #[cfg(feature = "marks")]
            if let stars @ 1.. = self.marks.get(&path).1 {
                text += &format!(
                    ", {}",
                    crate::support::format::count(stars as usize, "star")
                );
            }
            if let Some(meta) = self.info.meta_of(&path) {
                if !meta.saved.keywords.is_empty() {
                    let keywords = meta.saved.keywords.join(", ");
                    text += &crate::tf!(", keywords: {keywords}", keywords);
                }
                if !meta.saved.comment.is_empty() {
                    text += &format!(". {}", meta.saved.comment);
                }
                if !meta.access.extended.is_empty() {
                    text += &format!(". {}", meta.access.extended);
                }
                // The IPTC alt text is written for this: read first, before the file name.
                if !meta.access.alt.is_empty() {
                    text = format!("{}. {text}", meta.access.alt);
                }
            }
            text
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &text));
        // A dialog's text field takes the focus when nothing has it: never the image's then.
        if self.dialog_open() {
            response.surrender_focus();
        } else if ui.memory(|m| m.focused().is_none()) {
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

    /// The shown image's place in the list as a screen reader says it, such as "3 of 16". A
    /// search can hide the shown image, which then has no row, so the reader hears that it is
    /// not among the matches instead of the dash the status bar shows.
    pub(crate) fn spoken_place(&self) -> String {
        use crate::support::format::thousands;
        let (row, total) = match self.list.matches() {
            Some(m) => (self.cur_row(m), m.len()),
            None => (Some(self.list.cur()), self.list.files().len()),
        };
        match row {
            Some(row) => crate::tf!(
                "{done} of {total}",
                done = thousands(row + 1),
                total = thousands(total)
            ),
            None => crate::support::lang::tr("not among the matches").to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::support::test_support::app_on;
    use eframe::egui;

    /// The place read out is the row among the matches, and a shown image the search hides is
    /// said to be outside them, not "– of 3".
    #[test]
    fn a_hidden_image_is_not_among_the_matches() {
        let dir = std::env::temp_dir().join(format!("glimt-test-speak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, _ctx) = app_on(&dir);
        app.list
            .set_files((0..5).map(|i| format!("{i}.png").into()).collect());
        assert_eq!(app.spoken_place(), "1 of 5");
        app.list.set_matches(Some(vec![1, 2, 3]));
        assert_eq!(app.spoken_place(), "not among the matches");
        app.list.set_cur(2);
        assert_eq!(app.spoken_place(), "2 of 3");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The grid in the image area reads out the image shown, as the image does, so a move in it
    /// with the arrows is heard, and each of its cells says which file it is.
    #[test]
    fn the_grid_in_the_image_area_speaks() {
        let name = format!("glimt-test-speak-grid-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        (app.list).set_files(["a.png", "b.png", "c.png"].map(Into::into).to_vec());
        app.panel.toggle_area_grid();
        ctx.enable_accesskit();
        let mut nodes = Vec::new();
        for _ in 0..2 {
            let out = ctx.run_ui(Default::default(), |ui| app.area_grid(&ctx, ui));
            let update = out.platform_output.accesskit_update.map(|u| u.nodes);
            nodes.extend(update.into_iter().flatten().map(|(_, n)| n));
        }
        let said = |role, text| (nodes.iter()).any(|n| n.role() == role && n.label() == Some(text));
        assert!(said(egui::Role::Image, "a.png, 1 of 3"));
        assert!(said(egui::Role::ListItem, "b.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
