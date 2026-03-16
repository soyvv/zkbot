use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::nats_publisher::NatsPublisher;
use crate::proto::zk_gw_v1::gateway_simulator_admin_service_server::GatewaySimulatorAdminService;
use crate::proto::zk_gw_v1::*;
use crate::semantic_pipeline::SemanticPipeline;
use crate::venue::simulator::error_injection::{
    ErrorEffect, ErrorScope, InjectedErrorRule, MatchCriteria, TriggerPolicy,
};
use crate::venue::simulator::state::{BalanceEntry, PositionEntry, SimulatorState};
use crate::venue::simulator::adapter::{make_match_policy, SimulatorVenueAdapter};
use crate::venue_adapter::{VenueBalanceFact, VenueEvent, VenuePositionFact};

use zk_sim_core::match_policy::{FirstComeFirstServedMatchPolicy, ImmediateMatchPolicy};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Admin gRPC handler for simulator controls.
pub struct SimAdminHandler {
    pub sim_state: Arc<Mutex<SimulatorState>>,
    pub adapter: Arc<SimulatorVenueAdapter>,
    pub publisher: Arc<NatsPublisher>,
    pub pipeline: Arc<Mutex<SemanticPipeline>>,
}

#[tonic::async_trait]
impl GatewaySimulatorAdminService for SimAdminHandler {
    async fn force_match(
        &self,
        request: Request<ForceMatchRequest>,
    ) -> Result<Response<ForceMatchResponse>, Status> {
        let req = request.into_inner();
        let ts = now_ms();

        let fill_qty = if req.fill_mode == FillMode::Partial as i32 {
            Some(req.fill_qty)
        } else {
            None
        };
        let fill_price = if req.fill_price > 0.0 {
            Some(req.fill_price)
        } else {
            None
        };

        let results = {
            let mut state = self.sim_state.lock().await;
            match state
                .sim_core
                .force_match_order(req.order_id, fill_qty, fill_price, ts)
            {
                Ok(results) => {
                    // Update balances for fills.
                    for result in &results {
                        if let Some((instrument, side, qty, price)) =
                            SimulatorVenueAdapter::extract_fill_info(&result.order_report)
                        {
                            state
                                .account_state
                                .apply_fill_to_balances(&instrument, side, qty, price);
                        }
                    }
                    results
                }
                Err(msg) => {
                    return Ok(Response::new(ForceMatchResponse {
                        accepted: false,
                        message: msg,
                        ..Default::default()
                    }));
                }
            }
        };

        // Queue events through the adapter.
        for result in &results {
            self.adapter
                .queue_event(VenueEvent::OrderReport(result.order_report.clone()))
                .await;
        }

        // Optionally publish balance update.
        if req.publish_balance_update {
            let facts = {
                let state = self.sim_state.lock().await;
                state
                    .account_state
                    .balances
                    .iter()
                    .map(|(asset, entry)| VenueBalanceFact {
                        asset: asset.clone(),
                        total_qty: entry.total_qty,
                        avail_qty: entry.avail_qty,
                        frozen_qty: entry.frozen_qty,
                    })
                    .collect()
            };
            self.adapter
                .queue_event(VenueEvent::Balance(facts))
                .await;
        }

        let remaining = {
            let state = self.sim_state.lock().await;
            state
                .sim_core
                .order_cache()
                .get(&req.order_id)
                .map(|o| o.remaining_qty)
                .unwrap_or(0.0)
        };

        info!(order_id = req.order_id, "force match executed");

        Ok(Response::new(ForceMatchResponse {
            accepted: true,
            matched_order_id: req.order_id,
            generated_trade_count: results.len() as i32,
            remaining_open_qty: remaining,
            message: "force match applied".to_string(),
        }))
    }

    async fn inject_error(
        &self,
        request: Request<InjectErrorRequest>,
    ) -> Result<Response<InjectErrorResponse>, Status> {
        let req = request.into_inner();

        let error_id = if req.error_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            req.error_id
        };

