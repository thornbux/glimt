//! Edit → Crop: a part of the shown image dragged out over it, saved as a new file beside it.

#[cfg(feature = "video")]
use crate::files::external::is_video;
use crate::files::file_ops::Deleted;
use crate::support::lang::tr;
use crate::tf;
use eframe::egui;
#[cfg(feature = "video")]
use std::path::Path;
use std::path::PathBuf;

/// The part being chosen of the image `path`, as fractions (0–1) of the image as shown, turned;
/// `None` until a drag. Held by its path and with the copy as it was shown (`how`), not by a
/// place in the list: a list read again, or a file deleted before it, would make that place
/// another image's. Another image, turn or page shown ends it.
pub struct Crop {
    path: PathBuf,
    how: crate::images::save::CopyAs,
    anchor: egui::Pos2,
    part: Option<egui::Rect>,
}

impl crate::App {
    /// Whether `crop` is still of what is shown: the same file, and shown the same in every way
    /// a copy takes from the view (turns, Auto Rotate, the page …), compared whole so a way
    /// added later cannot be left out of the check, as Auto Rotate first was.
    fn crop_shown(&self, crop: &Crop) -> bool {
        self.is_file(self.list.cur(), &crop.path) && self.shown_copy() == crop.how
    }

    /// Edit → Crop: choose a part of the shown image. Not for a video, nor before the image
    /// is decoded.
    pub(crate) fn start_crop(&mut self) {
        // The part is chosen over the image, so a crop from the grid in the image area opens it.
        self.panel.open_from_grid();
        let cur = self.list.cur();
        #[cfg(feature = "video")]
        if (self.list.files().get(cur)).is_some_and(|f| is_video(Path::new(f))) {
            self.notice = Some(tr("A video is not cropped").into());
            return;
        }
        if self.list.slots().contains_key(&cur) {
            self.viewing.start_crop(Crop {
                path: self.path(cur),
                how: self.shown_copy(),
                anchor: egui::Pos2::ZERO,
                part: None,
            });
        }
    }

    /// Over the shown image at `bounds` on screen, `pixels` large as shown: a drag chooses the
    /// part, drawn with the rest darkened, its size in pixels, and a bar to save or cancel.
    pub(crate) fn crop_input(
        &mut self,
        ctx: &egui::Context,
        ui: &egui::Ui,
        response: &egui::Response,
        bounds: egui::Rect,
        pixels: [usize; 2],
    ) {
        if self.viewing.crop().is_some_and(|c| !self.crop_shown(c)) {
            self.viewing.stop_crop();
        }
        let Some(crop) = self.viewing.crop_mut() else {
            return;
        };
        let at = |p: egui::Pos2| {
            let p = bounds.clamp(p);
            egui::pos2(
                (p.x - bounds.left()) / bounds.width(),
                (p.y - bounds.top()) / bounds.height(),
            )
        };
        if response.drag_started()
            && let Some(p) = ctx.input(|i| i.pointer.press_origin())
        {
            crop.anchor = at(p);
        }
        if response.dragged()
            && let Some(p) = response.interact_pointer_pos()
        {
            let part = egui::Rect::from_two_pos(crop.anchor, at(p));
            crop.part = (part.width() * bounds.width() >= 2.0
                && part.height() * bounds.height() >= 2.0)
                .then_some(part);
        }
        let p = ui.painter();
        let shade = egui::Color32::from_black_alpha(150);
        if let Some(part) = crop.part {
            let to_screen = |q: egui::Pos2| bounds.min + q.to_vec2() * bounds.size();
            let sel = egui::Rect::from_min_max(to_screen(part.min), to_screen(part.max));
            for r in [
                egui::Rect::from_x_y_ranges(bounds.x_range(), bounds.top()..=sel.top()),
                egui::Rect::from_x_y_ranges(bounds.x_range(), sel.bottom()..=bounds.bottom()),
                egui::Rect::from_x_y_ranges(bounds.left()..=sel.left(), sel.y_range()),
                egui::Rect::from_x_y_ranges(sel.right()..=bounds.right(), sel.y_range()),
            ] {
                p.rect_filled(r, 0.0, shade);
            }
            let stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
            p.rect_stroke(sel, 0.0, stroke, egui::StrokeKind::Outside);
            let [_, _, w, h] = part_pixels(part, pixels);
            let text = format!("{w} × {h}");
            let font = egui::FontId::proportional(14.0);
            let galley = p.layout_no_wrap(text, font, egui::Color32::WHITE);
            let at = sel.left_top() + egui::vec2(6.0, 6.0);
            p.rect_filled(
                egui::Rect::from_min_size(at, galley.size()).expand(3.0),
                3.0,
                shade,
            );
            p.galley(at, galley, egui::Color32::WHITE);
        }
        let chosen = crop.part.is_some();
        let (mut save, mut cancel) = (false, false);
        egui::Area::new(egui::Id::unique("crop bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(response.rect.center_top() + egui::vec2(0.0, 8.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("Drag over the image to choose the part to keep"));
                        save =
                            crate::ui::menus::button_enabled(ui, chosen, tr("Save Copy (Enter)"))
                                .clicked();
                        cancel = crate::ui::menus::button(ui, tr("Cancel (Esc)")).clicked();
                    });
                });
            });
        if save {
            self.save_crop(ctx);
        } else if cancel {
            self.viewing.stop_crop();
        }
    }

    /// Save the part chosen as a new file beside the image, in the background; the folder is
    /// read again to show it.
    pub(crate) fn save_crop(&mut self, ctx: &egui::Context) {
        let Some(crop) = self.viewing.take_crop().filter(|c| self.crop_shown(c)) else {
            return;
        };
        let (Some(part), Some(slot)) = (crop.part, self.list.slots().get(&self.list.cur())) else {
            return;
        };
        let [w, h] = slot.orig;
        let pixels = if crop.how.turns % 2 == 1 {
            [h, w]
        } else {
            [w, h]
        };
        let how = crate::images::save::CopyAs {
            crop: Some(part_pixels(part, pixels)),
            ..crop.how
        };
        let path = crop.path;
        let deleted = self.work.deleted_sender();
        self.background(ctx, vec![path.clone()], move || {
            let saved =
                crate::images::decode::survive(|| crate::images::save::save_cropped(&path, &how));
            Some(match saved {
                Ok(new) => {
                    // The copy gets the marks and stars of the original, as it got its
                    // sidecar, unless it was stripped.
                    let _ = deleted.send(Deleted {
                        relist: true,
                        moved: (!how.strip)
                            .then(|| (path.clone(), new.clone()))
                            .into_iter()
                            .collect(),
                        copied: true,
                        ..Default::default()
                    });
                    tf!("Saved {file}", file = new.display())
                }
                Err(e) => tf!("Could not crop the image: {e}", e),
            })
        });
    }
}

