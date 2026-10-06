//! Attaching image files to a pane (`Attach image…` in the pane menu): a
//! list of the images in a directory on this client's machine, newest first,
//! where several can be marked and attached at once, oldest first. Each goes
//! the way a pasted clipboard image does: the client reads the file, the
//! server stages it and pastes its path into the pane, which works the same
//! for a remote server.
//!
//! The list opens in the directory of the last attach, else where the
//! system saves screenshots. It is a small file browser: `..` goes up, the
//! directories images were last attached from (and the screenshot one) are
//! shortcuts at the top, subdirectories come after the images, which are
//! what is picked. Marks are cleared on a directory change, so no unseen
//! file is attached. The pane is fixed when the list opens, so moving the
//! focus meanwhile never redirects it. A directory is read only on the
//! user's action.
//! Space shows the highlighted image in Quick Look, as in Finder; the
//! runtime owns that panel and closes it when the list closes.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::render::{display_width, put_text};
use super::*;

/// Images listed, and subdirectories; older images and later names are
/// left out.
const MAX_IMAGES: usize = 200;
const MAX_DIRS: usize = 200;
/// Directories images were attached from, kept in the client preferences,
/// and how many of them the list offers.
const MAX_RECENT_DIRS: usize = 8;
const MAX_SHORTCUTS: usize = 5;
const PICKER_WIDTH: u16 = 72;
/// Rows of images shown at once.
const VISIBLE_ROWS: usize = 14;
/// Columns the preview of the highlighted image adds, and the fewest it
/// is shown with.
const PREVIEW_WIDTH: u16 = 48;
const MIN_PREVIEW_WIDTH: u16 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntryKind {
    Image,
    /// `..`, the directory above.
    Parent,
    /// A directory images were attached from, or the screenshot one.
    Shortcut,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImageEntry {
    pub(super) kind: EntryKind,
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) modified: SystemTime,
    /// Width and height in pixels, read from a PNG's header; the preview
    /// shows PNG files only, which the terminal decodes itself.
    pub(super) size_px: Option<(u32, u32)>,
}

