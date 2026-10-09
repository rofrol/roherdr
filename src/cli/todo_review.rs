//! `herdr todo review`: one attempt of a run (`todo.review`) printed as
//! the view the coordinator reviews it in or, with `--json`, as the
//! server's reply.

use crate::api::schema::{
    TodoReview, TodoRunStatus, TodoToolFailureKind, WorkerCheckOutcome, WorkerVerdict,
};

/// Prints the reply to `todo.review`: its text view, or the reply itself
/// with `json`, for an error, or for a reply this client cannot read.
pub(super) fn print_review(response: &serde_json::Value, json: bool) -> std::io::Result<i32> {
    if json || response.get("error").is_some() {
        return super::print_response(response);
    }
    let result = &response["result"];
    if result["type"] != "todo_review" {
        return super::print_response(response);
    }
    match serde_json::from_value::<TodoReview>(result["review"].clone()) {
        Ok(review) => {
            print!("{}", review_text(&review));
            Ok(0)
        }
        Err(_) => super::print_response(response),
    }
}

fn status(status: TodoRunStatus) -> &'static str {
    match status {
        TodoRunStatus::Running => "running",
        TodoRunStatus::Waiting => "waiting",
        TodoRunStatus::Done => "done",
        TodoRunStatus::Blocked => "blocked",
        TodoRunStatus::Aborted => "aborted",
        TodoRunStatus::Unknown => "unknown",
    }
}

fn verdict(verdict: WorkerVerdict) -> &'static str {
    match verdict {
        WorkerVerdict::Verified => "verified",
        WorkerVerdict::Failed => "failed",
        WorkerVerdict::Unavailable => "unavailable",
        WorkerVerdict::Unknown => "unknown",
    }
}

fn outcome(outcome: WorkerCheckOutcome) -> (&'static str, &'static str) {
    match outcome {
        WorkerCheckOutcome::Passed => ("✓", "passed"),
        WorkerCheckOutcome::Failed => ("✗", "failed"),
        WorkerCheckOutcome::Unavailable => ("!", "unavailable"),
        WorkerCheckOutcome::Skipped => ("-", "skipped"),
        WorkerCheckOutcome::Unknown => ("?", "unknown"),
    }
}

/// `text`'s lines, each indented by two spaces.
fn indented(text: &str) -> String {
    text.trim_end()
        .lines()
        .map(|line| {
            if line.is_empty() {
                "\n".to_owned()
            } else {
                format!("  {line}\n")
            }
        })
        .collect()
}

