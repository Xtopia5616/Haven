//! Pure claim/lease state shared by the Tools runtime and ToolRun persistence.
//!
//! The time point is generic because in-process claims use `Instant`, while
//! SQLite leases use its UTC datetime representation. A lease never mixes
//! clock domains after construction.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunLease<T> {
    // Identifies the leased ToolRun or completion result, not its consumer.
    claim_token: String,
    expires_at: T,
    valid: bool,
}

impl<T: PartialOrd> ToolRunLease<T> {
    pub fn new(claim_token: impl Into<String>, expires_at: T) -> Self {
        Self {
            claim_token: claim_token.into(),
            expires_at,
            valid: true,
        }
    }

    /// Return a new lease if the current lease is absent, invalid, or expired.
    /// Persistence adapters must still perform their own CAS when committing
    /// the returned claim.
    pub fn try_claim(
        current: Option<&Self>,
        claim_token: impl Into<String>,
        now: &T,
        expires_at: T,
    ) -> Option<Self> {
        if !Self::can_claim(current, now) {
            return None;
        }
        Some(Self::new(claim_token, expires_at))
    }

    pub fn can_claim(current: Option<&Self>, now: &T) -> bool {
        current.is_none_or(|lease| !lease.is_live_at(now))
    }

    pub fn is_live_at(&self, now: &T) -> bool {
        self.valid && now < &self.expires_at
    }

    pub fn matches_token(&self, claim_token: &str) -> bool {
        self.valid && self.claim_token == claim_token
    }

    /// Invalidate a lease only for its matching claim identity.
    pub fn invalidate_for(&mut self, claim_token: &str) -> bool {
        if !self.matches_token(claim_token) {
            return false;
        }
        self.valid = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::ToolRunLease;

    #[test]
    fn active_claim_conflicts_even_when_the_token_differs() {
        let current = ToolRunLease::new("result-a", 20);
        assert!(ToolRunLease::try_claim(Some(&current), "result-a", &10, 30).is_none());
        assert!(ToolRunLease::try_claim(Some(&current), "result-b", &10, 30).is_none());
    }

    #[test]
    fn expired_or_invalidated_claim_can_be_reclaimed() {
        let current = ToolRunLease::try_claim(None, "result-a", &0, 10)
            .expect("a missing lease should be claimable");
        let reclaimed = ToolRunLease::try_claim(Some(&current), "result-a", &10, 20)
            .expect("a lease is expired at its deadline");
        assert!(reclaimed.is_live_at(&10));

        let mut invalidated = ToolRunLease::new("result-a", 20);
        assert!(invalidated.invalidate_for("result-a"));
        assert!(ToolRunLease::try_claim(Some(&invalidated), "result-a", &10, 30).is_some());
    }

    #[test]
    fn mismatched_token_cannot_invalidate_a_claim() {
        let mut lease = ToolRunLease::new("result-a", 20);
        assert!(!lease.invalidate_for("result-b"));
        assert!(lease.is_live_at(&10));
        assert!(lease.matches_token("result-a"));
    }
}
