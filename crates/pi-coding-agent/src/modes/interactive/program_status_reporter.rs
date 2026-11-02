//! Port of `packages/coding-agent/src/modes/interactive/program-status-reporter.ts`.

use crate::config::APP_NAME;
use crate::core::agent_session::AgentSessionEvent;
use crate::core::extensions::types::CompactionReason;
use indexmap::IndexMap;
use parking_lot::Mutex;
use pi_agent::types::AgentMessage;
use pi_ai::types::StopReason;
use pi_tui::program_status::{BlockedKind, ProgramState, ProgramStatus};
use pi_tui::terminal::Terminal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlockedStatusKind {
    #[serde(rename = "permission")]
    Permission,
    #[serde(rename = "question")]
    Question,
    #[serde(rename = "auth")]
    Auth,
}

impl BlockedStatusKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }

    fn to_tui(self) -> BlockedKind {
        match self {
            Self::Permission => BlockedKind::Permission,
            Self::Question => BlockedKind::Question,
            Self::Auth => BlockedKind::Auth,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedStatus {
    pub kind: BlockedStatusKind,
    pub message: String,
}

fn first_line(text: Option<&str>) -> String {
    let line = text.and_then(|t| t.split('\n').next()).map(|l| l.strip_suffix('\r').unwrap_or(l).trim());
    match line {
        Some(l) if !l.is_empty() => l.to_string(),
        _ => "Error".to_string(),
    }
}

fn status(state: ProgramState, message: Option<String>) -> ProgramStatus {
    ProgramStatus { state, app: None, kind: None, message }
}

struct State {
    run_active: bool,
    compacting: bool,
    /// Outcome of the current run, reported once it settles.
    run_result: ProgramStatus,
    /// Status while no run is active.
    resting_status: ProgramStatus,
    /// Open dialogs by source, in the order they opened. The most recent one is reported.
    blocked: IndexMap<String, BlockedStatus>,
    last_report: Option<ProgramStatus>,
}

/// Reports interactive-mode state to the terminal (OSC 7501): `working` during agent runs and
/// compaction, `blocked` while a dialog waits for the user, then `done`, `error`, or `idle` once the run
/// settles. Messages are limited to the session name, dialog titles, and the first line of errors; prompts
/// and assistant output are never reported.
pub struct ProgramStatusReporter {
    get_terminal: Arc<dyn Fn() -> Arc<dyn Terminal> + Send + Sync>,
    get_session_name: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    state: Mutex<State>,
}

impl ProgramStatusReporter {
    pub fn new(
        get_terminal: Arc<dyn Fn() -> Arc<dyn Terminal> + Send + Sync>,
        get_session_name: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        Self {
            get_terminal,
            get_session_name,
            state: Mutex::new(State {
                run_active: false,
                compacting: false,
                run_result: status(ProgramState::Done, None),
                resting_status: status(ProgramState::Idle, None),
                blocked: IndexMap::new(),
                last_report: None,
            }),
        }
    }

    pub fn handle_event(&self, event: &AgentSessionEvent) {
        {
            let mut s = self.state.lock();
            match event {
                AgentSessionEvent::AgentStart => {
                    s.run_active = true;
                    s.run_result = status(ProgramState::Done, None);
                }
                AgentSessionEvent::MessageEnd { message } => {
                    // The latest response decides the outcome, so a retried error is replaced by its successful retry.
                    let AgentMessage::Assistant(message) = message else { return };
                    s.run_result = if message.stop_reason == StopReason::Error {
                        status(ProgramState::Error, Some(first_line(message.error_message.as_deref())))
                    } else {
                        status(ProgramState::Done, None)
                    };
                }
                AgentSessionEvent::CompactionStart { .. } => {
                    s.compacting = true;
                }
                AgentSessionEvent::CompactionEnd { reason, aborted, error_message, .. } => {
                    s.compacting = false;
                    if s.run_active {
                        // A failed recovery compaction ends the run unless a later response succeeds.
                        if *aborted {
                            s.run_result = status(ProgramState::Idle, None);
                        } else if let Some(error) = error_message {
                            s.run_result = status(ProgramState::Error, Some(first_line(Some(error))));
                        }
                    } else if *aborted {
                        s.resting_status = status(ProgramState::Idle, None);
                    } else if *reason == CompactionReason::Manual {
                        s.resting_status = match error_message {
                            Some(error) => status(ProgramState::Error, Some(first_line(Some(error)))),
                            None => status(ProgramState::Done, None),
                        };
                    }
                }
                AgentSessionEvent::AgentSettled { aborted } => {
                    s.run_active = false;
                    s.resting_status =
                        if *aborted { status(ProgramState::Idle, None) } else { s.run_result.clone() };
                }
                AgentSessionEvent::SessionInfoChanged { .. } => {
                    // The session name is part of working and done reports.
                }
                _ => return,
            }
        }
        self.report();
    }

    /// Report `blocked` for a dialog until it is cleared with `None`. Reopening a source replaces it.
    pub fn set_blocked(&self, source: &str, status: Option<&BlockedStatus>) {
        {
            let mut s = self.state.lock();
            s.blocked.shift_remove(source);
            if let Some(status) = status {
                s.blocked.insert(source.to_string(), status.clone());
            }
        }
        self.report();
    }

    /// Forget the previous session's run, for example after switching sessions.
    pub fn reset(&self) {
        {
            let mut s = self.state.lock();
            s.run_active = false;
            s.compacting = false;
            s.run_result = status(ProgramState::Done, None);
            s.resting_status = status(ProgramState::Idle, None);
        }
        self.report();
    }

    pub fn report(&self) {
        let mut status = self.current_status();
        status.app = Some(APP_NAME.to_string());
        {
            let mut s = self.state.lock();
            if s.last_report.as_ref() == Some(&status) {
                return;
            }
            s.last_report = Some(status.clone());
        }
        (self.get_terminal)().set_program_status(&status);
    }

    fn current_status(&self) -> ProgramStatus {
        let (blocked, compacting, run_active, resting) = {
            let s = self.state.lock();
            (s.blocked.values().last().cloned(), s.compacting, s.run_active, s.resting_status.clone())
        };
        if let Some(blocked) = blocked {
            return ProgramStatus {
                state: ProgramState::Blocked,
                app: None,
                kind: Some(blocked.kind.to_tui()),
                message: Some(blocked.message),
            };
        }
        if compacting {
            return status(ProgramState::Working, Some("Compacting context".to_string()));
        }
        let current = if run_active { status(ProgramState::Working, None) } else { resting };
        if matches!(current.state, ProgramState::Working | ProgramState::Done) {
            return ProgramStatus { message: (self.get_session_name)(), ..current };
        }
        current
    }
}

#[cfg(test)]
mod tests {
    //! Port of `test/program-status-reporter.test.ts` (#10607).
    use super::*;
    use pi_ai::types::{AssistantContent, AssistantMessage, TextContent};
    use pi_tui::terminal::{InputHandler, ResizeHandler};

    #[derive(Default)]
    struct RecordingTerminal {
        reports: Mutex<Vec<ProgramStatus>>,
    }

    impl Terminal for RecordingTerminal {
        fn start(&self, _on_input: InputHandler, _on_resize: ResizeHandler) {}
        fn stop(&self) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> usize {
            80
        }
        fn rows(&self) -> usize {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn set_program_status(&self, status: &ProgramStatus) {
            self.reports.lock().push(status.clone());
        }
    }

    struct Setup {
        reporter: ProgramStatusReporter,
        terminal: Arc<RecordingTerminal>,
        name: Arc<Mutex<Option<String>>>,
    }

    fn setup(session_name: Option<&str>) -> Setup {
        let terminal = Arc::new(RecordingTerminal::default());
        let name = Arc::new(Mutex::new(session_name.map(str::to_string)));
        let t = terminal.clone();
        let n = name.clone();
        let reporter = ProgramStatusReporter::new(
            Arc::new(move || t.clone() as Arc<dyn Terminal>),
            Arc::new(move || n.lock().clone()),
        );
        Setup { reporter, terminal, name }
    }

    impl Setup {
        fn send(&self, events: &[AgentSessionEvent]) {
            for event in events {
                self.reporter.handle_event(event);
            }
        }
        /// The last report without `app`.
        fn last(&self) -> ProgramStatus {
            let mut status = self.terminal.reports.lock().last().cloned().unwrap();
            status.app = None;
            status
        }
        fn count(&self) -> usize {
            self.terminal.reports.lock().len()
        }
    }

    fn st(state: ProgramState, message: Option<&str>) -> ProgramStatus {
        ProgramStatus { state, app: None, kind: None, message: message.map(str::to_string) }
    }

    fn assistant_end(stop_reason: StopReason, error_message: Option<&str>) -> AgentSessionEvent {
        AgentSessionEvent::MessageEnd {
            message: AgentMessage::Assistant(AssistantMessage {
                content: vec![AssistantContent::Text(TextContent::new("secret assistant output"))].into(),
                stop_reason,
                error_message: error_message.map(str::to_string),
                ..Default::default()
            }),
        }
    }

    fn settled(aborted: bool) -> AgentSessionEvent {
        AgentSessionEvent::AgentSettled { aborted }
    }

    fn compaction_start(reason: CompactionReason) -> AgentSessionEvent {
        AgentSessionEvent::CompactionStart { reason }
    }

    fn compaction_end(reason: CompactionReason, aborted: bool, error_message: Option<&str>) -> AgentSessionEvent {
        AgentSessionEvent::CompactionEnd {
            reason,
            result: None,
            aborted,
            will_retry: false,
            error_message: error_message.map(str::to_string),
        }
    }

    #[test]
    fn reports_idle_working_during_a_run_and_done_once_it_settles() {
        let s = setup(Some("Fix login"));
        s.reporter.report();
        assert_eq!(
            s.terminal.reports.lock().last().cloned().unwrap(),
            ProgramStatus { state: ProgramState::Idle, app: Some("pi".into()), kind: None, message: None }
        );

        s.send(&[AgentSessionEvent::AgentStart]);
        assert_eq!(s.last(), st(ProgramState::Working, Some("Fix login")));

        s.send(&[assistant_end(StopReason::ToolUse, None), assistant_end(StopReason::Stop, None)]);
        assert_eq!(s.last(), st(ProgramState::Working, Some("Fix login")));

        s.send(&[settled(false)]);
        assert_eq!(s.last(), st(ProgramState::Done, Some("Fix login")));
        let all = format!("{:?}", s.terminal.reports.lock());
        assert!(!all.contains("secret assistant output"));
    }

    #[test]
    fn reports_only_the_outcome_of_the_run() {
        let s = setup(None);
        s.send(&[
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Error, Some("overloaded")),
            assistant_end(StopReason::Stop, None),
            settled(false),
        ]);
        assert_eq!(s.last(), st(ProgramState::Done, None));

        s.send(&[
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Error, Some("Invalid API key\n{details}")),
            settled(false),
        ]);
        assert_eq!(s.last(), st(ProgramState::Error, Some("Invalid API key")));

        s.send(&[AgentSessionEvent::AgentStart, assistant_end(StopReason::Aborted, None), settled(true)]);
        assert_eq!(s.last(), st(ProgramState::Idle, None));

        // Aborted after a successful response, for example in an agent_before_settle hook or a retry delay.
        s.send(&[AgentSessionEvent::AgentStart, assistant_end(StopReason::Stop, None), settled(true)]);
        assert_eq!(s.last(), st(ProgramState::Idle, None));
    }

    #[test]
    fn reports_a_failed_recovery_compaction_as_the_runs_error_unless_a_later_response_succeeds() {
        let s = setup(None);
        s.send(&[
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Length, None),
            compaction_start(CompactionReason::Overflow),
            compaction_end(CompactionReason::Overflow, false, Some("Compaction failed\nstack")),
            settled(false),
        ]);
        assert_eq!(s.last(), st(ProgramState::Error, Some("Compaction failed")));

        s.send(&[
            AgentSessionEvent::AgentStart,
            compaction_start(CompactionReason::Threshold),
            compaction_end(CompactionReason::Threshold, false, Some("Compaction failed")),
            assistant_end(StopReason::Stop, None),
            settled(false),
        ]);
        assert_eq!(s.last(), st(ProgramState::Done, None));
    }

    #[test]
    fn reports_compaction_inside_a_run_and_the_result_of_a_manual_compaction() {
        let s = setup(Some("Session"));
        s.send(&[AgentSessionEvent::AgentStart, compaction_start(CompactionReason::Threshold)]);
        assert_eq!(s.last(), st(ProgramState::Working, Some("Compacting context")));
        s.send(&[compaction_end(CompactionReason::Threshold, false, None)]);
        assert_eq!(s.last(), st(ProgramState::Working, Some("Session")));
        s.send(&[assistant_end(StopReason::Stop, None), settled(false)]);

        s.send(&[compaction_start(CompactionReason::Manual), compaction_end(CompactionReason::Manual, false, None)]);
        assert_eq!(s.last(), st(ProgramState::Done, Some("Session")));
        s.send(&[
            compaction_start(CompactionReason::Manual),
            compaction_end(CompactionReason::Manual, false, Some("No model")),
        ]);
        assert_eq!(s.last(), st(ProgramState::Error, Some("No model")));
        s.send(&[compaction_start(CompactionReason::Manual), compaction_end(CompactionReason::Manual, true, None)]);
        assert_eq!(s.last(), st(ProgramState::Idle, None));
    }

    #[test]
    fn reports_the_most_recent_open_dialog_and_the_underlying_state_once_all_close() {
        let s = setup(None);
        let blocked = |kind, message: &str| BlockedStatus { kind, message: message.to_string() };
        let blocked_status = |kind, message: &str| ProgramStatus {
            state: ProgramState::Blocked,
            app: None,
            kind: Some(kind),
            message: Some(message.to_string()),
        };
        s.send(&[AgentSessionEvent::AgentStart]);
        s.reporter
            .set_blocked("extension-selector", Some(&blocked(BlockedStatusKind::Permission, "Allow bash?")));
        s.reporter.set_blocked("login", Some(&blocked(BlockedStatusKind::Auth, "Log in to Anthropic")));
        assert_eq!(s.last(), blocked_status(BlockedKind::Auth, "Log in to Anthropic"));

        // The run settles while the selector is still open.
        s.reporter.set_blocked("login", None);
        s.send(&[assistant_end(StopReason::Stop, None), settled(false)]);
        assert_eq!(s.last(), blocked_status(BlockedKind::Permission, "Allow bash?"));

        // Reopening a source replaces its dialog instead of stacking a second one.
        s.reporter.set_blocked("extension-selector", Some(&blocked(BlockedStatusKind::Question, "Pick one")));
        assert_eq!(s.last(), blocked_status(BlockedKind::Question, "Pick one"));
        s.reporter.set_blocked("extension-selector", None);
        assert_eq!(s.last(), st(ProgramState::Done, None));
    }

    #[test]
    fn sends_each_status_once_and_follows_session_name_changes() {
        let s = setup(Some("Old"));
        s.send(&[AgentSessionEvent::AgentStart, AgentSessionEvent::TurnStart, assistant_end(StopReason::ToolUse, None)]);
        s.reporter.report();
        assert_eq!(s.count(), 1);

        *s.name.lock() = Some("New".into());
        s.send(&[AgentSessionEvent::SessionInfoChanged { name: Some("New".into()) }]);
        assert_eq!(s.last(), st(ProgramState::Working, Some("New")));
    }

    #[test]
    fn returns_to_idle_when_the_session_is_replaced() {
        let s = setup(None);
        s.send(&[AgentSessionEvent::AgentStart, assistant_end(StopReason::Stop, None), settled(false)]);
        s.reporter.reset();
        assert_eq!(s.last(), st(ProgramState::Idle, None));
    }
}
