//! `herdr history`: the life of each TODO item, read from the worker store's
//! `item_history` records ([`super::store`] writes them with the run events
//! that cause them) and joined with the runs, attempts and landings they
//! name. The reconcile compares the records with the repository's `TODO.md`
//! and reports what has no record; it never writes or drops one.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use super::runs::store_error;
use super::store::StoredHistory;
use super::{repository_of, todo_titles, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    HistoryAttempt, HistoryDeletedItem, HistoryEvent, HistoryEventKind, HistoryItem,
    HistoryItemSummary, HistoryOpenClaim, HistoryReconcile, HistoryRun, WorkerVerification,
};

/// The repository a directory is in, or the directory itself when it is
/// not in one (a repository that is gone keeps its records).
fn resolve_repo(dir: &str) -> String {
    repository_of(Path::new(dir)).unwrap_or_else(|| dir.to_owned())
}

/// The item's title from its latest record that holds its text.
fn title_of(records: &[&HistoryEvent]) -> Option<String> {
    records
        .iter()
        .rev()
        .filter_map(|event| event.item_text.as_deref())
        .find_map(todo_titles::title_of_text)
}

/// The titles of the items in each repository's `TODO.md`, each file read
/// at most once: the title of an item whose records hold none (one claimed
/// before the records kept its text) while it is still in the file.
#[derive(Default)]
struct OpenTitles(HashMap<String, HashMap<String, String>>);

impl OpenTitles {
    fn title(&mut self, repo: &str, item: &str) -> Option<String> {
        self.0
            .entry(repo.to_owned())
            .or_insert_with(|| todo_titles::read_titles(Path::new(repo)))
            .get(item)
            .cloned()
    }
}

/// Whether a record ends its run's claim.
fn ends_claim(kind: HistoryEventKind) -> bool {
    matches!(
        kind,
        HistoryEventKind::Noted
            | HistoryEventKind::Closed
            | HistoryEventKind::Aborted
            | HistoryEventKind::Blocked
    )
}

impl WorkerSupervisor {
    fn history_records(
        &self,
        repo: Option<&str>,
        item: Option<&str>,
    ) -> Result<Vec<StoredHistory>, WorkerError> {
        let repo = repo.map(resolve_repo);
        self.run_store()?
            .item_history(repo.as_deref(), item)
            .map_err(store_error)
    }

