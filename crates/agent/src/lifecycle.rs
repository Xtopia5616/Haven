//! R6 lifecycle window policy for branch rollback / in-loop truncate retry /
//! errored continue.
//!
//! Product defaults (P3 / 2026-08-24):
//! - Branch rollback: **Cancel-then-Allow** whenever a dispatcher run slot is
//!   held (`running_sessions`), including claim→spawn, stream, tool batch, and
//!   pause-write unwind. Idle ask/confirm waits: **Allow** after clearing gates.
//! - Empty / cut-off truncate retry: **Allow** only inside a live Running loop
//!   (already owned by `ResponsePolicy`); N/A outside the loop.
//! - Errored continue: **Allow** / **AwaitThenAllow** only from `Error |
//!   Paused*`; **Deny** for `Pending` / `Running` / `Completed` (enforced
//!   inside [`decide`], not a separate gate).
//!
//! Unavailable / honest UI guidance:
//! - Continue while `Running` or `Pending` — denied (use pause/cancel first).
//! - Branch from `Completed` with `pause=false` — denied by status machine
//!   (`Completed → Pending` illegal); reopen to Paused first.
//! - Steering never cancels in-flight tools (R1 default; no `CancelToolsOnSteer`).

use crate::session::SessionStatus;

/// External operation that must compose safely with session lifecycle windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleOp {
    /// `rollback_session` (branch restore).
    BranchRollback,
    /// In-loop empty / cut-off retry (`ResponsePolicy`). Not consulted for
    /// external API calls — documented here for the matrix.
    #[allow(dead_code)] // constructed only in unit tests (matrix documentation)
    TruncateRetry,
    /// `continue_session` (errored / paused retry).
    ErroredContinue,
}

/// Coarse lifecycle window used by the policy matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleWindow {
    /// No dispatcher run slot; status is idle pause / error / completed.
    Idle,
    /// `running_sessions` holds the id (claim→spawn, stream, tools, pause unwind).
    RunInFlight,
    /// `PausedAwaitingAnswer` and not in-flight.
    AskWait,
    /// `PausedAwaitingConfirm` and not in-flight.
    ConfirmWait,
}

/// Policy decision for one (window × op) cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleDecision {
    Allow,
    /// Cancel the run token (if any), await `unmark_running`, then proceed.
    CancelThenAllow,
    /// Await run unwind without cancelling (status already left Running).
    AwaitThenAllow,
    /// Product-denied window (e.g. continue while Pending / Running / Completed).
    Deny,
    /// Operation only exists inside the live loop.
    NotApplicable,
}

impl LifecycleWindow {
    /// Classify from session status + whether the dispatcher still holds the run slot.
    ///
    /// `SessionStatus::Running` counts as in-flight even without a dispatcher
    /// slot (direct `run_session_from_id` / tests) so rollback still cancels.
    pub fn classify(status: Option<&SessionStatus>, run_in_flight: bool) -> Self {
        if run_in_flight || matches!(status, Some(SessionStatus::Running)) {
            return Self::RunInFlight;
        }
        match status {
            Some(SessionStatus::PausedAwaitingAnswer) => Self::AskWait,
            Some(SessionStatus::PausedAwaitingConfirm) => Self::ConfirmWait,
            _ => Self::Idle,
        }
    }
}

/// Single source of allow/deny/queue semantics for R6.
///
/// For [`LifecycleOp::ErroredContinue`], pass the live status so Idle
/// non-retryable states (`Pending` / `Completed` / missing) return [`Deny`]
/// here instead of a separate gate.
pub fn decide(
    window: LifecycleWindow,
    op: LifecycleOp,
    status: Option<&SessionStatus>,
) -> LifecycleDecision {
    use LifecycleDecision::*;
    use LifecycleOp::*;
    use LifecycleWindow::*;

    match (window, op) {
        // --- Branch rollback ---
        (Idle, BranchRollback) => Allow,
        (AskWait, BranchRollback) => Allow,
        (ConfirmWait, BranchRollback) => Allow,
        (RunInFlight, BranchRollback) => CancelThenAllow,

        // --- Truncate retry (in-loop only) ---
        (RunInFlight, TruncateRetry) => Allow,
        (_, TruncateRetry) => NotApplicable,

        // --- Errored continue ---
        (AskWait, ErroredContinue) | (ConfirmWait, ErroredContinue) => Allow,
        (RunInFlight, ErroredContinue) => {
            if continue_status_allowed(status) {
                AwaitThenAllow
            } else {
                Deny
            }
        }
        (Idle, ErroredContinue) => {
            if continue_status_allowed(status) {
                Allow
            } else {
                Deny
            }
        }
    }
}

