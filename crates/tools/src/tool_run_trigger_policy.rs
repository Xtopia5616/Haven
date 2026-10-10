//! Pure scheduled-trigger admission policy.
//!
//! This module classifies the existing `due_at` / `delay_secs` /
//! `watch_tool_run_id` inputs and calculates their current due-time semantics.
//! The caller still owns configuration reads, durable admission, timer
//! creation, trigger delivery, and execution.

use chrono::{DateTime, Utc};
use haven_common::types::is_canonical_id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScheduledTrigger {
    At {
        due_at: DateTime<Utc>,
        remaining_secs: i64,
    },
    AfterToolRun {
        tool_run_id: String,
    },
}

#[derive(Debug)]
pub(crate) struct ScheduledTriggerRequest {
    due_at: Option<String>,
    delay_secs: Option<u64>,
    watch_tool_run_id: Option<String>,
}

impl ScheduledTriggerRequest {
    /// Normalize the dependency identity and reject a dependency combined
    /// with a timer. This runs before the caller samples its clock, matching
    /// the existing admission order.
    pub(crate) fn new(
        due_at: Option<String>,
        delay_secs: Option<u64>,
        watch_tool_run_id: Option<String>,
    ) -> anyhow::Result<Self> {
        if let Some(value) = watch_tool_run_id.as_deref()
            && !is_canonical_id(value, "toolrun")
        {
            anyhow::bail!("watch_tool_run_id must be a canonical toolrun- prefixed id");
        }
        if watch_tool_run_id.is_some() && (due_at.is_some() || delay_secs.is_some()) {
            anyhow::bail!("watch_tool_run_id cannot be combined with due_at or delay_secs");
        }

        Ok(Self {
            due_at,
            delay_secs,
            watch_tool_run_id,
        })
    }