    /// Every recorded item, of `repo`'s repository when given, with its
    /// latest record, the most recent first.
    pub(crate) fn history_list(
        &self,
        repo: Option<&str>,
    ) -> Result<Vec<HistoryItemSummary>, WorkerError> {
        let records = self.history_records(repo, None)?;
        let mut items: BTreeMap<(String, String), Vec<&HistoryEvent>> = BTreeMap::new();
        for record in &records {
            items
                .entry((record.event.repo.clone(), record.event.item.clone()))
                .or_default()
                .push(&record.event);
        }
        let mut summaries: Vec<HistoryItemSummary> = items
            .into_iter()
            .filter_map(|((repo, item), events)| {
                let last = (*events.last()?).clone();
                Some(HistoryItemSummary {
                    repo,
                    item,
                    title: title_of(&events),
                    last,
                })
            })
            .collect();
        let mut open_titles = OpenTitles::default();
        for summary in summaries
            .iter_mut()
            .filter(|summary| summary.title.is_none())
        {
            summary.title = open_titles.title(&summary.repo, &summary.item);
        }
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last.id));
        Ok(summaries)
    }

    /// One item's timeline: its records and the runs they name, each with
    /// its attempts (their verdict, decision and landed commit).
    pub(crate) fn history_item(
        &self,
        item: &str,
        repo: Option<&str>,
    ) -> Result<HistoryItem, WorkerError> {
        super::check_item_id(item)?;
        let records = self.history_records(repo, Some(item))?;
        if records.is_empty() {
            return Err(WorkerError::HistoryNotFound(format!(
                "no records of item {item}{}",
                repo.map(|repo| format!(" in {}", resolve_repo(repo)))
                    .unwrap_or_default()
            )));
        }
        let events: Vec<HistoryEvent> = records.into_iter().map(|record| record.event).collect();
        let mut run_ids: Vec<&str> = Vec::new();
        for run_id in events.iter().filter_map(|event| event.run_id.as_deref()) {
            if !run_ids.contains(&run_id) {
                run_ids.push(run_id);
            }
        }
        let store = self.run_store()?;
        let mut runs = Vec::new();
        for run_id in run_ids {
            let Some(run) = store.run(run_id).map_err(store_error)? else {
                continue;
            };
            let landings = store.landings_of_run(run_id).map_err(store_error)?;
            let attempts = store
                .attempts(run_id)
                .map_err(store_error)?
                .into_iter()
                .map(|row| HistoryAttempt {
                    attempt: row.number,
                    worker_id: row.worker_id,
                    branch: row.branch,
                    commit: row.attempt.commit,
                    decision: row.attempt.review_decision,
                    verdict: row
                        .attempt
                        .verification
                        .as_deref()
                        .and_then(|text| serde_json::from_str::<WorkerVerification>(text).ok())
                        .map(|verification| verification.verdict),
                    landed_sha: landings
                        .iter()
                        .rev()
                        .find(|(attempt, _)| *attempt == row.number)
                        .map(|(_, sha)| sha.clone()),
                })
                .collect();
            runs.push(HistoryRun {
                run_id: run.info.run_id,
                status: run.info.status,
                created_ms: run.info.created_ms,
                todo_commit: run.info.todo_commit,
                attempts,
            });
        }
        let title = title_of(&events.iter().collect::<Vec<_>>()).or_else(|| {
            let mut open_titles = OpenTitles::default();
            events
                .iter()
                .rev()
                .find_map(|event| open_titles.title(&event.repo, item))
        });
        Ok(HistoryItem {
            item: item.to_owned(),
            title,
            events,
            runs,
        })
    }

    /// Compares `repo`'s `TODO.md` with its records: the claims no end
    /// record of their run followed (the run's status now beside each), and
    /// the items seen at a claim that are gone from `TODO.md` without a
    /// `closed` record. Reports only.
    pub(crate) fn history_reconcile(&self, repo: &str) -> Result<HistoryReconcile, WorkerError> {
        let repo = resolve_repo(repo);
        let todo_path = Path::new(&repo).join("TODO.md");
        let todo = std::fs::read_to_string(&todo_path).map_err(|error| {
            WorkerError::Invalid(format!("cannot read {}: {error}", todo_path.display()))
        })?;
        let records = self.history_records(Some(&repo), None)?;
        let store = self.run_store()?;
        let mut open_claims = Vec::new();
        for (at, record) in records.iter().enumerate() {
            let claim = &record.event;
            if claim.kind != HistoryEventKind::Claimed {
                continue;
            }
            let Some(run_id) = claim.run_id.as_deref() else {
                continue;
            };
            let ended = records[at + 1..].iter().any(|later| {
                later.event.run_id.as_deref() == Some(run_id) && ends_claim(later.event.kind)
            });
            if ended {
                continue;
            }
            open_claims.push(HistoryOpenClaim {
                item: claim.item.clone(),
                run_id: run_id.to_owned(),
                claimed_ms: claim.ts_ms,
                run_status: store
                    .run(run_id)
                    .map_err(store_error)?
                    .map(|run| run.info.status),
            });
        }
        let present = todo_titles::item_ids(&todo);
        let closed: BTreeSet<&str> = records
            .iter()
            .filter(|record| record.event.kind == HistoryEventKind::Closed)
            .map(|record| record.event.item.as_str())
            .collect();
        let mut known: BTreeSet<&str> = BTreeSet::new();
        for record in &records {
            known.insert(record.event.item.as_str());
            known.extend(record.ids_at_claim.iter().map(String::as_str));
        }
        let deleted_without_close = known
            .into_iter()
            .filter(|item| !present.contains(*item) && !closed.contains(item))
            .map(|item| {
                let events: Vec<&HistoryEvent> = records
                    .iter()
                    .map(|record| &record.event)
                    .filter(|event| event.item == item)
                    .collect();
                HistoryDeletedItem {
                    item: item.to_owned(),
                    title: title_of(&events),
                    last: events.last().map(|event| (*event).clone()),
                }
            })
            .collect();
        Ok(HistoryReconcile {
            repo,
            open_claims,
            deleted_without_close,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_without_a_recorded_title_takes_it_from_todo_while_it_is_there() {
        let repo = std::env::temp_dir().join(format!(
            "herdr-history-titles-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("TODO.md"),
            "# TODO\n\n- [ ] Still open [t-abcd2345]\n  Its text.\n",
        )
        .unwrap();
        let repo_text = repo.display().to_string();
        let mut titles = OpenTitles::default();
        assert_eq!(
            titles.title(&repo_text, "t-abcd2345").as_deref(),
            Some("Still open")
        );
        // A closed item is gone from the file: no title, the id shows.
        assert_eq!(titles.title(&repo_text, "t-bcde3456"), None);
        // A repository whose TODO.md is gone has no titles.
        assert_eq!(titles.title("/nonexistent/herdr-repo", "t-abcd2345"), None);
        std::fs::remove_dir_all(&repo).unwrap();
    }
}
