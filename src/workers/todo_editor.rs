//! Herdr's own TODO editor: `scripts/todo_edit.py`, embedded in the binary
//! at build time, so the driver's TODO step and `herdr todo edit` edit
//! `TODO.md` and `DECISIONS.md` the same way in every repository, whether it
//! has a copy of the script or not.

use std::path::{Path, PathBuf};

/// The text of `scripts/todo_edit.py` this binary was built with.
pub(crate) const SCRIPT: &str = include_str!("../../scripts/todo_edit.py");

/// The file name the editor is written under.
pub(crate) const SCRIPT_NAME: &str = "todo_edit.py";

/// Writes the editor into `dir` (which must exist and hold nothing a
/// Python import could pick up, since Python puts a script's directory first
/// on its path) and returns its path, for `python3 <path> ...`.
pub(crate) fn write_script(dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(SCRIPT_NAME);
    std::fs::write(&path, SCRIPT).map_err(|error| {
        format!(
            "cannot write herdr's {SCRIPT_NAME} to {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded copy is the repository's script, byte for byte, so the
    /// two can never drift apart.
    #[test]
    fn embedded_editor_is_the_repository_script() {
        let script = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/todo_edit.py"),
        )
        .expect("read scripts/todo_edit.py");
        assert_eq!(SCRIPT, script);
    }
}
