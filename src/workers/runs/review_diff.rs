//! The diff an automatic review reads, cut to a budget by file: generated,
//! schema and contract files are never cut, so a change to a contract
//! cannot hide behind a large diff; the other files are cut first, whole,
//! smallest kept first, and the input names the files that were cut.

/// Paths whose diff is never cut (git pathspec style: `*` matches any
/// characters, `/` included; a pattern without `/` also matches a file's
/// name in any directory). A repository adds its own with
/// `[review] full_paths` in `.herdr/checks.toml`.
pub(super) const FULL_PATHS: [&str; 4] = [
    "docs/next/api/**",
    "tests/fixtures/**",
    "*.schema.json",
    "Cargo.lock",
];

/// The diff after the cut and the files the cut left out.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CutDiff {
    pub(super) text: String,
    pub(super) cut_files: Vec<String>,
}

/// Whether `text` matches `pattern`: `*` any run of characters (`/`
/// included), `?` one character, anything else itself.
fn wildcard(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|skip| wildcard(rest, &text[skip..])),
        Some((first, rest)) => match text.split_first() {
            Some((head, tail)) => (*first == '?' || first == head) && wildcard(rest, tail),
            None => false,
        },
    }
}

fn matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let whole: Vec<char> = path.chars().collect();
    if wildcard(&pattern, &whole) {
        return true;
    }
    if pattern.contains(&'/') {
        return false;
    }
    let name: Vec<char> = path.rsplit('/').next().unwrap_or(path).chars().collect();
    wildcard(&pattern, &name)
}

/// Whether the file's diff is never cut: one of [`FULL_PATHS`] or of the
/// repository's own `full_paths`.
pub(super) fn is_full_path(path: &str, extra: &[String]) -> bool {
    FULL_PATHS
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .any(|pattern| matches(pattern, path))
}

/// The file a `diff --git a/<old> b/<new>` header names (the new path).
fn file_of(chunk: &str) -> String {
    let header = chunk.lines().next().unwrap_or_default();
    let header = header.strip_prefix("diff --git ").unwrap_or(header);
    match header.rfind(" b/") {
        Some(at) => header[at + 3..].to_owned(),
        None => header.to_owned(),
    }
}

/// The diff cut to `max` bytes by files, protected files kept whole
/// whatever their size.
pub(super) fn cut(diff: String, max: usize, full_paths: &[String]) -> CutDiff {
    if diff.len() <= max {
        return CutDiff {
            text: diff,
            cut_files: Vec::new(),
        };
    }
    let mut chunks: Vec<&str> = Vec::new();
    let mut start = 0;
    for (at, _) in diff.match_indices("\ndiff --git ") {
        chunks.push(&diff[start..=at]);
        start = at + 1;
    }
    chunks.push(&diff[start..]);
    let files: Vec<String> = chunks.iter().map(|chunk| file_of(chunk)).collect();
    let mut kept: Vec<bool> = files
        .iter()
        .map(|file| is_full_path(file, full_paths))
        .collect();
    let protected: usize = chunks
        .iter()
        .zip(&kept)
        .filter(|(_, kept)| **kept)
        .map(|(chunk, _)| chunk.len())
        .sum();
    let mut budget = max.saturating_sub(protected);
    let mut others: Vec<usize> = (0..chunks.len()).filter(|index| !kept[*index]).collect();
    others.sort_by_key(|index| (chunks[*index].len(), *index));
    for index in others {
        if chunks[index].len() > budget {
            break;
        }
        budget -= chunks[index].len();
        kept[index] = true;
    }
    CutDiff {
        text: chunks
            .iter()
            .zip(&kept)
            .filter(|(_, kept)| **kept)
            .map(|(chunk, _)| *chunk)
            .collect(),
        cut_files: files
            .into_iter()
            .zip(&kept)
            .filter(|(_, kept)| !**kept)
            .map(|(file, _)| file)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(file: &str, size: usize) -> String {
        format!(
            "diff --git a/{file} b/{file}\n--- a/{file}\n+++ b/{file}\n+{}\n",
            "x".repeat(size)
        )
    }

    #[test]
    fn contract_paths_match_and_others_do_not() {
        for path in [
            "docs/next/api/herdr-api.schema.json",
            "tests/fixtures/endpoint-method-shapes-v1.json",
            "crates/x/foo.schema.json",
            "Cargo.lock",
            "crates/x/Cargo.lock",
        ] {
            assert!(is_full_path(path, &[]), "{path}");
        }
        for path in ["src/main.rs", "docs/next/CHANGELOG.md", "Cargo.toml"] {
            assert!(!is_full_path(path, &[]), "{path}");
        }
        assert!(is_full_path("proto/a.proto", &["proto/*.proto".to_owned()]));
        assert!(!is_full_path("src/a.proto", &["proto/*.proto".to_owned()]));
    }

    #[test]
    fn a_diff_within_the_budget_is_not_cut() {
        let diff = chunk("a.rs", 10);
        assert_eq!(
            cut(diff.clone(), 1000, &[]),
            CutDiff {
                text: diff,
                cut_files: vec![]
            }
        );
    }

    #[test]
    fn protected_files_stay_and_the_other_files_are_cut_whole_and_named() {
        let schema = chunk("docs/next/api/herdr-api.schema.json", 3_000);
        let small = chunk("src/small.rs", 100);
        let big = chunk("src/big.rs", 5_000);
        let diff = format!("{big}{schema}{small}");
        let cut = cut(diff, 4_000, &[]);
        assert!(cut.text.contains("herdr-api.schema.json"));
        assert!(cut.text.contains("src/small.rs"));
        assert!(!cut.text.contains("src/big.rs"));
        assert_eq!(cut.cut_files, ["src/big.rs"]);
        // Kept chunks stay in their original order and whole.
        assert!(cut.text.starts_with("diff --git a/docs/next/api"));
    }

    #[test]
    fn a_protected_file_over_the_budget_is_still_kept() {
        let lock = chunk("Cargo.lock", 10_000);
        let other = chunk("src/a.rs", 10);
        let cut = cut(format!("{other}{lock}"), 1_000, &[]);
        assert!(cut.text.contains("Cargo.lock"));
        assert_eq!(cut.cut_files, ["src/a.rs"]);
    }

    #[test]
    fn the_repositorys_full_paths_are_never_cut() {
        let diff = format!(
            "{}{}",
            chunk("proto/a.proto", 3_000),
            chunk("src/b.rs", 3_000)
        );
        let cut = cut(diff, 4_000, &["proto/*.proto".to_owned()]);
        assert!(cut.text.contains("proto/a.proto"));
        assert_eq!(cut.cut_files, ["src/b.rs"]);
    }
}