        let scope = match req.scope {
            x if x == ErrorScope_::PlaceOrder as i32 => ErrorScope::PlaceOrder,
            x if x == ErrorScope_::CancelOrder as i32 => ErrorScope::CancelOrder,
            x if x == ErrorScope_::QueryAccountBalance as i32 => {
                ErrorScope::QueryAccountBalance
            }
            x if x == ErrorScope_::QueryOrderDetail as i32 => {
                ErrorScope::QueryOrderDetail
            }
            _ => return Err(Status::invalid_argument("invalid error scope")),
        };

        let criteria = req.match_criteria.map(|c| MatchCriteria {
            account_id: if c.account_id != 0 {
                Some(c.account_id)
            } else {
                None
            },
            side: if c.side != 0 { Some(c.side) } else { None },
            instrument: if c.instrument.is_empty() {
                None
            } else {
                Some(c.instrument)
            },
            order_id: if c.order_id != 0 {
                Some(c.order_id)
            } else {
                None
            },
            client_order_id: if c.client_order_id != 0 {
                Some(c.client_order_id)
            } else {
                None
            },
            exch_account_id: if c.exch_account_id.is_empty() {
                None
            } else {
                Some(c.exch_account_id)
            },
        }).unwrap_or_default();

        let effect = match req.effect {
            x if x == ErrorEffectType::ErrorEffectRpcError as i32 => ErrorEffect::RpcError {
                grpc_status_code: req.grpc_status_code,
                error_message: req.error_message.clone(),
            },
            x if x == ErrorEffectType::ErrorEffectRejectOrder as i32 => {
                ErrorEffect::RejectOrder {
                    error_message: req.error_message.clone(),
                }
            }
            x if x == ErrorEffectType::ErrorEffectDelayResponse as i32 => {
                ErrorEffect::DelayResponse {
                    delay_ms: req.delay_ms as u64,
                }
            }
            _ => return Err(Status::invalid_argument("invalid error effect")),
        };

        let trigger_policy = match req.trigger_policy {
            x if x == TriggerPolicy_::Once as i32 => TriggerPolicy::Once,
            x if x == TriggerPolicy_::Times as i32 => {
                TriggerPolicy::Times(req.trigger_times as u32)
            }
            x if x == TriggerPolicy_::UntilCleared as i32 => {
                TriggerPolicy::UntilCleared
            }
            _ => TriggerPolicy::Once,
        };

        let remaining_triggers = match &trigger_policy {
            TriggerPolicy::Once => Some(1),
            TriggerPolicy::Times(n) => Some(*n),
            TriggerPolicy::UntilCleared => None,
        };

        let rule = InjectedErrorRule {
            error_id: error_id.clone(),
            scope,
            criteria,
            effect,
            trigger_policy,
            remaining_triggers,
            priority: req.priority,
            enabled: req.enabled,
            created_at: now_ms(),
            last_fired_at: None,
        };

        {
            let mut state = self.sim_state.lock().await;
            state.control_state.error_rules.push(rule);
        }

        info!(error_id, "error rule injected");