/// Whether `continue_session` accepts this status (independent of run slot).
pub fn continue_status_allowed(status: Option<&SessionStatus>) -> bool {
    matches!(
        status,
        Some(SessionStatus::Error)
            | Some(SessionStatus::Paused)
            | Some(SessionStatus::PausedAwaitingAnswer)
            | Some(SessionStatus::PausedAwaitingConfirm)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_branch_cancel_when_in_flight() {
        assert_eq!(
            decide(
                LifecycleWindow::RunInFlight,
                LifecycleOp::BranchRollback,
                Some(&SessionStatus::Running)
            ),
            LifecycleDecision::CancelThenAllow
        );
    }

    #[test]
    fn matrix_branch_allow_on_ask_confirm_idle() {
        assert_eq!(
            decide(
                LifecycleWindow::AskWait,
                LifecycleOp::BranchRollback,
                Some(&SessionStatus::PausedAwaitingAnswer)
            ),
            LifecycleDecision::Allow
        );
        assert_eq!(
            decide(
                LifecycleWindow::ConfirmWait,
                LifecycleOp::BranchRollback,
                Some(&SessionStatus::PausedAwaitingConfirm)
            ),
            LifecycleDecision::Allow
        );
        assert_eq!(
            decide(
                LifecycleWindow::Idle,
                LifecycleOp::BranchRollback,
                Some(&SessionStatus::Paused)
            ),
            LifecycleDecision::Allow
        );
    }

    #[test]
    fn matrix_continue_awaits_unwind() {
        assert_eq!(
            decide(
                LifecycleWindow::RunInFlight,
                LifecycleOp::ErroredContinue,
                Some(&SessionStatus::Paused)
            ),
            LifecycleDecision::AwaitThenAllow
        );
    }

    #[test]
    fn matrix_continue_denies_non_retryable_idle() {
        assert_eq!(
            decide(
                LifecycleWindow::Idle,
                LifecycleOp::ErroredContinue,
                Some(&SessionStatus::Pending)
            ),
            LifecycleDecision::Deny
        );
        assert_eq!(
            decide(
                LifecycleWindow::Idle,
                LifecycleOp::ErroredContinue,
                Some(&SessionStatus::Completed)
            ),
            LifecycleDecision::Deny
        );
        assert_eq!(
            decide(
                LifecycleWindow::RunInFlight,
                LifecycleOp::ErroredContinue,
                Some(&SessionStatus::Running)
            ),
            LifecycleDecision::Deny
        );
    }

    #[test]
    fn matrix_truncate_only_in_flight() {
        assert_eq!(
            decide(
                LifecycleWindow::RunInFlight,
                LifecycleOp::TruncateRetry,
                Some(&SessionStatus::Running)
            ),
            LifecycleDecision::Allow
        );
        assert_eq!(
            decide(
                LifecycleWindow::Idle,
                LifecycleOp::TruncateRetry,
                Some(&SessionStatus::Paused)
            ),
            LifecycleDecision::NotApplicable
        );
        assert_eq!(
            decide(
                LifecycleWindow::AskWait,
                LifecycleOp::TruncateRetry,
                Some(&SessionStatus::PausedAwaitingAnswer)
            ),
            LifecycleDecision::NotApplicable
        );
    }

    #[test]
    fn classify_prefers_run_in_flight_over_ask_status() {
        // Pause already flipped but handler still unwinding.
        assert_eq!(
            LifecycleWindow::classify(Some(&SessionStatus::PausedAwaitingAnswer), true),
            LifecycleWindow::RunInFlight
        );
        assert_eq!(
            LifecycleWindow::classify(Some(&SessionStatus::PausedAwaitingAnswer), false),
            LifecycleWindow::AskWait
        );
        // Direct run_session_from_id: Running without dispatcher slot.
        assert_eq!(
            LifecycleWindow::classify(Some(&SessionStatus::Running), false),
            LifecycleWindow::RunInFlight
        );
    }

    #[test]
    fn continue_status_gate() {
        assert!(continue_status_allowed(Some(&SessionStatus::Error)));
        assert!(continue_status_allowed(Some(
            &SessionStatus::PausedAwaitingConfirm
        )));
        assert!(!continue_status_allowed(Some(&SessionStatus::Pending)));
        assert!(!continue_status_allowed(Some(&SessionStatus::Running)));
        assert!(!continue_status_allowed(Some(&SessionStatus::Completed)));
    }
}
