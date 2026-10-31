use crate::model::{TaskEvent, TaskStatus};
use crate::store::config::ScopeSelection;
use crate::store::TaskStore;
use anyhow::Result;
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

impl TaskStore {
    pub fn log_event(
        &self,
        id: &str,
        action: &str,
        actor: &str,
        session_id: Option<&str>,
        from_status: Option<&str>,
        to_status: &str,
        eval_passed: Option<bool>,
        title: &str,
    ) -> Result<()> {
        let event = TaskEvent {
            timestamp: Local::now().to_rfc3339(),
            id: id.to_string(),
            action: action.to_string(),
            actor: actor.to_string(),
            session_id: session_id.map(|s| s.to_string()),
            from_status: from_status.map(|s| s.to_string()),
            to_status: to_status.to_string(),
            eval_passed,
            title: title.to_string(),
        };
        let events_file = self.tasks_path.join(".events.jsonl");
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(events_file)?;
        writeln!(f, "{}", serde_json::to_string(&event)?)?;
        Ok(())
    }

    pub(crate) fn read_events_from_path(tasks_path: &Path) -> Result<Vec<TaskEvent>> {
        let events_file = tasks_path.join(".events.jsonl");
        if !events_file.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(&events_file)?;
        let mut events = Vec::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<TaskEvent>(trimmed) {
                events.push(ev);
            }
        }
        Ok(events)
    }

    /// Read all telemetry events from `tasks/.events.jsonl`.
    /// Cards whose state the event log does not explain: `done` with no completion event, or an
    /// auto-accept approval with no `eval-approved` event. tkmd cannot stop an agent that writes
    /// the store directly; this is how such edits get noticed. Empty means consistent.
    pub fn integrity_report(&self) -> Result<Vec<String>> {
        let events = Self::read_events_from_path(&self.tasks_path)?;
        let mut issues = Vec::new();
        for card in self.list_store_cards()? {
            let id = card.meta.id.as_str();
            let mine: Vec<&TaskEvent> = events
                .iter()
                .filter(|e| e.id.eq_ignore_ascii_case(id))
                .collect();
            if card.meta.status == TaskStatus::Done && !mine.iter().any(|e| e.to_status == "done") {
                issues.push(format!(
                    "{}: status is `done` but no completion was recorded (card edited by hand?)",
                    id
                ));
            }
            if card.meta.approved_eval.is_some() {
                let last = mine
                    .iter()
                    .rev()
                    .find(|e| e.action == "eval-approved" || e.action == "eval-approval-revoked");
                if !last.is_some_and(|e| e.action == "eval-approved") {
                    issues.push(format!(
                        "{}: auto-accept approval is set but was never granted with `tk auto` (card edited by hand?)",
                        id
                    ));
                }
            }
            if card.meta.approval.as_deref() == Some("auto:eval")
                && !mine.iter().any(|e| e.action == "eval-approved")
            {
                issues.push(format!(
                    "{}: auto-accepted without a recorded `tk auto` approval",
                    id
                ));
            }
            if card.meta.status == TaskStatus::Blocked
                && !card.meta.blocked_by.is_empty()
                && self.open_blockers(&card)?.is_empty()
            {
                issues.push(format!(
                    "{}: still `blocked` but every blocker is closed; it can be unblocked (`tk c {}`)",
                    id, id
                ));
            }
        }
        // Archived cards were done when archived: that completion must be on record too.
        let archive = self.tasks_path.join("archive");
        for entry in fs::read_dir(&archive).into_iter().flatten().flatten() {
            if entry.path().extension().is_some_and(|x| x == "md")
                && let Ok(card) = Self::read_card(&entry.path())
                && !events
                    .iter()
                    .any(|e| e.id.eq_ignore_ascii_case(&card.meta.id) && e.to_status == "done")
            {
                issues.push(format!(
                    "{}: archived, but no completion was ever recorded (card edited or copied by hand?)",
                    card.meta.id
                ));
            }
        }
        Ok(issues)
    }

    pub fn read_events(&self) -> Result<Vec<TaskEvent>> {
        let mut events = Self::read_events_from_path(&self.tasks_path)?;
        if self.active_scope == ScopeSelection::All
            && let Some(ref peer) = self.peer
        {
            let mut peer_events = Self::read_events_from_path(&peer.tasks_path)?;
            events.append(&mut peer_events);
            events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        }
        Ok(events)
    }
}