        Ok(Response::new(InjectErrorResponse {
            error_id,
            accepted: true,
            message: "error rule injected".to_string(),
        }))
    }

    async fn list_injected_errors(
        &self,
        _request: Request<ListInjectedErrorsRequest>,
    ) -> Result<Response<ListInjectedErrorsResponse>, Status> {
        let state = self.sim_state.lock().await;
        let errors: Vec<InjectedErrorInfo> = state
            .control_state
            .error_rules
            .iter()
            .map(|r| InjectedErrorInfo {
                error_id: r.error_id.clone(),
                scope: match r.scope {
                    ErrorScope::PlaceOrder => ErrorScope_::PlaceOrder as i32,
                    ErrorScope::CancelOrder => ErrorScope_::CancelOrder as i32,
                    ErrorScope::QueryAccountBalance => {
                        ErrorScope_::QueryAccountBalance as i32
                    }
                    ErrorScope::QueryOrderDetail => {
                        ErrorScope_::QueryOrderDetail as i32
                    }
                },
                priority: r.priority,
                enabled: r.enabled,
                remaining_triggers: r.remaining_triggers.unwrap_or(0) as i32,
                created_at: r.created_at,
                last_fired_at: r.last_fired_at.unwrap_or(0),
                ..Default::default()
            })
            .collect();

        Ok(Response::new(ListInjectedErrorsResponse { errors }))
    }

    async fn clear_injected_error(
        &self,
        request: Request<ClearInjectedErrorRequest>,
    ) -> Result<Response<ClearInjectedErrorResponse>, Status> {
        let req = request.into_inner();
        let mut state = self.sim_state.lock().await;

        let before = state.control_state.error_rules.len();
        if req.error_id.is_empty() {
            state.control_state.error_rules.clear();
        } else {
            state
                .control_state
                .error_rules
                .retain(|r| r.error_id != req.error_id);
        }
        let cleared = before - state.control_state.error_rules.len();

        Ok(Response::new(ClearInjectedErrorResponse {
            accepted: true,
            cleared_count: cleared as i32,
            message: format!("cleared {cleared} error rule(s)"),
        }))
    }

    async fn reset_simulator(
        &self,
        _request: Request<ResetSimulatorRequest>,
    ) -> Result<Response<ResetSimulatorResponse>, Status> {
        let (cleared_orders, cleared_rules) = {
            let mut state = self.sim_state.lock().await;
            let orders = state.sim_core.order_cache().len() as i32;
            let rules = state.control_state.error_rules.len() as i32;
            state.sim_core.clear_orders();
            state.account_state.reset();
            state.control_state.reset();
            state.sim_core.match_policy =
                make_match_policy(&state.control_state.current_match_policy);
            (orders, rules)
        };

        // Also reset the semantic pipeline.
        {
            let mut pipeline = self.pipeline.lock().await;
            pipeline.reset();
        }

        info!("simulator reset");

        Ok(Response::new(ResetSimulatorResponse {
            accepted: true,
            cleared_orders,
            cleared_error_rules: cleared_rules,
            message: "simulator reset to initial state".to_string(),
        }))
    }

    async fn set_account_state(
        &self,
        request: Request<SetAccountStateRequest>,
    ) -> Result<Response<SetAccountStateResponse>, Status> {
        let req = request.into_inner();
        let mut state = self.sim_state.lock().await;

        if req.clear_existing_balances {
            state.account_state.balances.clear();
        }
        if req.clear_existing_positions {
            state.account_state.positions.clear();
        }

        let mut balance_count = 0i32;
        for m in &req.balance_mutations {
            if m.mode == MutationMode::Remove as i32 {
                state.account_state.balances.remove(&m.asset);
            } else {
                state.account_state.balances.insert(
                    m.asset.clone(),
                    BalanceEntry {
                        total_qty: m.total_qty,
                        avail_qty: m.avail_qty,
                        frozen_qty: m.frozen_qty,
                    },
                );
            }
            balance_count += 1;
        }

        let mut position_count = 0i32;
        for m in &req.position_mutations {
            if m.mode == MutationMode::Remove as i32 {
                state.account_state.positions.remove(&m.instrument_code);
            } else {
                state.account_state.positions.insert(
                    m.instrument_code.clone(),
                    PositionEntry {
                        instrument_code: m.instrument_code.clone(),
                        long_short_type: m.long_short_type,
                        total_qty: m.total_qty,
                        avail_qty: m.avail_qty,
                        frozen_qty: m.frozen_qty,
                    },
                );
            }
            position_count += 1;
        }

        // Optionally publish updates.
        if req.publish_updates {
            let balance_facts: Vec<VenueBalanceFact> = state
                .account_state
                .balances
                .iter()
                .map(|(asset, entry)| VenueBalanceFact {
                    asset: asset.clone(),
                    total_qty: entry.total_qty,
                    avail_qty: entry.avail_qty,
                    frozen_qty: entry.frozen_qty,
                })
                .collect();
            let account_id = state.account_state.account_id;
            let position_facts: Vec<VenuePositionFact> = state
                .account_state
                .positions
                .values()
                .map(|entry| VenuePositionFact {
                    instrument: entry.instrument_code.clone(),
                    long_short_type: entry.long_short_type,
                    qty: entry.total_qty,
                    avail_qty: entry.avail_qty,
                    frozen_qty: entry.frozen_qty,
                    account_id,
                })
                .collect();
            drop(state);
            self.adapter
                .queue_event(VenueEvent::Balance(balance_facts))
                .await;
            if !position_facts.is_empty() {
                self.adapter
                    .queue_event(VenueEvent::Position(position_facts))
                    .await;
            }
        }

        info!(balance_count, position_count, "account state updated");

        Ok(Response::new(SetAccountStateResponse {
            accepted: true,
            applied_balance_count: balance_count,
            applied_position_count: position_count,
            message: "account state updated".to_string(),
        }))
    }

    async fn submit_synthetic_tick(
        &self,
        request: Request<SubmitSyntheticTickRequest>,
    ) -> Result<Response<SubmitSyntheticTickResponse>, Status> {
        let req = request.into_inner();
        let tick = req
            .tick
            .ok_or_else(|| Status::invalid_argument("tick is required"))?;

        let results = {
            let mut state = self.sim_state.lock().await;
            if state.control_state.matching_paused {
                // Update orderbook cache but skip matching.
                state.sim_core.update_orderbook(&tick);
                vec![]
            } else {
                state.sim_core.on_tick(&tick)
            }
        };

        // Update balances under a single lock.
        let had_fills = !results.is_empty();
        {
            let mut state = self.sim_state.lock().await;
            for result in &results {
                if let Some((instrument, side, qty, price)) =
                    SimulatorVenueAdapter::extract_fill_info(&result.order_report)
                {
                    state
                        .account_state
                        .apply_fill_to_balances(&instrument, side, qty, price);
                }
            }
        }

        // Queue order report events.
        for result in &results {
            self.adapter
                .queue_event(VenueEvent::OrderReport(result.order_report.clone()))
                .await;
        }

        // Publish balance snapshot after fills.
        if had_fills {
            let facts = {
                let state = self.sim_state.lock().await;
                state
                    .account_state
                    .balances
                    .iter()
                    .map(|(asset, entry)| VenueBalanceFact {
                        asset: asset.clone(),
                        total_qty: entry.total_qty,
                        avail_qty: entry.avail_qty,
                        frozen_qty: entry.frozen_qty,
                    })
                    .collect()
            };
            self.adapter
                .queue_event(VenueEvent::Balance(facts))
                .await;
        }

        Ok(Response::new(SubmitSyntheticTickResponse {
            accepted: true,
            affected_order_count: 0, // TODO: count distinct orders affected
            generated_report_count: results.len() as i32,
            message: "synthetic tick processed".to_string(),
        }))
    }

    async fn list_open_orders(
        &self,
        _request: Request<ListOpenOrdersRequest>,
    ) -> Result<Response<ListOpenOrdersResponse>, Status> {
        let state = self.sim_state.lock().await;
        let orders: Vec<SimOpenOrder> = state
            .sim_core
            .order_cache()
            .values()
            .filter(|o| o.remaining_qty > 0.0)
            .map(|o| SimOpenOrder {
                order_id: o.req.correlation_id,
                exch_order_ref: o.exch_order_id.clone(),
                account_id: state.account_state.account_id,
                instrument: o.req.instrument.clone(),
                side: o.req.buysell_type,
                remaining_qty: o.remaining_qty,
                filled_qty: o.filled_qty,
                limit_price: o.req.scaled_price,
                created_ts: o.req.timestamp,
                updated_ts: o.update_ts,
            })
            .collect();

        Ok(Response::new(ListOpenOrdersResponse { orders }))
    }

    async fn pause_matching(
        &self,
        _request: Request<PauseMatchingRequest>,
    ) -> Result<Response<MatchingStateResponse>, Status> {
        let mut state = self.sim_state.lock().await;
        state.control_state.matching_paused = true;
        info!("matching paused");
        Ok(Response::new(MatchingStateResponse {
            matching_paused: true,
            message: "matching paused".to_string(),
        }))
    }

    async fn resume_matching(
        &self,
        _request: Request<ResumeMatchingRequest>,
    ) -> Result<Response<MatchingStateResponse>, Status> {
        let mut state = self.sim_state.lock().await;
        state.control_state.matching_paused = false;
        info!("matching resumed");
        Ok(Response::new(MatchingStateResponse {
            matching_paused: false,
            message: "matching resumed".to_string(),
        }))
    }

    async fn set_match_policy(
        &self,
        request: Request<SetMatchPolicyRequest>,
    ) -> Result<Response<SetMatchPolicyResponse>, Status> {
        let req = request.into_inner();
        let mut state = self.sim_state.lock().await;

        let previous = state.control_state.current_match_policy.clone();

        let new_policy: Box<dyn zk_sim_core::match_policy::MatchPolicy> = match req
            .policy_name
            .as_str()
        {
            "immediate" => Box::new(ImmediateMatchPolicy),
            "fcfs" => Box::new(FirstComeFirstServedMatchPolicy),
            _ => {
                return Ok(Response::new(SetMatchPolicyResponse {
                    previous_policy: previous,
                    current_policy: state.control_state.current_match_policy.clone(),
                    accepted: false,
                    message: format!("unknown policy: {}", req.policy_name),
                }));
            }
        };

        state.sim_core.match_policy = new_policy;
        state.control_state.current_match_policy = req.policy_name.clone();

        info!(
            previous_policy = previous,
            current_policy = req.policy_name,
            "match policy changed"
        );

        Ok(Response::new(SetMatchPolicyResponse {
            previous_policy: previous,
            current_policy: req.policy_name,
            accepted: true,
            message: "match policy updated".to_string(),
        }))
    }

    async fn advance_time(
        &self,
        request: Request<AdvanceTimeRequest>,
    ) -> Result<Response<AdvanceTimeResponse>, Status> {
        let req = request.into_inner();
        // Noop placeholder — returns success with acknowledged delta.
        Ok(Response::new(AdvanceTimeResponse {
            accepted: true,
            acknowledged_delta_ms: req.advance_ms,
            message: "advance_time is a noop placeholder".to_string(),
        }))
    }

    async fn get_simulator_state(
        &self,
        _request: Request<GetSimulatorStateRequest>,
    ) -> Result<Response<GetSimulatorStateResponse>, Status> {
        let state = self.sim_state.lock().await;

        let open_orders: Vec<SimOpenOrder> = state
            .sim_core
            .order_cache()
            .values()
            .filter(|o| o.remaining_qty > 0.0)
            .map(|o| SimOpenOrder {
                order_id: o.req.correlation_id,
                exch_order_ref: o.exch_order_id.clone(),
                account_id: state.account_state.account_id,
                instrument: o.req.instrument.clone(),
                side: o.req.buysell_type,
                remaining_qty: o.remaining_qty,
                filled_qty: o.filled_qty,
                limit_price: o.req.scaled_price,
                created_ts: o.req.timestamp,
                updated_ts: o.update_ts,
            })
            .collect();

        let balances: Vec<SimBalanceEntry> = state
            .account_state
            .balances
            .iter()
            .map(|(asset, entry)| SimBalanceEntry {
                asset: asset.clone(),
                total_qty: entry.total_qty,
                avail_qty: entry.avail_qty,
                frozen_qty: entry.frozen_qty,
            })
            .collect();

        let positions: Vec<SimPositionEntry> = state
            .account_state
            .positions
            .values()
            .map(|entry| SimPositionEntry {
                instrument_code: entry.instrument_code.clone(),
                long_short_type: entry.long_short_type,
                total_qty: entry.total_qty,
                avail_qty: entry.avail_qty,
                frozen_qty: entry.frozen_qty,
            })
            .collect();

        let active_error_rules: Vec<InjectedErrorInfo> = state
            .control_state
            .error_rules
            .iter()
            .map(|r| InjectedErrorInfo {
                error_id: r.error_id.clone(),
                priority: r.priority,
                enabled: r.enabled,
                remaining_triggers: r.remaining_triggers.unwrap_or(0) as i32,
                created_at: r.created_at,
                last_fired_at: r.last_fired_at.unwrap_or(0),
                ..Default::default()
            })
            .collect();

        Ok(Response::new(GetSimulatorStateResponse {
            open_orders,
            balances,
            positions,
            active_error_rules,
            current_match_policy: state.control_state.current_match_policy.clone(),
            matching_paused: state.control_state.matching_paused,
            last_tick_ts: 0,
            last_event_ts: 0,
        }))
    }
}

// Alias the proto enum types to avoid naming conflicts with our Rust enums.
use crate::proto::zk_gw_v1::ErrorScope as ErrorScope_;
use crate::proto::zk_gw_v1::TriggerPolicy as TriggerPolicy_;
