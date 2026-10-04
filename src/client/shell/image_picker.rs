//! Attaching image files to a pane (`Attach image…` in the pane menu): a
//! list of the images in a directory on this client's machine, newest first,
//! where several can be marked and attached at once, oldest first. Each goes
//! the way a pasted clipboard image does: the client reads the file, the
//! server stages it and pastes its path into the pane, which works the same
//! for a remote server.
//!
//! The directory is where the system saves screenshots. The pane is fixed
//! when the list opens, so moving the focus meanwhile never redirects it.
//! The directory is read only when the list opens, on the user's action.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::render::{display_width, put_text};
use super::*;

/// Images listed; older ones are left out.
const MAX_IMAGES: usize = 200;
const PICKER_WIDTH: u16 = 72;
/// Rows of images shown at once.
const VISIBLE_ROWS: usize = 14;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImageEntry {
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) modified: SystemTime,
}

#[derive(Debug)]
pub(super) struct ImagePickerOverlay {
    /// The pane the images go to, fixed when the list opened.
    pub(super) pane_id: String,
    pub(super) dir: PathBuf,
    /// Newest first.
    pub(super) entries: Vec<ImageEntry>,
    pub(super) highlighted: usize,
    pub(super) marked: BTreeSet<usize>,
    pub(super) scroll: usize,
    /// Why the directory could not be listed.
    pub(super) error: Option<String>,
}

/// The images to attach to a pane.
#[derive(Debug)]
pub(crate) struct AttachImages {
    pub(crate) pane_id: String,
    /// Oldest first, the order they are pasted in.
    pub(crate) paths: Vec<PathBuf>,
}

fn image_extension(name: &str) -> bool {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
    )
}

/// The images in `dir`, newest first. Hidden files are left out: macOS
/// writes a screenshot to a hidden name and renames it when it is complete.
pub(super) fn list_images(dir: &Path) -> Result<Vec<ImageEntry>, String> {
    let entries = std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut images = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !image_extension(&name) {
                return None;
            }
            let metadata = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some(ImageEntry {
                path: entry.path(),
                name,
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            })
        })
        .collect::<Vec<_>>();
    images.sort_by_key(|image| std::cmp::Reverse(image.modified));
    images.truncate(MAX_IMAGES);
    Ok(images)
}

