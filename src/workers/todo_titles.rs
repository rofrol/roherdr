//! The titles of TODO items by their stable id, read from a repository's
//! `TODO.md` when a client asks for the workers' runs (`worker.runs`). Only
//! the first line of each item counts: `- [ ] <title> [t-xxxxxxxx]`, or the
//! id right after the box (`- [ ] [t-xxxxxxxx] <title>`), as
//! `scripts/todo_edit.py` writes them.

use std::collections::HashMap;
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
    fn a_long_title_is_cut() {
        let line = format!("- [ ] {} [t-abcd2345]", "x".repeat(200));
        let titles = parse_titles(&line);
        let title = &titles["t-abcd2345"];
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert!(title.ends_with('…'));
    }
}
