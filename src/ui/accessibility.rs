//! Screen readers such as Orca (`cargo build --features accessibility`): eframe's AccessKit
//! gives them the menus, buttons, lists and dialogs; here the shown image gets a text, read
//! again each time the user browses, and the notices are read out as they come. Built without
//! the feature, none of this is in Glimt.

use crate::App;
use eframe::egui;

impl App {
    /// The shown image for a screen reader: its IPTC alt text, name, place in the list, stars,
    /// keywords, comment and extended description (which a plugin can fill). The image area
    /// keeps the focus while nothing else has it and no dialog is open, so browsing reads out
    /// the next image.
    pub fn speak_image(&mut self, ui: &egui::Ui, response: &egui::Response) {
        self.image_area_id = Some(response.id);
        let text = if self.list.files().is_empty() {
            crate::support::lang::tr("No images").to_owned()
        } else {
            // Read in the background, as for the info panel: here once for each image.
            self.request_and_take_exif(ui.ctx());
            let path = self.path(self.list.shown());
            let place = self.spoken_place();
            // The name the list shows, not the whole path a collection's entry holds.
            let mut text = format!("{}, {place}", self.name(self.list.shown()));
            if let Some(e) = self.list.decode_errors().get(&self.list.shown()) {
                text += &crate::tf!(", could not be shown: {e}", e);
            }
            #[cfg(feature = "marks")]
            if let stars @ 1.. = self.marks.get(&path).1 {
                text += &format!(
                    ", {}",
                    crate::support::format::count(stars as usize, "star")
                );
            }
            if let Some(meta) = self.info_panel.metadata_edit_of(&path) {
                if !meta.saved.keywords.is_empty() {
                    let keywords = meta.saved.keywords.join(", ");
                    text += &crate::tf!(", keywords: {keywords}", keywords);
                }
                if !meta.saved.comment.is_empty() {
                    text += &format!(". {}", meta.saved.comment);
                }
                if !meta.saved_alt_text.extended_description.is_empty() {
                    text += &format!(". {}", meta.saved_alt_text.extended_description);
                }
                // The IPTC alt text is written for this: read first, before the file name.
                if !meta.saved_alt_text.text.is_empty() {
                    text = format!("{}. {text}", meta.saved_alt_text.text);
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
            Some(m) => (self.shown_row(m), m.len()),
            None => (Some(self.list.shown()), self.list.files().len()),
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

/// Gives a screen reader `notice`, the text shown at the foot of `area`, as a status that it
/// reads out when the text changes, without moving the focus. The status is there in every
/// frame, empty while there is no notice, because a screen reader reads a change in a status
/// it knows, not always one that appears with its text. A notice given again with the same text
/// is read out again too. Its status then gains or loses a space at its end, because an
/// unchanged status is not read out.
pub(crate) fn speak_notice(ui: &egui::Ui, area: egui::Rect, notice: Option<&str>) {
    let id = egui::Id::unique("glimt-notice");
    // A notice given again is a new String, made while the one before is still held, so the
    // address of its text tells it from the notice already read out, also when the text is the
    // same.
    // shortcut: a notice taken away and given again with the same text within one frame may
    // get the old address back and is not read out again; give each notice a number of its own
    // if that is ever heard missing.
    let address = notice.map(|text| text.as_ptr() as usize);
    let (last_address, last_status) = (ui.ctx())
        .data(|d| d.get_temp::<(Option<usize>, String)>(id))
        .unwrap_or_default();
    let text = notice.unwrap_or_default();
    let status = if last_status.trim_end() != text {
        text.to_owned()
    } else if address == last_address {
        last_status
    } else if last_status.ends_with(' ') {
        text.to_owned()
    } else {
        format!("{text} ")
    };
    (ui.ctx()).data_mut(|d| d.insert_temp(id, (address, status.clone())));
    // A line along the area's foot, where the notice shows, and not an empty box, which a
    // screen reader may leave out as outside what it is in.
    let at = egui::Rect::from_min_max(
        area.left_bottom() - egui::vec2(0.0, 1.0),
        area.right_bottom(),
    );
    let response = ui.interact(at, id, egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Status, true, &status));
    (ui.ctx()).accesskit_node_builder(id, |node| node.set_live(egui::accesskit::Live::Polite));
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
        app.list.set_shown(2);
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

    /// A notice is a status that a screen reader reads out as it comes, over the image and over
    /// the grid in the image area, and the status stays, empty, while there is no notice.
    #[test]
    fn notices_are_read_out() {
        let name = format!("glimt-test-speak-notice-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = app_on(&dir);
        (app.list).set_files(["a.png", "b.png"].map(Into::into).to_vec());
        ctx.enable_accesskit();
        let status = |app: &mut crate::App, grid: bool| {
            if grid != app.panel.is_area_grid_shown() {
                app.panel.toggle_area_grid();
            }
            let mut found = None;
            for _ in 0..2 {
                let out = ctx.run_ui(Default::default(), |ui| match grid {
                    true => app.area_grid(&ctx, ui),
                    false => app.image_area(&ctx, ui, false, true),
                });
                let update = out.platform_output.accesskit_update.map(|u| u.nodes);
                for (_, node) in update.into_iter().flatten() {
                    if node.role() == egui::Role::Status {
                        let live = node.live() == Some(egui::accesskit::Live::Polite);
                        found = Some((node.label().unwrap_or_default().to_owned(), live));
                    }
                }
            }
            found
        };
        assert_eq!(status(&mut app, false), Some((String::new(), true)));
        app.notice = Some("Copied 1 file".into());
        assert_eq!(
            status(&mut app, false),
            Some(("Copied 1 file".into(), true))
        );
        app.notice = Some("Moved 1 file".into());
        assert_eq!(status(&mut app, true), Some(("Moved 1 file".into(), true)));
        // The same notice given again is read out again: its status changes, but not in the
        // frames that only show it, and back and forth for each time it is given.
        let mut heard = vec!["Moved 1 file".to_owned()];
        for _ in 0..2 {
            app.notice = Some("Moved 1 file".into());
            let (again, _) = status(&mut app, true).unwrap();
            assert_eq!(again.trim_end(), "Moved 1 file");
            assert_ne!(
                Some(&again),
                heard.last(),
                "the same notice was not read out again"
            );
            heard.push(again);
            assert_eq!(status(&mut app, true).map(|s| s.0).as_ref(), heard.last());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
