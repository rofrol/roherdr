//! The titles of TODO items by their stable id, read from a repository's
//! `TODO.md` when a client asks for the workers' runs (`worker.runs`). Only
//! the first line of each item counts: `- [ ] <title> [t-xxxxxxxx]`, or the
//! id right after the box (`- [ ] [t-xxxxxxxx] <title>`), as
//! `scripts/todo_edit.py` writes them.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// Characters a title keeps; a longer one is cut with `…`.
const TITLE_CHARS: usize = 80;

/// The titles of the items in `<repo>/TODO.md` by id; empty when there is
/// no such file.
pub(super) fn read_titles(repo: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(repo.join("TODO.md"))
        .map(|text| parse_titles(&text))
        .unwrap_or_default()
}

pub(super) fn parse_titles(text: &str) -> HashMap<String, String> {
    text.lines().filter_map(item_title).collect()
}

/// The ids of the items in `text`.
pub(super) fn item_ids(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(item_title)
        .map(|(id, _)| id)
        .collect()
}

/// The whole text of item `id` in `text`: its first line and every line up
/// to the next item or heading (outside a code fence), trailing blank lines
/// dropped, as `scripts/todo_edit.py` delimits an item.
pub(super) fn item_text(text: &str, id: &str) -> Option<String> {
    let mut in_fence = false;
    let mut lines: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
        }
        let starts_item = ["- [ ] ", "- [x] ", "- [X] "]
            .iter()
            .any(|box_| line.starts_with(box_));
        if !in_fence && (line.starts_with('#') || starts_item) {
            if lines.is_some() {
                break;
            }
            if item_title(line).is_some_and(|(found, _)| found == id) {
                lines = Some(vec![line]);
            }
            continue;
        }
        if let Some(lines) = lines.as_mut() {
            lines.push(line);
        }
    }
    let mut lines = lines?;
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    Some(lines.join("\n") + "\n")
}

/// An item's title from its text's first line.
pub(super) fn title_of_text(text: &str) -> Option<String> {
    item_title(text.lines().next()?).map(|(_, title)| title)
}

/// An item's first line as `(id, title)`; none for any other line or an
/// item without an id.
fn item_title(line: &str) -> Option<(String, String)> {
    let rest = ["- [ ] ", "- [x] ", "- [X] "]
        .iter()
        .find_map(|box_| line.strip_prefix(box_))?;
    let rest = rest.trim_end();
    let (id, title) = if let Some(lead) = rest.strip_prefix('[') {
        let (id, title) = lead.split_once(']')?;
        (id, title.trim())
    } else {
        let tail = rest.strip_suffix(']')?;
        let open = tail.rfind('[')?;
        (&tail[open + 1..], tail[..open].trim_end())
    };
    super::is_item_id(id).then(|| (id.to_owned(), shorten(title)))
}

fn shorten(title: &str) -> String {
    if title.chars().count() <= TITLE_CHARS {
        return title.to_owned();
    }
    let mut cut: String = title.chars().take(TITLE_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_each_items_first_line_with_the_id_at_either_end() {
        let titles = parse_titles(
            "# TODO\n\n## Next, in order\n\n\
             - [ ] Items popup on the coordinator's line [t-abcd2345]\n  \
             more text [t-zzzzzzzz] that is not a first line\n\
             - [ ] [t-qrst6723] Which one?\n\
             - [x] Done already [t-bbbbbbbb]\n\
             - [ ] No id here\n\
             - [ ] A bad id [t-ABCD2345]\n",
        );
        assert_eq!(titles.len(), 3);
        assert_eq!(
            titles["t-abcd2345"],
            "Items popup on the coordinator's line"
        );
        assert_eq!(titles["t-qrst6723"], "Which one?");
        assert_eq!(titles["t-bbbbbbbb"], "Done already");
    }

    #[test]
    fn an_items_text_runs_to_the_next_item_or_heading() {
        let text = "# TODO\n\n\
                    - [ ] First [t-abcd2345]\n  more\n\n  ```\n  # not a heading\n  ```\n\n\
                    - [ ] Second [t-qrst6723]\n  two\n\n## Done\n";
        assert_eq!(
            item_text(text, "t-abcd2345").as_deref(),
            Some("- [ ] First [t-abcd2345]\n  more\n\n  ```\n  # not a heading\n  ```\n")
        );
        assert_eq!(
            item_text(text, "t-qrst6723").as_deref(),
            Some("- [ ] Second [t-qrst6723]\n  two\n")
        );
        assert_eq!(item_text(text, "t-zzzzzzzz"), None);
        assert_eq!(
            item_ids(text).into_iter().collect::<Vec<_>>(),
            ["t-abcd2345", "t-qrst6723"]
        );
        assert_eq!(
            title_of_text("- [ ] Second [t-qrst6723]\n  two\n").as_deref(),
            Some("Second")
        );
    }

    #[test]
    fn a_long_title_is_cut() {
        let line = format!("- [ ] {} [t-abcd2345]", "x".repeat(200));
        let titles = parse_titles(&line);
        let title = &titles["t-abcd2345"];
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert!(title.ends_with('…'));
    }
}