/// `12s`, `5m`, `3h`, `2d`: how long ago.
pub(super) fn age(modified: SystemTime, now: SystemTime) -> String {
    let seconds = now
        .duration_since(modified)
        .map(|age| age.as_secs())
        .unwrap_or(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// The file's first bytes are an image's: the extension alone could lie.
pub(crate) fn has_image_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"BM")
        || (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

impl ClientShellState {
    /// Opens the image list for `pane_id` in the screenshot directory.
    pub(super) fn open_image_picker(&mut self, pane_id: String) {
        let dir = crate::platform::screenshot_dir().unwrap_or_else(|| PathBuf::from("."));
        self.open_image_picker_in(pane_id, dir);
    }

    pub(super) fn open_image_picker_in(&mut self, pane_id: String, dir: PathBuf) {
        let (entries, error) = match list_images(&dir) {
            Ok(entries) => (entries, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        self.overlay = Some(ClientShellOverlay::ImagePicker(ImagePickerOverlay {
            pane_id,
            dir,
            entries,
            highlighted: 0,
            marked: BTreeSet::new(),
            scroll: 0,
            error,
        }));
    }

    pub(super) fn image_picker_key(
        &mut self,
        code: crossterm::event::KeyCode,
        outcome: &mut ClientShellInput,
    ) {
        use crossterm::event::KeyCode;
        outcome.repaint = true;
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Enter => self.attach_picked_images(outcome),
            KeyCode::Up | KeyCode::Char('k') => self.move_image_picker(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_image_picker(1),
            KeyCode::PageUp => self.move_image_picker(-(VISIBLE_ROWS as isize)),
            KeyCode::PageDown => self.move_image_picker(VISIBLE_ROWS as isize),
            KeyCode::Char(' ') => {
                if let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() {
                    let index = picker.highlighted;
                    if index < picker.entries.len() && !picker.marked.remove(&index) {
                        picker.marked.insert(index);
                    }
                }
                self.move_image_picker(1);
            }
            KeyCode::Char('a') => self.mark_images_since_last_attach(),
            _ => {}
        }
    }

    fn move_image_picker(&mut self, delta: isize) {
        let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        let last = picker.entries.len().saturating_sub(1) as isize;
        picker.highlighted = (picker.highlighted as isize + delta).clamp(0, last.max(0)) as usize;
        if picker.highlighted < picker.scroll {
            picker.scroll = picker.highlighted;
        } else if picker.highlighted >= picker.scroll + VISIBLE_ROWS {
            picker.scroll = picker.highlighted + 1 - VISIBLE_ROWS;
        }
    }

    /// A click on a row marks it, or unmarks it.
    pub(super) fn click_image_picker_row(&mut self, index: usize) {
        if let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() {
            if index < picker.entries.len() {
                picker.highlighted = index;
                if !picker.marked.remove(&index) {
                    picker.marked.insert(index);
                }
            }
        }
    }

    /// Marks every image newer than the last attach to this pane (all of
    /// them before the first attach): the shots taken for the current task.
    fn mark_images_since_last_attach(&mut self) {
        let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        let since = self.image_attach_times.get(&picker.pane_id).copied();
        picker.marked = picker
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| since.is_none_or(|since| entry.modified > since))
            .map(|(index, _)| index)
            .collect();
    }

    /// Attaches the marked images, or the highlighted one, oldest first.
    pub(super) fn attach_picked_images(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.take() else {
            return;
        };
        outcome.repaint = true;
        let mut picked = if picker.marked.is_empty() {
            picker
                .entries
                .get(picker.highlighted)
                .cloned()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            picker
                .marked
                .iter()
                .filter_map(|index| picker.entries.get(*index).cloned())
                .collect()
        };
        if picked.is_empty() {
            return;
        }
        picked.sort_by_key(|entry| entry.modified);
        if let Some(newest) = picked.last() {
            self.image_attach_times
                .insert(picker.pane_id.clone(), newest.modified);
        }
        outcome
            .actions
            .push(ClientShellAction::AttachImages(AttachImages {
                pane_id: picker.pane_id,
                paths: picked.into_iter().map(|entry| entry.path).collect(),
            }));
    }
}

/// Last attach time of an image per pane, for "new since the last attach".
pub(super) type ImageAttachTimes = HashMap<String, SystemTime>;

pub(super) fn render_image_picker(
    b: &mut Buffer,
    picker: &ImagePickerOverlay,
    now: SystemTime,
    p: &Palette,
) -> Option<super::render::OverlayRender> {
    let rows = picker.entries.len().clamp(1, VISIBLE_ROWS) as u16;
    // Border, header, gap, rows, gap, footer, border.
    let outer = super::render::popup_area(b.area, PICKER_WIDTH, rows.saturating_add(6))?;
    let inner = super::render::panel_area(b, outer, p.accent, p.panel_bg)?;
    let base = Style::default().bg(p.panel_bg);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dir = match home
        .as_deref()
        .and_then(|home| picker.dir.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => picker.dir.display().to_string(),
    };
    put_text(
        b,
        inner.x.saturating_add(1),
        inner.y,
        inner.width.saturating_sub(2),
        &format!("attach image · {dir}"),
        base.fg(p.text).add_modifier(Modifier::BOLD),
    );
    let mut menu_rows = Vec::new();
    let body_y = inner.y.saturating_add(2);
    if let Some(error) = picker.error.as_deref() {
        put_text(
            b,
            inner.x.saturating_add(1),
            body_y,
            inner.width.saturating_sub(2),
            error,
            base.fg(p.red),
        );
    } else if picker.entries.is_empty() {
        put_text(
            b,
            inner.x.saturating_add(1),
            body_y,
            inner.width.saturating_sub(2),
            "no images here",
            base.fg(p.overlay0),
        );
    }
    for (row, (index, entry)) in picker
        .entries
        .iter()
        .enumerate()
        .skip(picker.scroll)
        .take(VISIBLE_ROWS)
        .enumerate()
    {
        let y = body_y.saturating_add(row as u16);
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let highlighted = index == picker.highlighted;
        let style = if highlighted {
            base.bg(p.surface1).fg(p.text)
        } else {
            base.fg(p.subtext0)
        };
        b.set_style(rect, style);
        let mark = if picker.marked.contains(&index) {
            "[x]"
        } else {
            "[ ]"
        };
        let age = age(entry.modified, now);
        let age_width = display_width(&age);
        let name_width = inner.width.saturating_sub(age_width + 8);
        put_text(b, inner.x.saturating_add(1), y, 3, mark, style.fg(p.accent));
        put_text(
            b,
            inner.x.saturating_add(5),
            y,
            name_width,
            &crate::ui::truncate_end(&entry.name, usize::from(name_width)),
            style,
        );
        put_text(
            b,
            inner.right().saturating_sub(age_width + 1),
            y,
            age_width,
            &age,
            style.fg(p.overlay1),
        );
        menu_rows.push((rect, index));
    }
    let footer_y = inner.bottom().saturating_sub(1);
    let count = picker
        .marked
        .len()
        .max(usize::from(!picker.entries.is_empty()));
    let label = format!(" attach {count} ");
    let label_width = display_width(&label).min(inner.width);
    let primary = Rect::new(
        inner.right().saturating_sub(label_width),
        footer_y,
        label_width,
        1,
    );
    put_text(
        b,
        inner.x.saturating_add(1),
        footer_y,
        inner.width.saturating_sub(label_width + 2),
        "space mark · a new since last · enter attach · esc",
        base.fg(p.overlay0),
    );
    if !picker.entries.is_empty() {
        put_text(
            b,
            primary.x,
            primary.y,
            primary.width,
            &label,
            Style::default()
                .fg(p.panel_bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        );
    }
    Some(super::render::OverlayRender {
        area: outer,
        menu_rows,
        primary,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_images_newest_first_without_hidden_or_other_files() {
        let dir = std::env::temp_dir().join(format!("herdr-image-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, age_secs: u64| {
            let path = dir.join(name);
            std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
            let time = SystemTime::now() - std::time::Duration::from_secs(age_secs);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(time)
                .unwrap();
        };
        write("old.png", 300);
        write("new.PNG", 10);
        write(".Screenshot in progress.png", 1);
        write("notes.txt", 5);
        let names = list_images(&dir)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["new.PNG", "old.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ages_and_magic_bytes() {
        let now = SystemTime::now();
        let ago = |secs| now - std::time::Duration::from_secs(secs);
        assert_eq!(age(ago(12), now), "12s");
        assert_eq!(age(ago(300), now), "5m");
        assert_eq!(age(ago(3 * 3600), now), "3h");
        assert_eq!(age(ago(2 * 86_400), now), "2d");
        assert!(has_image_magic(b"\x89PNG\r\n\x1a\nrest"));
        assert!(has_image_magic(b"\xff\xd8\xff\xe0"));
        assert!(has_image_magic(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!has_image_magic(b"#!/bin/sh"));
    }

    #[test]
    fn marks_the_shots_since_the_last_attach_and_attaches_oldest_first() {
        let dir = std::env::temp_dir().join(format!("herdr-image-attach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, age_secs: u64| {
            let path = dir.join(name);
            std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - std::time::Duration::from_secs(age_secs))
                .unwrap();
        };
        write("a.png", 300);
        write("b.png", 200);
        let mut state = ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        let attached = |state: &mut ClientShellState| {
            let mut outcome = ClientShellInput::default();
            state.image_picker_key(crossterm::event::KeyCode::Char('a'), &mut outcome);
            state.image_picker_key(crossterm::event::KeyCode::Enter, &mut outcome);
            outcome
                .actions
                .into_iter()
                .find_map(|action| match action {
                    ClientShellAction::AttachImages(attach) => Some(attach),
                    _ => None,
                })
                .expect("an attach")
        };
        state.open_image_picker_in("pane_1".into(), dir.clone());
        let first = attached(&mut state);
        assert_eq!(first.pane_id, "pane_1");
        assert_eq!(first.paths, [dir.join("a.png"), dir.join("b.png")]);
        assert!(state.overlay.is_none());

        // A new shot: only it is new since the last attach.
        write("c.png", 10);
        state.open_image_picker_in("pane_1".into(), dir.clone());
        assert_eq!(attached(&mut state).paths, [dir.join("c.png")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