/// The attempt as text: a heading line, then one section per part, each
/// section's title on a line of its own.
pub(super) fn review_text(review: &TodoReview) -> String {
    let mut out = format!(
        "run {} · attempt {} of {} · {} · {} · {}\n",
        review.run_id,
        review.attempt,
        review.attempts,
        review.item,
        status(review.status),
        review.repo
    );
    let mut place = Vec::new();
    if let Some(worker) = &review.worker_id {
        place.push(format!("worker {worker}"));
    }
    if let Some(branch) = &review.branch {
        place.push(format!("branch {branch}"));
    }
    if let Some(base) = &review.base {
        place.push(format!("base {base}"));
    }
    if let Some(commit) = &review.commit {
        place.push(format!("commit {commit}"));
    }
    if !place.is_empty() {
        out.push_str(&format!("{}\n", place.join(" · ")));
    }
    if let Some(from) = review.from_attempt {
        out.push_str(&match &review.from_commit {
            Some(commit) => format!("starts from attempt {from}'s commit {commit}\n"),
            None => format!("follows attempt {from}, from the base\n"),
        });
    }

    out.push_str("\nTask\n");
    out.push_str(&indented(&review.task));

    out.push_str("\nFinal message\n");
    out.push_str(&match &review.final_message {
        Some(text) => indented(text),
        None => "  (none)\n".to_owned(),
    });

    out.push_str(&format!("\nQuestions ({})\n", review.questions.len()));
    for question in &review.questions {
        out.push_str(&format!(
            "  ? {}: {} → {}\n",
            question.tool_name,
            question.text,
            question.answer.as_deref().unwrap_or("not answered")
        ));
    }

    out.push_str(&format!(
        "\nFailed or denied tool calls ({})\n",
        review.tool_failures.len()
    ));
    for failure in &review.tool_failures {
        let how = match failure.kind {
            TodoToolFailureKind::Failed => "failed",
            TodoToolFailureKind::Denied => "denied",
            TodoToolFailureKind::Unknown => "unknown",
        };
        let input = failure
            .input
            .as_deref()
            .map(|input| format!(" `{input}`"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  ✗ {}{input} {how}: {}\n",
            failure.tool_name, failure.detail
        ));
    }

    out.push_str("\nDiff\n");
    match (&review.base, &review.commit, &review.diff_error) {
        (_, _, Some(error)) => out.push_str(&format!("  ({error})\n")),
        (Some(base), Some(commit), None) => {
            out.push_str(&format!("  git diff --stat {base} {commit}\n"));
            out.push_str(&match &review.diff_stat {
                Some(stat) => indented(stat),
                None => "  (no changes)\n".to_owned(),
            });
            if let Some(diff) = &review.diff {
                out.push('\n');
                out.push_str(diff.trim_end());
                out.push('\n');
            }
        }
        _ => out.push_str("  (none)\n"),
    }

    out.push_str("\nVerify\n");
    match &review.verification {
        None => out.push_str("  (not verified)\n"),
        Some(verification) => {
            out.push_str(&format!(
                "  {} · base {}{}\n",
                verdict(verification.verdict),
                verification.base,
                verification
                    .head
                    .as_deref()
                    .map(|head| format!(" · head {head}"))
                    .unwrap_or_default()
            ));
            for check in &verification.checks {
                let (mark, said) = outcome(check.outcome);
                let name = [check.name.as_deref(), check.path.as_deref()]
                    .into_iter()
                    .flatten()
                    .map(|name| format!(" {name}"))
                    .collect::<String>();
                out.push_str(&format!("  {mark} {}{name}: {said}\n", check.check));
                if !check.detail.is_empty() {
                    out.push_str(&indented(&indented(&check.detail)));
                }
            }
        }
    }

    out.push_str("\nDecision\n");
    match &review.decision {
        None => out.push_str("  (none yet)\n"),
        Some(decision) => out.push_str(&format!("  {decision}\n")),
    }
    if let Some(text) = &review.review_text {
        out.push_str(&indented(&indented(text)));
    }
    if let Some(approved) = &review.approved {
        out.push_str(&format!(
            "  approved commit {} on base {}\n",
            approved.commit, approved.base
        ));
    }
    if let Some(landed) = &review.landed_sha {
        out.push_str(&format!("  landed as {landed}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        TodoApproval, TodoReviewQuestion, TodoReviewToolFailure, WorkerVerification,
        WorkerVerifyCheck,
    };

    fn stub() -> TodoReview {
        TodoReview {
            run_id: "r-abcd2345".into(),
            item: "t-abcd2345".into(),
            repo: "/repo".into(),
            status: TodoRunStatus::Done,
            attempt: 2,
            attempts: 2,
            worker_id: Some("w7".into()),
            branch: Some("todo/t-abcd2345-r-abcd2345-2".into()),
            base: Some("b0".into()),
            commit: Some("c2".into()),
            from_attempt: Some(1),
            from_commit: Some("c1".into()),
            task: "Do it\n\n---\nCommit as exactly one commit".into(),
            final_message: Some("done\nWORKER-DONE c2 | it".into()),
            questions: vec![TodoReviewQuestion {
                request_id: Some("q1".into()),
                tool_name: "WebFetch".into(),
                text: "https://example.com".into(),
                answer: Some("allow".into()),
            }],
            tool_failures: vec![TodoReviewToolFailure {
                kind: TodoToolFailureKind::Denied,
                tool_name: "Bash".into(),
                input: Some("git push".into()),
                detail: "denied by the policy: no pushes".into(),
            }],
            diff_stat: Some(" a.txt | 1 +\n 1 file changed, 1 insertion(+)".into()),
            diff: Some("diff --git a/a.txt b/a.txt\n+change\n".into()),
            diff_error: None,
            verification: Some(WorkerVerification {
                verdict: WorkerVerdict::Verified,
                base: "b0".into(),
                head: Some("c2".into()),
                commits: vec!["c2".into()],
                checks: vec![
                    WorkerVerifyCheck {
                        check: "message".into(),
                        outcome: WorkerCheckOutcome::Passed,
                        path: None,
                        name: None,
                        detail: String::new(),
                    },
                    WorkerVerifyCheck {
                        check: "command".into(),
                        outcome: WorkerCheckOutcome::Passed,
                        path: None,
                        name: Some("ok".into()),
                        detail: "all good".into(),
                    },
                ],
                verified_ms: 1,
            }),
            decision: Some("approve".into()),
            review_text: None,
            approved: Some(TodoApproval {
                commit: "c2".into(),
                base: "b0".into(),
            }),
            landed_sha: Some("l9".into()),
        }
    }

    #[test]
    fn a_review_reads_as_one_view() {
        assert_eq!(
            review_text(&stub()),
            "run r-abcd2345 · attempt 2 of 2 · t-abcd2345 · done · /repo
worker w7 · branch todo/t-abcd2345-r-abcd2345-2 · base b0 · commit c2
starts from attempt 1's commit c1

Task
  Do it

  ---
  Commit as exactly one commit

Final message
  done
  WORKER-DONE c2 | it

Questions (1)
  ? WebFetch: https://example.com → allow

Failed or denied tool calls (1)
  ✗ Bash `git push` denied: denied by the policy: no pushes

Diff
  git diff --stat b0 c2
   a.txt | 1 +
   1 file changed, 1 insertion(+)

diff --git a/a.txt b/a.txt
+change

Verify
  verified · base b0 · head c2
  ✓ message: passed
  ✓ command ok: passed
    all good

Decision
  approve
  approved commit c2 on base b0
  landed as l9
"
        );
    }

    #[test]
    fn an_attempt_without_a_commit_or_verify_says_so() {
        let review = TodoReview {
            commit: None,
            from_attempt: None,
            final_message: None,
            questions: Vec::new(),
            tool_failures: Vec::new(),
            diff_stat: None,
            diff: None,
            diff_error: Some("the attempt has no commit".into()),
            verification: None,
            decision: None,
            approved: None,
            landed_sha: None,
            ..stub()
        };
        let text = review_text(&review);
        for part in [
            "Final message\n  (none)\n",
            "Questions (0)\n",
            "Diff\n  (the attempt has no commit)\n",
            "Verify\n  (not verified)\n",
            "Decision\n  (none yet)\n",
        ] {
            assert!(text.contains(part), "{part:?} in {text}");
        }
    }

    #[test]
    fn json_keeps_the_replys_shape() {
        let value = serde_json::to_value(stub()).unwrap();
        for key in [
            "run_id",
            "item",
            "repo",
            "status",
            "attempt",
            "attempts",
            "worker_id",
            "branch",
            "base",
            "commit",
            "from_attempt",
            "from_commit",
            "task",
            "final_message",
            "questions",
            "tool_failures",
            "diff_stat",
            "diff",
            "verification",
            "decision",
            "approved",
            "landed_sha",
        ] {
            assert!(value.get(key).is_some(), "{key} in {value}");
        }
        assert_eq!(value["status"], "done");
        assert_eq!(value["tool_failures"][0]["kind"], "denied");
        assert_eq!(
            value["approved"],
            serde_json::json!({"commit": "c2", "base": "b0"})
        );
        // Absent parts are left out, not null.
        let bare = serde_json::to_value(TodoReview {
            diff: None,
            approved: None,
            questions: Vec::new(),
            ..stub()
        })
        .unwrap();
        for key in ["diff", "approved", "questions"] {
            assert!(bare.get(key).is_none(), "{key} in {bare}");
        }
        assert_eq!(serde_json::from_value::<TodoReview>(value).unwrap(), stub());
    }
}