#[derive(Debug)]
pub(super) struct ImagePickerOverlay {
    /// The pane the images go to, fixed when the list opened.
    pub(super) pane_id: String,
    pub(super) dir: PathBuf,
    /// The directories offered at the top wherever the list goes.
    pub(super) shortcuts: Vec<PathBuf>,
    /// `..`, the shortcuts, the images newest first, the subdirectories.
    pub(super) entries: Vec<ImageEntry>,
    pub(super) highlighted: usize,
    pub(super) marked: BTreeSet<usize>,
    pub(super) scroll: usize,
    /// Why the directory could not be listed.
    pub(super) error: Option<String>,
    /// Why the last preview failed, shown in place of the key hints.
    pub(super) notice: Option<String>,
    /// The highlighted image is shown beside the list.
    pub(super) preview: bool,
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

/// The rows for `dir`: `..`, the shortcuts other than `dir`, the images
/// newest first, then the subdirectories by name. Hidden files are left
/// out: macOS writes a screenshot to a hidden name and renames it when it
/// is complete. The error says why `dir` could not be read; the rows to
/// leave it are there anyway.
pub(super) fn list_entries(dir: &Path, shortcuts: &[PathBuf]) -> (Vec<ImageEntry>, Option<String>) {
    let navigation = |kind, path: &Path, name: String| ImageEntry {
        kind,
        path: path.to_path_buf(),
        name,
        modified: SystemTime::UNIX_EPOCH,
        size_px: None,
    };
    let mut rows = Vec::new();
    if let Some(parent) = dir.parent() {
        rows.push(navigation(EntryKind::Parent, parent, "..".to_owned()));
    }
    rows.extend(
        shortcuts
            .iter()
            .filter(|shortcut| shortcut.as_path() != dir)
            .map(|shortcut| navigation(EntryKind::Shortcut, shortcut, tilde(shortcut))),
    );
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => return (rows, Some(format!("{}: {err}", tilde(dir)))),
    };
    let mut images = Vec::new();
    let mut dirs = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let is_image = image_extension(&name);
        // Symlinks are followed, so a linked directory opens like one.
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if metadata.is_dir() {
            dirs.push(navigation(EntryKind::Directory, &entry.path(), name));
        } else if is_image && metadata.is_file() {
            images.push(ImageEntry {
                kind: EntryKind::Image,
                path: entry.path(),
                size_px: None,
                name,
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
    images.sort_by_key(|image| std::cmp::Reverse(image.modified));
    images.truncate(MAX_IMAGES);
    // Headers of the listed files only: a big directory is not opened file
    // by file.
    for image in &mut images {
        image.size_px = png_size(&image.path);
    }
    dirs.sort_by_key(|dir| dir.name.to_lowercase());
    dirs.truncate(MAX_DIRS);
    rows.extend(images);
    rows.extend(dirs);
    (rows, None)
}

/// `path` with the home directory as `~`.
fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A PNG file's width and height from its `IHDR` chunk.
fn png_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut header = [0u8; 24];
    std::fs::File::open(path)
        .ok()?
        .read_exact(&mut header)
        .ok()?;
    if !header.starts_with(b"\x89PNG\r\n\x1a\n") || &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
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
    /// Opens the image list for `pane_id` in the directory of the last
    /// attach, else the screenshot directory.
    pub(super) fn open_image_picker(&mut self, pane_id: String) {
        let screenshots = crate::platform::screenshot_dir();
        let dir = self
            .recent_image_dirs
            .iter()
            .find(|dir| dir.is_dir())
            .cloned()
            .or_else(|| screenshots.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let mut shortcuts = Vec::new();
        for shortcut in self.recent_image_dirs.iter().chain(screenshots.as_ref()) {
            if shortcuts.len() < MAX_SHORTCUTS && !shortcuts.contains(shortcut) && shortcut.is_dir()
            {
                shortcuts.push(shortcut.clone());
            }
        }
        self.open_image_picker_in(pane_id, dir, shortcuts);
    }

    pub(super) fn open_image_picker_in(
        &mut self,
        pane_id: String,
        dir: PathBuf,
        shortcuts: Vec<PathBuf>,
    ) {
        self.overlay = Some(ClientShellOverlay::ImagePicker(ImagePickerOverlay {
            pane_id,
            dir: PathBuf::new(),
            shortcuts,
            entries: Vec::new(),
            highlighted: 0,
            marked: BTreeSet::new(),
            scroll: 0,
            error: None,
            notice: None,
            preview: self.image_previews,
        }));
        self.enter_image_dir(dir);
    }

    /// Lists `dir` in the open image list, the newest image highlighted.
    fn enter_image_dir(&mut self, dir: PathBuf) {
        let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        let (entries, error) = list_entries(&dir, &picker.shortcuts);
        picker.highlighted = entries
            .iter()
            .position(|entry| entry.kind == EntryKind::Image)
            .unwrap_or(0);
        picker.dir = dir;
        picker.entries = entries;
        picker.error = error;
        picker.marked.clear();
        picker.scroll = 0;
        self.move_image_picker(0);
    }

    /// Goes into the highlighted directory row; returns whether it was one.
    fn open_highlighted_image_dir(&mut self) -> bool {
        let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_ref() else {
            return false;
        };
        let Some(entry) = picker
            .entries
            .get(picker.highlighted)
            .filter(|entry| entry.kind != EntryKind::Image)
        else {
            return false;
        };
        let dir = entry.path.clone();
        self.enter_image_dir(dir);
        true
    }

    fn image_parent_dir(&mut self) {
        let parent = match self.overlay.as_ref() {
            Some(ClientShellOverlay::ImagePicker(picker)) => {
                picker.dir.parent().map(Path::to_path_buf)
            }
            _ => None,
        };
        if let Some(parent) = parent {
            self.enter_image_dir(parent);
        }
    }

    pub(super) fn image_picker_key(
        &mut self,
        code: crossterm::event::KeyCode,
        outcome: &mut ClientShellInput,
    ) {
        use crossterm::event::KeyCode;
        outcome.repaint = true;
        if let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() {
            picker.notice = None;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Enter => {
                let marked = matches!(
                    self.overlay.as_ref(),
                    Some(ClientShellOverlay::ImagePicker(picker)) if !picker.marked.is_empty()
                );
                if marked || !self.open_highlighted_image_dir() {
                    self.attach_picked_images(outcome);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.open_highlighted_image_dir();
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.image_parent_dir(),
            KeyCode::Up | KeyCode::Char('k') => self.move_image_picker(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_image_picker(1),
            KeyCode::PageUp => self.move_image_picker(-(VISIBLE_ROWS as isize)),
            KeyCode::PageDown => self.move_image_picker(VISIBLE_ROWS as isize),
            KeyCode::Char(' ') => {
                if let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_ref() {
                    if let Some(entry) = picker
                        .entries
                        .get(picker.highlighted)
                        .filter(|entry| entry.kind == EntryKind::Image)
                    {
                        outcome
                            .actions
                            .push(ClientShellAction::PreviewImage(entry.path.clone()));
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() {
                    let index = picker.highlighted;
                    let image = picker
                        .entries
                        .get(index)
                        .is_some_and(|entry| entry.kind == EntryKind::Image);
                    if image && !picker.marked.remove(&index) {
                        picker.marked.insert(index);
                    }
                }
                self.move_image_picker(1);
            }
            KeyCode::Char('a') => self.mark_images_since_last_attach(),
            _ => {}
        }
    }

    pub(crate) fn image_picker_open(&self) -> bool {
        matches!(self.overlay, Some(ClientShellOverlay::ImagePicker(_)))
    }

    /// Shows why a preview could not open; returns whether to repaint.
    pub(crate) fn image_preview_failed(&mut self, notice: String) -> bool {
        let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() else {
            return false;
        };
        picker.notice = Some(notice);
        true
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

    /// A click on an image marks it, or unmarks it; on a directory it goes
    /// there.
    pub(super) fn click_image_picker_row(&mut self, index: usize) {
        let Some(ClientShellOverlay::ImagePicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        let Some(kind) = picker.entries.get(index).map(|entry| entry.kind) else {
            return;
        };
        picker.highlighted = index;
        if kind != EntryKind::Image {
            self.open_highlighted_image_dir();
        } else if !picker.marked.remove(&index) {
            picker.marked.insert(index);
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
            .filter(|(_, entry)| {
                entry.kind == EntryKind::Image && since.is_none_or(|since| entry.modified > since)
            })
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
        picked.retain(|entry| entry.kind == EntryKind::Image);
        if picked.is_empty() {
            return;
        }
        self.recent_image_dirs.retain(|dir| *dir != picker.dir);
        self.recent_image_dirs.insert(0, picker.dir.clone());
        self.recent_image_dirs.truncate(MAX_RECENT_DIRS);
        self.persist_chrome_preferences(outcome);
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
    let has_images = picker
        .entries
        .iter()
        .any(|entry| entry.kind == EntryKind::Image);
    let with_preview = picker.preview
        && has_images
        && b.area.width.saturating_sub(4) >= PICKER_WIDTH + MIN_PREVIEW_WIDTH;
    let (width, rows) = if with_preview {
        (PICKER_WIDTH + PREVIEW_WIDTH, VISIBLE_ROWS as u16)
    } else {
        (
            PICKER_WIDTH,
            (picker.entries.len() + usize::from(!has_images)).clamp(1, VISIBLE_ROWS) as u16,
        )
    };
    // Border, header, gap, rows, gap, footer, border.
    let outer = super::render::popup_area(b.area, width, rows.saturating_add(6))?;
    let inner = super::render::panel_area(b, outer, p.accent, p.panel_bg)?;
    let base = Style::default().bg(p.panel_bg);
    let dir = tilde(&picker.dir);
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
    // The list keeps its own width; the preview takes the rest, a column
    // apart, as high as the rows.
    let list = if with_preview {
        Rect::new(
            inner.x,
            inner.y,
            PICKER_WIDTH.saturating_sub(2),
            inner.height,
        )
    } else {
        inner
    };
    let image_preview = if with_preview {
        let x = list.right().saturating_add(1);
        Rect::new(
            x,
            body_y,
            inner.right().saturating_sub(x).saturating_sub(1),
            rows.min(inner.bottom().saturating_sub(body_y).saturating_sub(2)),
        )
    } else {
        Rect::default()
    };
    if let Some(error) = picker.error.as_deref() {
        put_text(
            b,
            inner.x.saturating_add(1),
            body_y,
            inner.width.saturating_sub(2),
            error,
            base.fg(p.red),
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
        let rect = Rect::new(list.x, y, list.width, 1);
        let highlighted = index == picker.highlighted;
        let style = if highlighted {
            base.bg(p.surface1).fg(p.text)
        } else {
            base.fg(p.subtext0)
        };
        b.set_style(rect, style);
        let (mark, name) = match entry.kind {
            EntryKind::Image if picker.marked.contains(&index) => ("[x]", entry.name.clone()),
            EntryKind::Image => ("[ ]", entry.name.clone()),
            EntryKind::Parent => (" ↑ ", entry.name.clone()),
            EntryKind::Shortcut => (" ↺ ", entry.name.clone()),
            EntryKind::Directory => (" ▸ ", format!("{}/", entry.name)),
        };
        let age = if entry.kind == EntryKind::Image {
            age(entry.modified, now)
        } else {
            String::new()
        };
        let age_width = display_width(&age);
        let name_width = list.width.saturating_sub(age_width + 8);
        put_text(b, list.x.saturating_add(1), y, 3, mark, style.fg(p.accent));
        put_text(
            b,
            list.x.saturating_add(5),
            y,
            name_width,
            &crate::ui::truncate_end(&name, usize::from(name_width)),
            style,
        );
        put_text(
            b,
            list.right().saturating_sub(age_width + 1),
            y,
            age_width,
            &age,
            style.fg(p.overlay1),
        );
        menu_rows.push((rect, index));
    }
    if picker.error.is_none() && !has_images {
        let y = body_y.saturating_add(
            picker
                .entries
                .len()
                .saturating_sub(picker.scroll)
                .min(VISIBLE_ROWS) as u16,
        );
        if y < inner.bottom().saturating_sub(2) {
            put_text(
                b,
                list.x.saturating_add(5),
                y,
                list.width.saturating_sub(6),
                "no images here",
                base.fg(p.overlay0),
            );
        }
    }
    if with_preview
        && picker
            .entries
            .get(picker.highlighted)
            .is_some_and(|entry| entry.kind == EntryKind::Image && entry.size_px.is_none())
    {
        put_text(
            b,
            image_preview.x,
            image_preview.y,
            image_preview.width,
            "no preview (PNG only)",
            base.fg(p.overlay0),
        );
    }
    let footer_y = inner.bottom().saturating_sub(1);
    let highlighted_image = picker
        .entries
        .get(picker.highlighted)
        .is_some_and(|entry| entry.kind == EntryKind::Image);
    let count = picker.marked.len().max(usize::from(highlighted_image));
    let label = format!(" attach {count} ");
    let label_width = display_width(&label).min(inner.width);
    let primary = Rect::new(
        inner.right().saturating_sub(label_width),
        footer_y,
        label_width,
        1,
    );
    let (hints, hints_style) = match picker.notice.as_deref() {
        Some(notice) => (notice, base.fg(p.red)),
        None => (
            "space preview · x mark · a new · ← up · enter · esc",
            base.fg(p.overlay0),
        ),
    };
    put_text(
        b,
        inner.x.saturating_add(1),
        footer_y,
        inner.width.saturating_sub(label_width + 2),
        hints,
        hints_style,
    );
    if count > 0 {
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
        image_preview,
        ..Default::default()
    })
}

/// Kitty image id of the image list's preview: outside the ranges pane
/// images use (10 000..910 000 and 0x4000_0000..0x6000_0000).
const PREVIEW_IMAGE_ID: u32 = 0x2000_0001;

/// The cells an image of `image` pixels covers, scaled to fit `area` with
/// its aspect kept, centred in it.
pub(super) fn fit_preview(
    area: Rect,
    image: (u32, u32),
    cell: crate::kitty_graphics::HostCellSize,
) -> Rect {
    let (cell_w, cell_h) = if cell.is_known() {
        (f64::from(cell.width_px), f64::from(cell.height_px))
    } else {
        (8.0, 16.0)
    };
    let (image_w, image_h) = (f64::from(image.0.max(1)), f64::from(image.1.max(1)));
    let scale =
        (f64::from(area.width) * cell_w / image_w).min(f64::from(area.height) * cell_h / image_h);
    let cols = ((image_w * scale / cell_w).round() as u16).clamp(1, area.width.max(1));
    let rows = ((image_h * scale / cell_h).round() as u16).clamp(1, area.height.max(1));
    Rect::new(
        area.x + (area.width.saturating_sub(cols)) / 2,
        area.y + (area.height.saturating_sub(rows)) / 2,
        cols,
        rows,
    )
}

impl ClientShellState {
    /// Lets the image list show the highlighted image: the host terminal
    /// takes kitty images and reads them from this machine's files.
    pub(crate) fn set_image_previews(&mut self, enabled: bool) {
        self.image_previews = enabled;
    }

    /// The kitty commands that show the highlighted image beside the list,
    /// or take the last one away. The file goes by path (`t=f`), so the
    /// terminal reads and scales it; it is sent once per file and placed
    /// again each frame, as pane images are.
    pub(super) fn compose_image_preview(&mut self) -> Vec<u8> {
        let wanted = match self.overlay.as_ref() {
            Some(ClientShellOverlay::ImagePicker(picker))
                if !self.hits.image_preview.is_empty() =>
            {
                picker
                    .entries
                    .get(picker.highlighted)
                    .filter(|entry| entry.kind == EntryKind::Image)
                    .and_then(|entry| Some((entry.path.clone(), entry.size_px?)))
            }
            _ => None,
        };
        let mut out = Vec::new();
        let path = wanted
            .as_ref()
            .and_then(|(path, _)| path.to_str().map(str::to_owned));
        if self.image_preview_sent.is_some()
            && self.image_preview_sent.as_ref() != wanted.as_ref().map(|(path, _)| path)
        {
            crate::kitty_graphics::encode_delete_image(&mut out, PREVIEW_IMAGE_ID);
            self.image_preview_sent = None;
        }
        let (Some((file, size)), Some(path)) = (wanted, path) else {
            return out;
        };
        let cells = fit_preview(self.hits.image_preview, size, self.graphics_cell_size);
        let place = format!(
            "i={PREVIEW_IMAGE_ID},p=1,c={},r={},z=1,C=1,q=2",
            cells.width, cells.height
        );
        let leading = format!("\x1b[{};{}H", cells.y + 1, cells.x + 1);
        if self.image_preview_sent.is_none() {
            encode_kitty_file(
                &mut out,
                leading.as_bytes(),
                &format!("a=T,f=100,{place}"),
                &path,
            );
            self.image_preview_sent = Some(file);
        } else {
            out.extend_from_slice(b"\x1b7");
            out.extend_from_slice(leading.as_bytes());
            out.extend_from_slice(format!("\x1b_Ga=p,{place};\x1b\\\x1b8").as_bytes());
        }
        out
    }
}

/// `encode_kitty_regular_file` with the file's path as the payload.
fn encode_kitty_file(out: &mut Vec<u8>, leading: &[u8], control: &str, path: &str) {
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
    out.extend_from_slice(b"\x1b7");
    out.extend_from_slice(leading);
    out.extend_from_slice(format!("\x1b_G{control},t=f;{payload}\x1b\\").as_bytes());
    out.extend_from_slice(b"\x1b8");
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
        std::fs::create_dir_all(dir.join("Zed")).unwrap();
        std::fs::create_dir_all(dir.join("archive")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        let shortcut = PathBuf::from("/shots");
        let (rows, error) = list_entries(&dir, &[dir.clone(), shortcut.clone()]);
        assert_eq!(error, None);
        let rows = rows
            .into_iter()
            .map(|entry| (entry.kind, entry.name))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                (EntryKind::Parent, "..".to_owned()),
                (EntryKind::Shortcut, tilde(&shortcut)),
                (EntryKind::Image, "new.PNG".to_owned()),
                (EntryKind::Image, "old.png".to_owned()),
                (EntryKind::Directory, "archive".to_owned()),
                (EntryKind::Directory, "Zed".to_owned()),
            ]
        );
        // An unreadable directory keeps the way out.
        let (rows, error) = list_entries(&dir.join("missing"), &[]);
        assert!(error.is_some());
        assert_eq!(rows.len(), 1);
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
        state.open_image_picker_in("pane_1".into(), dir.clone(), Vec::new());
        let first = attached(&mut state);
        assert_eq!(first.pane_id, "pane_1");
        assert_eq!(first.paths, [dir.join("a.png"), dir.join("b.png")]);
        assert!(state.overlay.is_none());
        assert_eq!(state.recent_image_dirs, std::slice::from_ref(&dir));

        // A new shot: only it is new since the last attach.
        write("c.png", 10);
        state.open_image_picker_in("pane_1".into(), dir.clone(), Vec::new());
        assert_eq!(attached(&mut state).paths, [dir.join("c.png")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn space_previews_the_highlighted_image_and_x_marks_it() {
        let dir = std::env::temp_dir().join(format!("herdr-image-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            std::fs::write(dir.join(name), b"\x89PNG\r\n\x1a\n").unwrap();
        }
        let mut state = ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        state.open_image_picker_in("pane_1".into(), dir.clone(), Vec::new());
        let highlighted = |state: &ClientShellState| match state.overlay.as_ref() {
            Some(ClientShellOverlay::ImagePicker(picker)) => {
                (picker.highlighted, picker.marked.clone())
            }
            _ => panic!("the image list is open"),
        };
        // Row 0 is `..`; the newest image is highlighted.
        let first = match state.overlay.as_ref() {
            Some(ClientShellOverlay::ImagePicker(picker)) => picker.entries[1].path.clone(),
            _ => panic!("the image list is open"),
        };

        let mut outcome = ClientShellInput::default();
        state.image_picker_key(crossterm::event::KeyCode::Char(' '), &mut outcome);
        assert!(matches!(
            outcome.actions.as_slice(),
            [ClientShellAction::PreviewImage(path)] if *path == first
        ));
        assert_eq!(highlighted(&state), (1, BTreeSet::new()));

        let mut outcome = ClientShellInput::default();
        state.image_picker_key(crossterm::event::KeyCode::Char('x'), &mut outcome);
        assert!(outcome.actions.is_empty());
        assert_eq!(highlighted(&state), (2, BTreeSet::from([1])));

        assert!(state.image_preview_failed("no preview".into()));
        assert!(state.image_picker_open());
        state.image_picker_key(crossterm::event::KeyCode::Esc, &mut outcome);
        assert!(!state.image_picker_open());
        assert!(!state.image_preview_failed("no preview".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fits_the_preview_keeping_the_aspect_and_reads_png_sizes() {
        let cell = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        // 1600x1000 px in 40x14 cells (400x280 px): width bound, 400x250 px.
        assert_eq!(
            fit_preview(Rect::new(10, 5, 40, 14), (1600, 1000), cell),
            Rect::new(10, 5, 40, 13)
        );
        // A tall image: width is not the limit, it stays centred.
        assert_eq!(
            fit_preview(Rect::new(0, 0, 40, 14), (500, 2000), cell),
            Rect::new(16, 0, 7, 14)
        );

        let dir = std::env::temp_dir().join(format!("herdr-image-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&3024u32.to_be_bytes());
        png.extend_from_slice(&1964u32.to_be_bytes());
        std::fs::write(dir.join("shot.png"), &png).unwrap();
        std::fs::write(dir.join("photo.jpg"), b"\xff\xd8\xff\xe0").unwrap();
        let mut sizes = list_entries(&dir, &[])
            .0
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::Image)
            .map(|entry| (entry.name, entry.size_px))
            .collect::<Vec<_>>();
        sizes.sort();
        assert_eq!(
            sizes,
            [
                ("photo.jpg".to_owned(), None),
                ("shot.png".to_owned(), Some((3024, 1964)))
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sends_the_preview_once_places_it_each_frame_and_deletes_it_on_close() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        state.set_image_previews(true);
        state.open_image_picker_in("pane_1".into(), std::env::temp_dir(), Vec::new());
        let Some(ClientShellOverlay::ImagePicker(picker)) = state.overlay.as_mut() else {
            panic!("the image list is open");
        };
        assert!(picker.preview);
        picker.entries = ["/shots/a.png", "/shots/b.png"]
            .into_iter()
            .map(|path| ImageEntry {
                kind: EntryKind::Image,
                path: PathBuf::from(path),
                name: path.into(),
                modified: SystemTime::UNIX_EPOCH,
                size_px: Some((800, 600)),
            })
            .collect();
        picker.highlighted = 0;
        state.hits.image_preview = Rect::new(70, 3, 40, 14);
        let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
        let path = |file: &str| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(file)
        };

        let sent = text(state.compose_image_preview());
        assert!(sent.contains("a=T,f=100,i=536870913,p=1,"), "{sent}");
        assert!(
            sent.contains(&format!("t=f;{}", path("/shots/a.png"))),
            "{sent}"
        );
        let placed = text(state.compose_image_preview());
        assert!(placed.contains("\x1b_Ga=p,i=536870913,p=1,"), "{placed}");
        assert!(!placed.contains("t=f"), "{placed}");

        let mut outcome = ClientShellInput::default();
        state.image_picker_key(crossterm::event::KeyCode::Down, &mut outcome);
        let moved = text(state.compose_image_preview());
        assert!(moved.starts_with("\x1b_Ga=d,d=I,i=536870913,"), "{moved}");
        assert!(moved.contains(&path("/shots/b.png")), "{moved}");

        state.image_picker_key(crossterm::event::KeyCode::Esc, &mut outcome);
        assert_eq!(
            text(state.compose_image_preview()),
            "\x1b_Ga=d,d=I,i=536870913,q=2;\x1b\\"
        );
        assert!(state.compose_image_preview().is_empty());
    }

    #[test]
    fn browses_into_directories_and_back_clearing_marks() {
        let dir = std::env::temp_dir().join(format!("herdr-image-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("top.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(dir.join("sub/inner.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let mut state = ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        state.open_image_picker_in("pane_1".into(), dir.clone(), Vec::new());
        let picker = |state: &ClientShellState| match state.overlay.as_ref() {
            Some(ClientShellOverlay::ImagePicker(picker)) => (
                picker.dir.clone(),
                picker.highlighted,
                picker.marked.len(),
                picker.entries[picker.highlighted].name.clone(),
            ),
            _ => panic!("the image list is open"),
        };
        let key = |state: &mut ClientShellState, code| {
            let mut outcome = ClientShellInput::default();
            state.image_picker_key(code, &mut outcome);
            outcome
        };
        use crossterm::event::KeyCode;
        // `..`, top.png (highlighted), sub/.
        key(&mut state, KeyCode::Char('x'));
        assert_eq!(picker(&state), (dir.clone(), 2, 1, "sub".to_owned()));
        // A click on a directory goes there; the marks are cleared.
        state.click_image_picker_row(2);
        assert_eq!(
            picker(&state),
            (dir.join("sub"), 1, 0, "inner.png".to_owned())
        );
        key(&mut state, KeyCode::Left);
        assert_eq!(picker(&state), (dir.clone(), 1, 0, "top.png".to_owned()));
        key(&mut state, KeyCode::Down);
        let outcome = key(&mut state, KeyCode::Enter);
        assert!(outcome.actions.is_empty());
        assert_eq!(picker(&state).0, dir.join("sub"));
        let outcome = key(&mut state, KeyCode::Enter);
        assert!(matches!(
            outcome.actions.as_slice(),
            [ClientShellAction::AttachImages(attach)] if attach.paths == [dir.join("sub/inner.png")]
        ));
        assert_eq!(state.recent_image_dirs, [dir.join("sub")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
