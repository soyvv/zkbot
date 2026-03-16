use std::time::{SystemTime, UNIX_EPOCH};

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorScope {
    PlaceOrder,
    CancelOrder,
    QueryAccountBalance,
    QueryOrderDetail,
}

#[derive(Debug, Clone)]
pub enum ErrorEffect {
    /// Return a gRPC error with the given status code and message.
    RpcError {
        grpc_status_code: i32,
        error_message: String,
    },
    /// Reject the order (return rejection in GatewayResponse).
    RejectOrder { error_message: String },
    /// Delay the response by the given number of milliseconds.
    DelayResponse { delay_ms: u64 },
}

#[derive(Debug, Clone)]
pub enum TriggerPolicy {
    Once,
    Times(u32),
    UntilCleared,
}

#[derive(Debug, Clone, Default)]
pub struct MatchCriteria {
    pub account_id: Option<i64>,
    pub side: Option<i32>,
    pub instrument: Option<String>,
    pub order_id: Option<i64>,
    pub client_order_id: Option<i64>,
    pub exch_account_id: Option<String>,
}

/// Context built from each incoming request for matching against rules.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub account_id: Option<i64>,
    pub side: Option<i32>,
    pub instrument: Option<String>,
    pub order_id: Option<i64>,
    pub client_order_id: Option<i64>,
    pub exch_account_id: Option<String>,
}

// ─── InjectedErrorRule ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct InjectedErrorRule {
    pub error_id: String,
    pub scope: ErrorScope,
    pub criteria: MatchCriteria,
    pub effect: ErrorEffect,
    pub trigger_policy: TriggerPolicy,
    pub remaining_triggers: Option<u32>,
    pub priority: i32,
    pub enabled: bool,
    pub created_at: i64,
    pub last_fired_at: Option<i64>,
}

impl InjectedErrorRule {
    /// Check if this rule matches the given request context.
    fn matches(&self, ctx: &RequestContext) -> bool {
        if !self.enabled {
            return false;
        }
        // Check remaining triggers.
        if let Some(remaining) = self.remaining_triggers {
            if remaining == 0 {
                return false;
            }
        }
        // All populated criteria fields must match; absent = wildcard.
        if let Some(aid) = self.criteria.account_id {
            if ctx.account_id != Some(aid) {
                return false;
            }
        }
        if let Some(side) = self.criteria.side {
            if ctx.side != Some(side) {
                return false;
            }
        }
        if let Some(ref inst) = self.criteria.instrument {
            if ctx.instrument.as_deref() != Some(inst) {
                return false;
            }
        }
        if let Some(oid) = self.criteria.order_id {
            if ctx.order_id != Some(oid) {
                return false;
            }
        }
        if let Some(coid) = self.criteria.client_order_id {
            if ctx.client_order_id != Some(coid) {
                return false;
            }
        }
        if let Some(ref eaid) = self.criteria.exch_account_id {
            if ctx.exch_account_id.as_deref() != Some(eaid) {
                return false;
            }
        }
        true
    }

    /// Fire the rule: decrement trigger count, update last_fired_at.
    fn fire(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.last_fired_at = Some(now);
        if let Some(ref mut remaining) = self.remaining_triggers {
            *remaining = remaining.saturating_sub(1);
        }
    }
}

// ─── Error injection engine ─────────────────────────────────────────────────

/// Evaluate all rules for a given scope and context.
/// Returns the effect of the first matching rule (by priority), or None.
pub fn evaluate_rules(
    rules: &mut [InjectedErrorRule],
    scope: &ErrorScope,
    ctx: &RequestContext,
) -> Option<ErrorEffect> {
    // Find the first matching rule by priority (lower number = higher priority).
    let mut best: Option<usize> = None;
    for (i, rule) in rules.iter().enumerate() {
        if rule.scope != *scope {
            continue;
        }
        if !rule.matches(ctx) {
            continue;
        }
        match best {
            None => best = Some(i),
            Some(bi) => {
                if rule.priority < rules[bi].priority {
                    best = Some(i);
                }
            }
        }
    }

    if let Some(idx) = best {
        let effect = rules[idx].effect.clone();
        rules[idx].fire();
        Some(effect)
    } else {
        None
    }
}