/// `part` (fractions of the image) in whole pixels of an image `pixels` large: x, y, width,
/// height, at least one pixel, within the image.
fn part_pixels(part: egui::Rect, [w, h]: [usize; 2]) -> [u32; 4] {
    let (w, h) = (w as f32, h as f32);
    let x = (part.left() * w).round().clamp(0.0, w - 1.0);
    let y = (part.top() * h).round().clamp(0.0, h - 1.0);
    let right = (part.right() * w).round().clamp(x + 1.0, w);
    let bottom = (part.bottom() * h).round().clamp(y + 1.0, h);
    [x as u32, y as u32, (right - x) as u32, (bottom - y) as u32]
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    /// Enter saves the part being chosen, but not while a text field has the focus, because
    /// the Enter then belongs to the field.
    #[test]
    fn enter_in_a_text_field_does_not_save_the_crop() {
        let dir = std::env::temp_dir().join(format!("glimt-test-crop-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let crop = super::Crop {
            path: dir.join("a.png"),
            how: app.shown_copy(),
            anchor: egui::Pos2::ZERO,
            part: None,
        };
        app.viewing.start_crop(crop);
        let enter = |app: &mut crate::App, typing: bool| {
            let input = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                let field = egui::Id::unique("a text field");
                match typing {
                    true => ui.memory_mut(|m| m.request_focus(field)),
                    false => ui.memory_mut(|m| m.surrender_focus(field)),
                }
                app.read_keys(ui.ctx());
            });
        };
        enter(&mut app, true);
        assert!(app.viewing.is_cropping());
        // Without the field, Enter takes the crop to save it (here a crop of nothing).
        enter(&mut app, false);
        assert!(!app.viewing.is_cropping());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// G for the grid in the image area ends a crop, so the Enter that opens an image from the
    /// grid never saves a part chosen before. Crop chosen in the grid opens the image.
    #[test]
    fn the_grid_ends_a_crop() {
        let dir = std::env::temp_dir().join(format!("glimt-test-crop-grid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, ctx) = crate::support::test_support::app_on(&dir);
        let crop = super::Crop {
            path: dir.join("a.png"),
            how: app.shown_copy(),
            anchor: egui::Pos2::ZERO,
            part: None,
        };
        app.viewing.start_crop(crop);
        app.run(&ctx, crate::ui::actions::Action::ThumbnailGrid);
        assert!(!app.viewing.is_cropping());
        // And Crop chosen in the grid opens the image, where the part is chosen.
        app.start_crop();
        assert!(app.panel.area_grid_on() && !app.panel.area_grid_shown());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A part in whole pixels, inside the image and never empty.
    #[test]
    fn rounds_the_part_to_pixels() {
        let r =
            |a: [f32; 4]| egui::Rect::from_min_max(egui::pos2(a[0], a[1]), egui::pos2(a[2], a[3]));
        assert_eq!(
            super::part_pixels(r([0.0, 0.0, 1.0, 1.0]), [300, 200]),
            [0, 0, 300, 200]
        );
        assert_eq!(
            super::part_pixels(r([0.25, 0.5, 0.75, 1.0]), [300, 200]),
            [75, 100, 150, 100]
        );
        assert_eq!(
            super::part_pixels(r([1.0, 1.0, 1.0, 1.0]), [300, 200]),
            [299, 199, 1, 1]
        );
    }

    /// A crop's copy gets the stars of the original, as its sidecar does.
    #[cfg(feature = "marks")]
    #[test]
    fn a_crop_gets_the_stars_of_its_image() {
        use crate::support::test_support::{app_on, settle};
        let dir =
            std::env::temp_dir().join(format!("glimt-test-crop-stars-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        image::RgbImage::new(8, 8).save(&a).unwrap();
        let (mut app, ctx) = app_on(&dir);
        assert!(settle(&mut app, &ctx, |a| a.list.slots().contains_key(&0)));
        app.marks.rate(std::slice::from_ref(&a), 4);
        app.start_crop();
        if let Some(crop) = app.viewing.crop_mut() {
            crop.part = Some(egui::Rect::from_min_max(
                egui::pos2(0.0, 0.0),
                egui::pos2(0.5, 0.5),
            ));
        }
        app.save_crop(&ctx);
        let new = dir.join("a-cropped.png");
        assert!(settle(&mut app, &ctx, |app| app.marks.get(&new).1 == 4));
        // Copied, not moved: the original keeps its stars.
        assert_eq!(app.marks.get(&a).1, 4);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
