use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A git repository with `files`, in its own directory.
fn repository(files: &[(&str, &str)]) -> std::path::PathBuf {
    let repo = std::env::temp_dir().join(format!(
        "herdr-pack-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env_remove("GIT_DIR")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
    };
    run(&["init", "-q", "-b", "master"]);
    for (key, value) in [
        ("user.name", "t"),
        ("user.email", "t@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        run(&["config", key, value]);
    }
    for (path, text) in files {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "feat: seed"]);
    repo
}

fn parts() -> Parts {
    Parts {
        item_text: "item".into(),
        decisions: vec![json!({"title": "d1", "text": "x".repeat(300)})],
        history: (0..4)
            .map(|i| json!({"kind": "verify_failed", "n": i, "text": "y".repeat(300)}))
            .collect(),
        log: (0..15)
            .map(|i| format!("abc{i} subject {}", "z".repeat(40)))
            .collect(),
        hits: (0..20)
            .map(|i| json!({"file": "f", "line": i, "text": "w".repeat(100)}))
            .collect(),
    }
}

#[test]
fn identifiers_are_the_backticked_names_of_code() {
    let text = "Change `call_model_for` and `Run::finish`, not `a b`, `x`, `--flag`, \
                `foo()`, `call_model_for` again; plain word_here is ignored.";
    assert_eq!(identifiers(text), ["call_model_for", "Run::finish", "foo"]);
}

#[test]
fn named_paths_are_the_existing_files_the_text_names() {
    let repo = repository(&[("src/a.rs", "fn a() {}\n"), ("TODO.md", "- item\n")]);
    let text = "Edit src/a.rs:12, (src/missing.rs), ../etc/passwd, /abs/x.rs and TODO.md; \
                see src/a.rs.";
    assert_eq!(named_paths(&repo, text), ["src/a.rs"]);
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn hits_name_file_line_and_text_for_symbols_that_exist() {
    let repo = repository(&[
        ("src/a.rs", "fn one() {}\nfn call_model_for() {}\n"),
        ("TODO.md", "- `call_model_for` is in the item\n"),
    ]);
    let hits = code_hits(&repo, "Use `call_model_for` and `not_there_anywhere`.");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["file"], "src/a.rs");
    assert_eq!(hits[0]["line"], 2);
    assert_eq!(hits[0]["symbol"], "call_model_for");
    assert_eq!(hits[0]["text"], "fn call_model_for() {}");
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn hits_stop_at_twenty() {
    let body = "alpha beta gamma delta epsilon\n".repeat(8);
    let repo = repository(&[("src/a.rs", &body)]);
    let hits = code_hits(&repo, "`alpha` `beta` `gamma` `delta` `epsilon`");
    assert_eq!(hits.len(), HITS_MAX, "{hits:?}");
    assert_eq!(hits[19]["symbol"], "delta");
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn the_log_is_the_commits_of_the_paths() {
    let repo = repository(&[("src/a.rs", "a\n"), ("src/b.rs", "b\n")]);
    let log = log_of(&repo, None, &["src/a.rs".to_owned()]);
    assert_eq!(log.len(), 1);
    assert!(log[0].ends_with("feat: seed"));
    assert!(log_of(&repo, None, &[]).is_empty());
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn an_oversize_pack_is_cut_in_order_and_says_what_was_cut() {
    let whole = bound(parts(), PACK_MAX);
    assert_eq!(whole["cut"], json!([]));
    assert_eq!(whole["code_hits"].as_array().unwrap().len(), 20);

    let size = render(&parts(), &[]).to_string().len();
    let tight = size - 1_500 + CUT_NOTES_RESERVE;
    let pack = bound(parts(), tight);
    assert!(pack.to_string().len() <= tight);
    let cut = pack["cut"].as_array().unwrap();
    assert!(
        cut[0].as_str().unwrap().starts_with("code_hits: kept"),
        "{cut:?}"
    );
    // Hits go first: the log, the history and the decisions are whole.
    assert_eq!(pack["git_log"].as_array().unwrap().len(), 15);
    assert_eq!(pack["run_history"].as_array().unwrap().len(), 4);
    assert_eq!(pack["decisions"].as_array().unwrap().len(), 1);
    // The same input is cut the same way.
    assert_eq!(pack, bound(parts(), tight));

    // Far too small: everything but the item goes, and each is named.
    let tiny = bound(parts(), CUT_NOTES_RESERVE + 200);
    assert_eq!(tiny["item_text"], "item");
    assert_eq!(tiny["decisions"], json!([]));
    assert_eq!(tiny["run_history"], json!([]));
    assert_eq!(tiny["cut"].as_array().unwrap().len(), 4);
}

#[test]
fn the_oldest_history_goes_first_and_a_long_item_is_cut() {
    let history: Vec<Value> = (0..6)
        .map(|i| json!({"n": i, "text": "y".repeat(1_000)}))
        .collect();
    let pack = bound(
        Parts {
            item_text: "é".repeat(ITEM_TEXT_MAX + 5),
            decisions: vec![],
            history,
            log: vec![],
            hits: vec![],
        },
        CUT_NOTES_RESERVE + ITEM_TEXT_MAX * 2 + 3_500,
    );
    let kept = pack["run_history"].as_array().unwrap();
    assert!(!kept.is_empty() && kept.len() < 6, "{}", kept.len());
    assert_eq!(kept.last().unwrap()["n"], 5);
    assert_eq!(kept[0]["n"], 6 - kept.len());
    let cut = pack["cut"].as_array().unwrap();
    assert!(cut[0].as_str().unwrap().starts_with("item_text: cut at"));
    assert!(pack["item_text"].as_str().unwrap().ends_with("..."));
}

#[test]
fn a_built_pack_holds_the_item_the_log_and_the_hits() {
    let repo = repository(&[("src/a.rs", "fn needle() {}\n")]);
    let pack = build(
        &repo,
        None,
        "Fix `needle` in src/a.rs",
        vec![json!({"title": "t", "text": "x"})],
        vec![json!({"kind": "verify_failed"})],
        LogPaths::Named,
    );
    assert_eq!(pack["item_text"], "Fix `needle` in src/a.rs");
    assert_eq!(pack["git_log"].as_array().unwrap().len(), 1);
    assert_eq!(pack["code_hits"][0]["file"], "src/a.rs");
    assert_eq!(pack["decisions"][0]["title"], "t");
    assert_eq!(pack["cut"], json!([]));
    let globs = ["src/*.rs".to_owned()];
    let pack = build(
        &repo,
        None,
        "no names",
        vec![],
        vec![],
        LogPaths::Globs(&globs),
    );
    assert_eq!(pack["git_log"].as_array().unwrap().len(), 1);
    assert_eq!(pack["code_hits"], json!([]));
    std::fs::remove_dir_all(repo).unwrap();
}
