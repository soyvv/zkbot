//! tonic implementation of `OmsService`.
//!
//! The handler struct holds:
//! - `cmd_tx`: sends mutations to the OMS writer task
//! - `replica`: atomic read path for queries (never blocks the writer)
//!
//! # Mutation flow (PlaceOrder, CancelOrder, Panic, etc.)
//! 1. Validate request fields.
//! 2. Send `OmsCommand` + `oneshot::Sender<OmsResponse>` via `cmd_tx`.
//! 3. Await reply from writer task.
//! 4. Map to gRPC response.
//!
//! # Query flow (QueryOpenOrders, QueryBalances, QueryPositions)
//! 1. `snap = replica.load()` — atomic pointer load.
//! 2. Filter snapshot maps; no blocking, no contention with writer.
//! 3. Return response.
//!
//! # Idempotency
//! PlaceOrder checks `snap.orders.contains_key(&order_id)` before sending the
//! command.  A duplicate `order_id` returns `DUPLICATE` without touching the
//! writer task.  CancelOrder is idempotent by design in OmsCore.
//!
//! # Config readability (TODO Phase 3.5 — Pilot integration)
//! A future `GetConfig` RPC will return `OmsSvcConfig` fields (risk limits,
//! enabled checks, etc.) so Pilot can display current per-OMS configuration
//! without requiring SSH access.

use std::sync::Arc;

use tokio::sync::mpsc;
use tonic::{Request, Response, Status};

use zk_proto_rs::zk::{
    common::v1::{DummyRequest, ServiceHealthResponse},
    oms::v1::{
        AlgoOrderRequest, Balance, BatchCancelOrdersRequest,
        BatchPlaceOrdersRequest, CancelAlgoOrderRequest, DontPanicRequest,
        OmsResponse, OmsErrorType, OrderDetailResponse, PanicRequest, PlaceOrderRequest,
        CancelOrderRequest, PositionResponse, QueryBalancesRequest, QueryBalancesResponse,
        QueryInstrumentRefdataRequest,
        QueryInstrumentRefdataResponse, QueryOpenOrderRequest, QueryOrderDetailRequest,
        QueryPositionRequest, QueryTradeDetailRequest, TradeDetailResponse,
    },
};

use crate::{
    latency::system_time_ns,
    oms_actor::{err_response, send_cmd_await, OmsCommand, ReadReplica},
    proto::oms_svc::oms_service_server::OmsService,
};
use zk_oms_rs::utils::gen_timestamp_ms;

// ── Handler struct ────────────────────────────────────────────────────────────

pub struct OmsGrpcHandler {
    pub cmd_tx:  mpsc::Sender<OmsCommand>,
    pub replica: ReadReplica,
    pub oms_id:  Arc<String>,
}

// ── OmsService implementation ─────────────────────────────────────────────────

#[tonic::async_trait]
impl OmsService for OmsGrpcHandler {
    // ── Mutations ────────────────────────────────────────────────────────────

    async fn place_order(
        &self,
        request: Request<PlaceOrderRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        let req = request.into_inner();
        let order_req = req.order_request.ok_or_else(|| {
            Status::invalid_argument("place_order: missing order_request")
        })?;

        // Idempotency: reject duplicate order_id without hitting the writer.
        let order_id = order_req.order_id;
        {
            let snap = self.replica.load();
            if snap.orders.contains_key(&order_id) {
                return Ok(Response::new(err_response(
                    OmsErrorType::OmsErrTypeInvalidReq,
                    &format!("duplicate order_id={order_id}"),
                )));
            }
        }

        let oms_received_ns = system_time_ns(); // t1: OMS gRPC handler entry
        let resp = send_cmd_await(&self.cmd_tx, |reply| OmsCommand::PlaceOrder {
            req: order_req,
            oms_received_ns,
            reply,
        })
        .await?;
        Ok(Response::new(resp))
    }

    async fn batch_place_orders(
        &self,
        request: Request<BatchPlaceOrdersRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        let req = request.into_inner();
        let resp = send_cmd_await(&self.cmd_tx, |reply| OmsCommand::BatchPlaceOrders {
            reqs: req.order_requests,
            reply,
        })
        .await?;
        Ok(Response::new(resp))
    }

    async fn cancel_order(
        &self,
        request: Request<CancelOrderRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        let req = request.into_inner();
        let cancel_req = req.order_cancel_request.ok_or_else(|| {
            Status::invalid_argument("cancel_order: missing order_cancel_request")
        })?;
        let resp = send_cmd_await(&self.cmd_tx, |reply| OmsCommand::CancelOrder {
            req: cancel_req,
            reply,
        })
        .await?;
        Ok(Response::new(resp))
    }

    async fn batch_cancel_orders(
        &self,
        request: Request<BatchCancelOrdersRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        let req = request.into_inner();
        let resp = send_cmd_await(&self.cmd_tx, |reply| OmsCommand::BatchCancelOrders {
            reqs: req.order_cancel_requests,
            reply,
        })
        .await?;
        Ok(Response::new(resp))
    }

    async fn execute_algo_order(
        &self,
        _request: Request<AlgoOrderRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        // TODO: AlgoOrder support — decompose into child orders in writer task.
        Err(Status::unimplemented("AlgoOrder not yet supported"))
    }

    async fn cancel_algo_order(
        &self,
        _request: Request<CancelAlgoOrderRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        Err(Status::unimplemented("CancelAlgoOrder not yet supported"))
    }

