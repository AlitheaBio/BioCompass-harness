//! Browser run events. A terminal notice is emitted by the publisher after commit.
//! Logs, including module-authored lifecycle messages, cannot complete a subscription.
use crate::{OrgId, RunId, RunLogEvent, RunStatus};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalNotice {
    pub org_id: OrgId,
    pub run_id: RunId,
    pub status: RunStatus,
}

pub fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Canceled
    )
}

pub struct RunEvents {
    org_id: OrgId,
    run_id: RunId,
    logs: broadcast::Receiver<RunLogEvent>,
    terminal: broadcast::Receiver<TerminalNotice>,
    initial: Option<RunStatus>,
    done: bool,
}

impl RunEvents {
    /// Subscribe both receivers before reading initial status so completion cannot fall in between.
    pub fn new(
        org_id: OrgId,
        run_id: RunId,
        logs: broadcast::Receiver<RunLogEvent>,
        terminal: broadcast::Receiver<TerminalNotice>,
        status: RunStatus,
    ) -> Self {
        Self {
            org_id,
            run_id,
            logs,
            terminal,
            initial: Some(status).filter(|s| is_terminal(*s)),
            done: false,
        }
    }

    fn completion(&mut self, status: RunStatus) -> (&'static str, serde_json::Value) {
        self.done = true;
        (
            "terminal",
            serde_json::json!({"run_id": self.run_id, "status": status}),
        )
    }

    pub async fn next_message(&mut self) -> Option<(&'static str, serde_json::Value)> {
        if self.done {
            return None;
        }
        if let Some(status) = self.initial.take() {
            return Some(self.completion(status));
        }
        loop {
            tokio::select! {
                biased;
                event = self.terminal.recv() => match event {
                    Ok(ev) if ev.org_id == self.org_id && ev.run_id == self.run_id && is_terminal(ev.status) => {
                        return Some(self.completion(ev.status));
                    }
                    Ok(_) => continue,
                    // A missed completion is replayed from durable state on the next connection.
                    Err(_) => return None,
                },
                event = self.logs.recv() => match event {
                    Ok(ev) if ev.run_id == self.run_id => return Some(("log", serde_json::json!(ev))),
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replay_closes_stream_once_and_contains_only_public_fields() {
        let org = OrgId::from("one");
        let id = RunId::from("run");
        let (_logs, rx_logs) = broadcast::channel(4);
        let (_terminal, rx_terminal) = broadcast::channel(4);
        let mut events = RunEvents::new(org, id, rx_logs, rx_terminal, RunStatus::Succeeded);
        assert_eq!(
            events.next_message().await,
            Some((
                "terminal",
                serde_json::json!({"run_id":"run","status":"succeeded"})
            ))
        );
        assert_eq!(events.next_message().await, None);
    }

    #[tokio::test]
    async fn filters_foreign_and_unpublished_completions_and_keeps_logs_separate() {
        let org = OrgId::from("one");
        let id = RunId::from("run");
        let (logs, rx_logs) = broadcast::channel(4);
        let (terminal, rx_terminal) = broadcast::channel(8);
        let mut events = RunEvents::new(
            org.clone(),
            id.clone(),
            rx_logs,
            rx_terminal,
            RunStatus::Running,
        );
        for (org_id, run_id, status) in [
            (OrgId::from("two"), id.clone(), RunStatus::Succeeded),
            (org.clone(), RunId::from("other"), RunStatus::Failed),
            (org.clone(), id.clone(), RunStatus::Running),
        ] {
            terminal
                .send(TerminalNotice {
                    org_id,
                    run_id,
                    status,
                })
                .unwrap();
        }
        logs.send(RunLogEvent {
            run_id: id.clone(),
            timestamp: chrono::Utc::now(),
            stream: "stdout".into(),
            line: "succeeded".into(),
        })
        .unwrap();
        assert_eq!(events.next_message().await.unwrap().0, "log");
        for _ in 0..2 {
            terminal
                .send(TerminalNotice {
                    org_id: org.clone(),
                    run_id: id.clone(),
                    status: RunStatus::Failed,
                })
                .unwrap();
        }
        assert_eq!(
            events.next_message().await,
            Some((
                "terminal",
                serde_json::json!({"run_id":"run","status":"failed"})
            ))
        );
        assert_eq!(events.next_message().await, None);
    }
}