    /// Apply time-dependent input rules using the caller's single UTC sample.
    /// Absolute due-time horizon is checked separately because its configured
    /// limit is read asynchronously by ToolRunService after parsing and future
    /// validation, as it was before this policy extraction.
    pub(crate) fn resolve(self, now: DateTime<Utc>) -> anyhow::Result<ScheduledTriggerCandidate> {
        if let Some(tool_run_id) = self.watch_tool_run_id {
            return Ok(ScheduledTriggerCandidate {
                trigger: ScheduledTrigger::AfterToolRun { tool_run_id },
                absolute_due_remaining_secs: None,
            });
        }

        match (self.due_at.as_deref(), self.delay_secs) {
            (Some(_), Some(_)) => {
                anyhow::bail!("use exactly one of due_at or delay_secs, not both")
            }
            (Some(value), None) => {
                let due_at = DateTime::parse_from_rfc3339(value.trim())
                    .map_err(|_| anyhow::anyhow!("due_at must be an ISO 8601 timestamp"))?
                    .with_timezone(&Utc);
                let remaining_secs = (due_at - now).num_seconds();
                if remaining_secs <= 0 {
                    anyhow::bail!("due_at must be in the future");
                }
                Ok(ScheduledTriggerCandidate {
                    trigger: ScheduledTrigger::At {
                        due_at,
                        remaining_secs,
                    },
                    absolute_due_remaining_secs: Some(remaining_secs),
                })
            }
            (None, Some(delay_secs)) if (1..=86_400).contains(&delay_secs) => {
                let remaining_secs = delay_secs as i64;
                Ok(ScheduledTriggerCandidate {
                    trigger: ScheduledTrigger::At {
                        due_at: now + chrono::Duration::seconds(remaining_secs),
                        remaining_secs,
                    },
                    absolute_due_remaining_secs: None,
                })
            }
            (None, Some(_)) => {
                anyhow::bail!("delay_secs must be between 1 and 86400")
            }
            (None, None) => {
                anyhow::bail!("either due_at, delay_secs or watch_tool_run_id is required")
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ScheduledTriggerCandidate {
    trigger: ScheduledTrigger,
    absolute_due_remaining_secs: Option<i64>,
}

impl ScheduledTriggerCandidate {
    pub(crate) fn needs_due_horizon_check(&self) -> bool {
        self.absolute_due_remaining_secs.is_some()
    }

    pub(crate) fn validate_due_horizon(&self, max_due_horizon_secs: i64) -> anyhow::Result<()> {
        if self
            .absolute_due_remaining_secs
            .is_some_and(|remaining| remaining > max_due_horizon_secs)
        {
            anyhow::bail!("due_at is more than 365 days in the future");
        }
        Ok(())
    }

    pub(crate) fn into_trigger(self) -> ScheduledTrigger {
        self.trigger
    }
}

#[cfg(test)]
mod tests {
    use super::{ScheduledTrigger, ScheduledTriggerRequest};
    use chrono::{Duration, TimeZone, Utc};

    fn fixed_now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2030, 1, 2, 3, 4, 5)
            .single()
            .expect("valid test timestamp")
    }

    #[test]
    fn resolves_relative_delay_from_the_supplied_clock_sample() {
        let now = fixed_now();
        let candidate = ScheduledTriggerRequest::new(None, Some(60), None)
            .unwrap()
            .resolve(now)
            .unwrap();

        assert!(!candidate.needs_due_horizon_check());
        assert_eq!(
            candidate.into_trigger(),
            ScheduledTrigger::At {
                due_at: now + Duration::seconds(60),
                remaining_secs: 60,
            }
        );

        let candidate = ScheduledTriggerRequest::new(None, Some(86_400), None)
            .unwrap()
            .resolve(now)
            .unwrap();
        assert_eq!(
            candidate.into_trigger(),
            ScheduledTrigger::At {
                due_at: now + Duration::days(1),
                remaining_secs: 86_400,
            }
        );
    }

    #[test]
    fn resolves_absolute_due_at_in_utc_and_enforces_the_configured_horizon() {
        let now = fixed_now();
        let due_at = now + Duration::days(365);
        let candidate = ScheduledTriggerRequest::new(Some(due_at.to_rfc3339()), None, None)
            .unwrap()
            .resolve(now)
            .unwrap();

        assert!(candidate.needs_due_horizon_check());
        candidate.validate_due_horizon(365 * 24 * 60 * 60).unwrap();
        assert_eq!(
            candidate.into_trigger(),
            ScheduledTrigger::At {
                due_at,
                remaining_secs: 365 * 24 * 60 * 60,
            }
        );

        let beyond_horizon = (now + Duration::days(365) + Duration::seconds(1)).to_rfc3339();
        let candidate = ScheduledTriggerRequest::new(Some(beyond_horizon), None, None)
            .unwrap()
            .resolve(now)
            .unwrap();
        let error = candidate
            .validate_due_horizon(365 * 24 * 60 * 60)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "due_at is more than 365 days in the future"
        );
    }

    #[test]
    fn resolves_canonical_tool_run_dependency_without_a_timer() {
        let candidate = ScheduledTriggerRequest::new(
            None,
            None,
            Some("toolrun-00000000000000000000000000000001".into()),
        )
        .unwrap()
        .resolve(fixed_now())
        .unwrap();

        assert!(!candidate.needs_due_horizon_check());
        assert_eq!(
            candidate.into_trigger(),
            ScheduledTrigger::AfterToolRun {
                tool_run_id: "toolrun-00000000000000000000000000000001".into(),
            }
        );
    }

    #[test]
    fn rejects_noncanonical_tool_run_dependency() {
        let error =
            ScheduledTriggerRequest::new(None, None, Some("toolrun-abc".into())).unwrap_err();
        assert_eq!(
            error.to_string(),
            "watch_tool_run_id must be a canonical toolrun- prefixed id"
        );
    }

    #[test]
    fn preserves_trigger_input_errors_and_subsecond_due_boundary() {
        let now = fixed_now();
        let cases = [
            (
                Some("2030-01-02T03:04:06Z".into()),
                Some(1),
                None,
                "use exactly one of due_at or delay_secs, not both",
            ),
            (
                None,
                Some(0),
                None,
                "delay_secs must be between 1 and 86400",
            ),
            (
                None,
                None,
                None,
                "either due_at, delay_secs or watch_tool_run_id is required",
            ),
            (
                Some("not-a-time".into()),
                None,
                None,
                "due_at must be an ISO 8601 timestamp",
            ),
            (
                Some((now + Duration::milliseconds(999)).to_rfc3339()),
                None,
                None,
                "due_at must be in the future",
            ),
        ];

        for (due_at, delay_secs, watch_tool_run_id, expected) in cases {
            let result = ScheduledTriggerRequest::new(due_at, delay_secs, watch_tool_run_id)
                .unwrap()
                .resolve(now);
            assert_eq!(result.unwrap_err().to_string(), expected);
        }

        let error = ScheduledTriggerRequest::new(None, Some(86_401), None)
            .unwrap()
            .resolve(now)
            .unwrap_err();
        assert_eq!(error.to_string(), "delay_secs must be between 1 and 86400");

        let error = ScheduledTriggerRequest::new(
            Some("2030-01-02T03:04:06Z".into()),
            None,
            Some("toolrun-00000000000000000000000000000001".into()),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "watch_tool_run_id cannot be combined with due_at or delay_secs"
        );
    }
}
