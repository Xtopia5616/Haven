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
    /// Time-based triggers retain their full duration from this clock sample;
    /// the configured horizon is checked separately after ToolRunService reads
    /// it asynchronously. Dependency triggers have no due-time horizon.
    pub(crate) fn resolve(self, now: DateTime<Utc>) -> anyhow::Result<ScheduledTriggerCandidate> {
        if let Some(tool_run_id) = self.watch_tool_run_id {
            return Ok(ScheduledTriggerCandidate {
                trigger: ScheduledTrigger::AfterToolRun { tool_run_id },
                due_horizon: None,
            });
        }

        match (self.due_at.as_deref(), self.delay_secs) {
            (Some(_), Some(_)) => {
                anyhow::bail!("use exactly one of due_at or delay_secs, not both")
            }
            (Some(value), None) => {
                let due_at = DateTime::parse_from_rfc3339(value.trim())
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "due_at must be an ISO 8601 timestamp with a timezone offset"
                        )
                    })?
                    .with_timezone(&Utc);
                let due_duration = due_at - now;
                let remaining_secs = due_duration.num_seconds();
                if remaining_secs <= 0 {
                    anyhow::bail!("due_at must be in the future");
                }
                Ok(ScheduledTriggerCandidate {
                    trigger: ScheduledTrigger::At {
                        due_at,
                        remaining_secs,
                    },
                    due_horizon: Some(due_duration),
                })
            }
            (None, Some(delay_secs)) if (1..=86_400).contains(&delay_secs) => {
                let remaining_secs = delay_secs as i64;
                Ok(ScheduledTriggerCandidate {
                    trigger: ScheduledTrigger::At {
                        due_at: now + chrono::Duration::seconds(remaining_secs),
                        remaining_secs,
                    },
                    due_horizon: Some(chrono::Duration::seconds(remaining_secs)),
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
    due_horizon: Option<chrono::Duration>,
}

impl ScheduledTriggerCandidate {
    pub(crate) fn needs_due_horizon_check(&self) -> bool {
        self.due_horizon.is_some()
    }

    pub(crate) fn validate_due_horizon(&self, max_due_horizon_secs: i64) -> anyhow::Result<()> {
        if self
            .due_horizon
            .is_some_and(|duration| duration > chrono::Duration::seconds(max_due_horizon_secs))
        {
            anyhow::bail!(
                "scheduled due time exceeds the configured horizon of {max_due_horizon_secs} seconds"
            );
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
    use haven_common::config::MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS;

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

        assert!(candidate.needs_due_horizon_check());
        candidate.validate_due_horizon(60).unwrap();
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
            "scheduled due time exceeds the configured horizon of 31536000 seconds"
        );
    }

    #[test]
    fn enforces_zero_and_short_horizons_for_timer_triggers() {
        let now = fixed_now();

        let absolute_due_at = (now + Duration::seconds(5)).to_rfc3339();
        let disabled_absolute = ScheduledTriggerRequest::new(Some(absolute_due_at), None, None)
            .unwrap()
            .resolve(now)
            .unwrap();
        assert_eq!(
            disabled_absolute
                .validate_due_horizon(0)
                .unwrap_err()
                .to_string(),
            "scheduled due time exceeds the configured horizon of 0 seconds"
        );

        let disabled = ScheduledTriggerRequest::new(None, Some(1), None)
            .unwrap()
            .resolve(now)
            .unwrap();
        assert_eq!(
            disabled.validate_due_horizon(0).unwrap_err().to_string(),
            "scheduled due time exceeds the configured horizon of 0 seconds"
        );

        let delay_longer_than_horizon = ScheduledTriggerRequest::new(None, Some(86_400), None)
            .unwrap()
            .resolve(now)
            .unwrap();
        assert_eq!(
            delay_longer_than_horizon
                .validate_due_horizon(30)
                .unwrap_err()
                .to_string(),
            "scheduled due time exceeds the configured horizon of 30 seconds"
        );

        let delay_at_horizon = ScheduledTriggerRequest::new(None, Some(30), None)
            .unwrap()
            .resolve(now)
            .unwrap();
        delay_at_horizon.validate_due_horizon(30).unwrap();

        let fractional_overrun = ScheduledTriggerRequest::new(
            Some((now + Duration::milliseconds(1_500)).to_rfc3339()),
            None,
            None,
        )
        .unwrap()
        .resolve(now)
        .unwrap();
        assert!(fractional_overrun.validate_due_horizon(1).is_err());
    }

    #[test]
    fn enforces_the_configured_ten_year_horizon_and_reports_its_value() {
        let now = fixed_now();
        let at_limit = ScheduledTriggerRequest::new(
            Some((now + Duration::seconds(MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS)).to_rfc3339()),
            None,
            None,
        )
        .unwrap()
        .resolve(now)
        .unwrap();
        at_limit
            .validate_due_horizon(MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS)
            .unwrap();

        let beyond_limit = ScheduledTriggerRequest::new(
            Some(
                (now + Duration::seconds(MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS)
                    + Duration::seconds(1))
                .to_rfc3339(),
            ),
            None,
            None,
        )
        .unwrap()
        .resolve(now)
        .unwrap();
        assert_eq!(
            beyond_limit
                .validate_due_horizon(MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS)
                .unwrap_err()
                .to_string(),
            format!(
                "scheduled due time exceeds the configured horizon of {MAX_SCHEDULED_TOOL_RUNS_DUE_HORIZON_SECS} seconds"
            )
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
        candidate.validate_due_horizon(0).unwrap();
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
                "due_at must be an ISO 8601 timestamp with a timezone offset",
            ),
            (
                Some("2030-01-02T03:04:06".into()),
                None,
                None,
                "due_at must be an ISO 8601 timestamp with a timezone offset",
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