    async fn panic(
        &self,
        request: Request<PanicRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        let req = request.into_inner();
        let account_id = req.panic_account_id;
        let resp = send_cmd_await(&self.cmd_tx, |reply| OmsCommand::Panic {
            account_id,
            reply,
        })
        .await?;
        Ok(Response::new(resp))
    }

    async fn dont_panic(
        &self,
        request: Request<DontPanicRequest>,
    ) -> Result<Response<OmsResponse>, Status> {
        let req = request.into_inner();
        let account_id = req.panic_account_id;
        let resp = send_cmd_await(&self.cmd_tx, |reply| OmsCommand::DontPanic {
            account_id,
            reply,
        })
        .await?;
        Ok(Response::new(resp))
    }

    // ── Queries (lock-free read replica) ─────────────────────────────────────

    async fn query_balances(
        &self,
        request: Request<QueryBalancesRequest>,
    ) -> Result<Response<QueryBalancesResponse>, Status> {
        let req  = request.into_inner();
        let snap = self.replica.load();
        let ts   = gen_timestamp_ms();

        let balances: Vec<_> = snap
            .balances
            .get(&req.account_id)
            .map(|acct| acct.values().map(|p| Balance {
                account_id: req.account_id,
                asset: p.position_state.instrument_code.clone(),
                total_qty: p.position_state.total_qty,
                frozen_qty: p.position_state.frozen_qty,
                avail_qty: p.position_state.avail_qty,
                sync_timestamp: p.position_state.sync_timestamp,
                update_timestamp: p.position_state.update_timestamp,
                is_from_exch: p.position_state.is_from_exch,
                exch_data_raw: p.position_state.exch_data_raw.clone(),
            }).collect())
            .unwrap_or_default();

        Ok(Response::new(QueryBalancesResponse {
            account_id: req.account_id,
            balances,
            timestamp:  ts,
        }))
    }

    async fn query_position(
        &self,
        _request: Request<QueryPositionRequest>,
    ) -> Result<Response<PositionResponse>, Status> {
        Err(Status::unimplemented(
            "OMS position query not implemented after balance/position split; \
             use QueryBalances for asset inventory",
        ))
    }

    async fn query_open_orders(
        &self,
        request: Request<QueryOpenOrderRequest>,
    ) -> Result<Response<OrderDetailResponse>, Status> {
        let req  = request.into_inner();
        let snap = self.replica.load();

        let orders: Vec<_> = snap
            .open_order_ids_by_account
            .get(&req.account_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| snap.orders.get(id))
                    .map(|o| o.order_state.clone())
                    .collect()
            })
            .unwrap_or_default();

        Ok(Response::new(OrderDetailResponse {
            orders,
            pagination: None,
        }))
    }

    async fn query_order_details(
        &self,
        request: Request<QueryOrderDetailRequest>,
    ) -> Result<Response<OrderDetailResponse>, Status> {
        let req  = request.into_inner();
        let snap = self.replica.load();

        // `order_refs` are exch_order_refs; do a linear scan (typically small batch).
        let refs: std::collections::HashSet<&str> =
            req.order_refs.iter().map(String::as_str).collect();

        let orders: Vec<_> = snap
            .orders
            .values()
            .filter(|o| {
                o.exch_order_ref
                    .as_deref()
                    .map(|r| refs.contains(r))
                    .unwrap_or(false)
            })
            .map(|o| o.order_state.clone())
            .collect();

        Ok(Response::new(OrderDetailResponse { orders, pagination: None }))
    }

    async fn query_trade_details(
        &self,
        request: Request<QueryTradeDetailRequest>,
    ) -> Result<Response<TradeDetailResponse>, Status> {
        let req  = request.into_inner();
        let snap = self.replica.load();

        let refs: std::collections::HashSet<&str> =
            req.order_refs.iter().map(String::as_str).collect();

        let trades: Vec<_> = snap
            .orders
            .values()
            .filter(|o| {
                o.exch_order_ref
                    .as_deref()
                    .map(|r| refs.contains(r))
                    .unwrap_or(false)
            })
            .flat_map(|o| o.trades.clone())
            .collect();

        Ok(Response::new(TradeDetailResponse { trades, pagination: None }))
    }

    async fn query_instrument_refdata(
        &self,
        _request: Request<QueryInstrumentRefdataRequest>,
    ) -> Result<Response<QueryInstrumentRefdataResponse>, Status> {
        // TODO: serve from ConfdataManager refdata once exposed via OmsSnapshot.
        Ok(Response::new(QueryInstrumentRefdataResponse {
            instrument_refdata: vec![],
        }))
    }

    // ── Health / metadata ─────────────────────────────────────────────────────

    async fn health_check(
        &self,
        _request: Request<DummyRequest>,
    ) -> Result<Response<ServiceHealthResponse>, Status> {
        let snap = self.replica.load();
        let open_orders: usize = snap.open_order_ids_by_account.values().map(|s| s.len()).sum();
        Ok(Response::new(ServiceHealthResponse {
            service_id: self.oms_id.as_ref().clone(),
            status:     "HEALTHY".into(),
            detail:     format!(
                "oms_id={} snapshot_seq={} open_orders={}",
                self.oms_id, snap.seq, open_orders
            ),
            uptime_ms:  0, // TODO: track process start time
        }))
    }
}
