use std::sync::Arc;

use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use zk_proto_rs::zk::exch_gw::v1::{
    BalanceUpdate, ExchangeOrderStatus, OrderReportEntry, OrderReportType, OrderStateReport,
};

use crate::gw_executor::{GwExecAction, GwExecPool};
use crate::proto::zk_gw_v1::gateway_service_server::GatewayService;
use crate::proto::zk_gw_v1::*;
use crate::reconnect::GatewayState;
use crate::venue_adapter::*;

fn validate_send_order_request(req: &SendOrderRequest) -> Result<(), &'static str> {
    if req.correlation_id <= 0 {
        return Err("place_order: correlation_id must be > 0");
    }
    if req.exch_account_id.trim().is_empty() {
        return Err("place_order: exch_account_id is required");
    }
    if req.instrument.trim().is_empty() {
        return Err("place_order: instrument is required");
    }
    if !req.scaled_qty.is_finite() || req.scaled_qty <= 0.0 {
        return Err("place_order: scaled_qty must be finite and > 0");
    }
    if !req.scaled_price.is_finite() {
        return Err("place_order: scaled_price must be finite");
    }
    Ok(())
}

fn validate_cancel_order_request(req: &CancelOrderRequest) -> Result<(), &'static str> {
    if req.order_id <= 0 {
        return Err("cancel_order: order_id must be > 0");
    }
    Ok(())
}

/// GatewayService gRPC handler.
///
/// Execution commands (place/cancel) are dispatched to the internal execution
/// pool. gRPC success = validated + accepted for async processing. It does NOT
/// guarantee enqueue to a worker or venue acceptance. Queue-full drops publish
/// synthetic rejection reports asynchronously via NATS.
/// Query RPCs still call the adapter synchronously.
pub struct GrpcHandler {
    pub exec_pool: Arc<GwExecPool>,
    /// Adapter kept for query RPCs (balance, position, orders, trades).
    pub adapter: Arc<dyn VenueAdapter>,
    pub gw_state: Arc<Mutex<GatewayState>>,
    pub account_id: i64,
}

#[tonic::async_trait]
impl GatewayService for GrpcHandler {
    async fn place_order(
        &self,
        request: Request<SendOrderRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let req = request.into_inner();
        if let Err(msg) = validate_send_order_request(&req) {
            warn!(
                account_id = self.account_id,
                correlation_id = req.correlation_id,
                exch_account_id = %req.exch_account_id,
                instrument = %req.instrument,
                scaled_qty = req.scaled_qty,
                scaled_price = req.scaled_price,
                "rejecting invalid gateway place_order request: {msg}"
            );
            return Err(Status::invalid_argument(msg));
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let correlation_id = req.correlation_id;
        let exch_account_id = req.exch_account_id.clone();
        let instrument = req.instrument.clone();
        let scaled_qty = req.scaled_qty;
        let scaled_price = req.scaled_price;
        let venue_req = VenuePlaceOrder {
            correlation_id: req.correlation_id,
            exch_account_id: req.exch_account_id,
            instrument: req.instrument,
            buysell_type: req.buysell_type,
            openclose_type: req.openclose_type,
            order_type: req.order_type,
            price: req.scaled_price,
            qty: req.scaled_qty,
            leverage: req.leverage,
            timestamp: req.timestamp,
        };

        self.exec_pool.dispatch_or_reject(
            correlation_id,
            GwExecAction::PlaceOrder {
                venue_req,
                correlation_id,
            },
        );
        info!(
            account_id = self.account_id,
            correlation_id,
            exch_account_id = %exch_account_id,
            instrument = %instrument,
            scaled_qty,
            scaled_price,
            "accepted gateway place_order request for async execution"
        );
        Ok(Response::new(GatewayResponse {
            timestamp: ts,
            status: gateway_response::Status::GwRespStatusSuccess as i32,
            ..Default::default()
        }))
    }

    async fn batch_place_orders(
        &self,
        request: Request<BatchSendOrdersRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let batch = request.into_inner();
        for req in batch.order_requests {
            if let Err(msg) = validate_send_order_request(&req) {
                warn!(
                    account_id = self.account_id,
                    correlation_id = req.correlation_id,
                    "rejecting invalid gateway batch place_order request: {msg}"
                );
                return Err(Status::invalid_argument(msg));
            }
            let correlation_id = req.correlation_id;
            let instrument = req.instrument.clone();
            let scaled_qty = req.scaled_qty;
            let scaled_price = req.scaled_price;
            let venue_req = VenuePlaceOrder {
                correlation_id: req.correlation_id,
                exch_account_id: req.exch_account_id,
                instrument: req.instrument,
                buysell_type: req.buysell_type,
                openclose_type: req.openclose_type,
                order_type: req.order_type,
                price: req.scaled_price,
                qty: req.scaled_qty,
                leverage: req.leverage,
                timestamp: req.timestamp,
            };
            self.exec_pool.dispatch_or_reject(
                correlation_id,
                GwExecAction::PlaceOrder {
                    venue_req,
                    correlation_id,
                },
            );
            info!(
                account_id = self.account_id,
                correlation_id,
                instrument = %instrument,
                scaled_qty,
                scaled_price,
                "accepted gateway batch place_order item for async execution"
            );
        }
        Ok(Response::new(GatewayResponse {
            timestamp: ts,
            status: gateway_response::Status::GwRespStatusSuccess as i32,
            ..Default::default()
        }))
    }

    async fn cancel_order(
        &self,
        request: Request<CancelOrderRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let req = request.into_inner();
        if let Err(msg) = validate_cancel_order_request(&req) {
            warn!(
                account_id = self.account_id,
                order_id = req.order_id,
                "rejecting invalid gateway cancel_order request: {msg}"
            );
            return Err(Status::invalid_argument(msg));
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let order_id = req.order_id;
        let exch_order_ref = req.exch_order_ref.clone();
        let venue_req = VenueCancelOrder {
            exch_order_ref: req.exch_order_ref,
            order_id: req.order_id,
            timestamp: req.timestamp,
        };

        self.exec_pool.dispatch_or_reject(
            order_id,
            GwExecAction::CancelOrder {
                venue_req,
                order_id,
            },
        );
        info!(
            account_id = self.account_id,
            order_id,
            exch_order_ref = %exch_order_ref,
            "accepted gateway cancel_order request for async execution"
        );
        Ok(Response::new(GatewayResponse {
            timestamp: ts,
            status: gateway_response::Status::GwRespStatusSuccess as i32,
            ..Default::default()
        }))
    }

    async fn batch_cancel_orders(
        &self,
        request: Request<BatchCancelOrdersRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let batch = request.into_inner();
        for req in batch.cancel_requests {
            if let Err(msg) = validate_cancel_order_request(&req) {
                warn!(
                    account_id = self.account_id,
                    order_id = req.order_id,
                    "rejecting invalid gateway batch cancel_order request: {msg}"
                );
                return Err(Status::invalid_argument(msg));
            }
            let order_id = req.order_id;
            let exch_order_ref = req.exch_order_ref.clone();
            let venue_req = VenueCancelOrder {
                exch_order_ref: req.exch_order_ref,
                order_id: req.order_id,
                timestamp: req.timestamp,
            };
            self.exec_pool.dispatch_or_reject(
                order_id,
                GwExecAction::CancelOrder {
                    venue_req,
                    order_id,
                },
            );
            info!(
                account_id = self.account_id,
                order_id,
                exch_order_ref = %exch_order_ref,
                "accepted gateway batch cancel_order item for async execution"
            );
        }
        Ok(Response::new(GatewayResponse {
            timestamp: ts,
            status: gateway_response::Status::GwRespStatusSuccess as i32,
            ..Default::default()
        }))
    }

    // ── Query RPCs — still synchronous via adapter ──────────────────────────

    async fn query_account_balance(
        &self,
        request: Request<QueryAccountRequest>,
    ) -> Result<Response<AccountResponse>, Status> {
        let req = request.into_inner();
        let venue_req = VenueBalanceQuery {
            explicit_symbols: req.explicit_symbols,
        };

        match self.adapter.query_balance(venue_req).await {
            Ok(facts) => {
                let balances: Vec<zk_proto_rs::zk::exch_gw::v1::PositionReport> = facts
                    .iter()
                    .map(|f| zk_proto_rs::zk::exch_gw::v1::PositionReport {
                        instrument_code: f.asset.clone(),
                        instrument_type: zk_proto_rs::zk::common::v1::InstrumentType::InstTypeSpot
                            as i32,
                        qty: f.total_qty,
                        avail_qty: f.avail_qty,
                        account_id: self.account_id,
                        ..Default::default()
                    })
                    .collect();

                Ok(Response::new(AccountResponse {
                    balance_update: Some(BalanceUpdate { balances }),
                    ..Default::default()
                }))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn query_position(
        &self,
        _request: Request<QueryPositionRequest>,
    ) -> Result<Response<PositionResponse>, Status> {
        let venue_req = VenuePositionQuery {};
        match self.adapter.query_positions(venue_req).await {
            Ok(facts) => {
                let positions = facts.iter().map(venue_position_fact_to_proto).collect();
                Ok(Response::new(PositionResponse { positions }))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn query_order_details(
        &self,
        request: Request<QueryOrderDetailRequest>,
    ) -> Result<Response<OrderDetailResponse>, Status> {
        let req = request.into_inner();
        let mut orders = Vec::new();
        for q in req.order_queries {
            let venue_req = VenueOrderQuery {
                exch_order_ref: if q.exch_order_ref.is_empty() {
                    None
                } else {
                    Some(q.exch_order_ref)
                },
                order_id: if q.order_id == 0 {
                    None
                } else {
                    Some(q.order_id)
                },
                instrument: if q.symbol.is_empty() {
                    None
                } else {
                    Some(q.symbol)
                },
            };
            match self.adapter.query_order(venue_req).await {
                Ok(facts) => {
                    for f in facts {
                        orders.push(venue_order_fact_to_exch_order(&f));
                    }
                }
                Err(e) => warn!(error = %e, "query_order failed for single query"),
            }
        }
        Ok(Response::new(OrderDetailResponse { orders }))
    }

    async fn query_open_orders(
        &self,
        _request: Request<QueryOpenOrderRequest>,
    ) -> Result<Response<OrderDetailResponse>, Status> {
        let venue_req = VenueOpenOrdersQuery::default();
        match self.adapter.query_open_orders(venue_req).await {
            Ok(facts) => {
                let orders = facts.iter().map(venue_order_fact_to_exch_order).collect();
                Ok(Response::new(OrderDetailResponse { orders }))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn query_account_fees(
        &self,
        _request: Request<QueryFeeRequest>,
    ) -> Result<Response<FeeResponse>, Status> {
        Ok(Response::new(FeeResponse { fees: vec![] }))
    }

    async fn query_order_trades(
        &self,
        _request: Request<QueryOrderTradesRequest>,
    ) -> Result<Response<OrderTradesResponse>, Status> {
        Ok(Response::new(OrderTradesResponse::default()))
    }

    async fn generic_rpc_call(
        &self,
        _request: Request<GenericRequest>,
    ) -> Result<Response<GenericResponse>, Status> {
        Ok(Response::new(GenericResponse {
            response_json: "{}".to_string(),
        }))
    }

    async fn health_check(
        &self,
        _request: Request<zk_proto_rs::zk::common::v1::DummyRequest>,
    ) -> Result<Response<zk_proto_rs::zk::common::v1::ServiceHealthResponse>, Status> {
        let state = self.gw_state.lock().await;
        let status_str = if state.is_serving() {
            "SERVING"
        } else {
            "NOT_SERVING"
        };
        Ok(Response::new(
            zk_proto_rs::zk::common::v1::ServiceHealthResponse {
                service_id: String::new(),
                status: status_str.to_string(),
                detail: format!("gateway state: {state}"),
                uptime_ms: 0,
            },
        ))
    }
}

// ── Conversion helpers ──────────────────────────────────────────────────────

fn venue_order_status_to_exch(s: &VenueOrderStatus) -> i32 {
    match s {
        VenueOrderStatus::Booked => ExchangeOrderStatus::ExchOrderStatusBooked as i32,
        VenueOrderStatus::PartiallyFilled => {
            ExchangeOrderStatus::ExchOrderStatusPartialFilled as i32
        }
        VenueOrderStatus::Filled => ExchangeOrderStatus::ExchOrderStatusFilled as i32,
        VenueOrderStatus::Cancelled => ExchangeOrderStatus::ExchOrderStatusCancelled as i32,
        VenueOrderStatus::Rejected => ExchangeOrderStatus::ExchOrderStatusExchRejected as i32,
    }
}

fn venue_order_fact_to_exch_order(f: &VenueOrderFact) -> ExchOrder {
    let state_report = OrderStateReport {
        exch_order_status: venue_order_status_to_exch(&f.status),
        filled_qty: f.filled_qty,
        unfilled_qty: f.unfilled_qty,
        avg_price: f.avg_price,
        ..Default::default()
    };
    let entry = OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeState as i32,
        report: Some(
            zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::OrderStateReport(
                state_report,
            ),
        ),
    };
    ExchOrder {
        order_ref: f.exch_order_ref.clone(),
        instrument: f.instrument.clone(),
        order_report: Some(zk_proto_rs::zk::exch_gw::v1::OrderReport {
            exch_order_ref: f.exch_order_ref.clone(),
            order_id: f.order_id,
            order_report_entries: vec![entry],
            ..Default::default()
        }),
        timestamp: f.timestamp,
        ..Default::default()
    }
}

fn infer_instrument_type(instrument: &str) -> i32 {
    use zk_proto_rs::zk::common::v1::InstrumentType;

    if instrument.contains("SWAP") {
        InstrumentType::InstTypePerp as i32
    } else if instrument.contains("FUTURES")
        || instrument.contains('-') && instrument.matches('-').count() >= 2
    {
        InstrumentType::InstTypeFuture as i32
    } else if instrument.contains('_') {
        InstrumentType::InstTypeCfd as i32
    } else {
        InstrumentType::InstTypeSpot as i32
    }
}

fn venue_position_fact_to_proto(f: &VenuePositionFact) -> zk_proto_rs::zk::oms::v1::Position {
    let inst_type = if f.instrument_type != 0 {
        f.instrument_type
    } else {
        infer_instrument_type(&f.instrument)
    };
    zk_proto_rs::zk::oms::v1::Position {
        account_id: f.account_id,
        instrument_code: f.instrument.clone(),
        long_short_type: f.long_short_type,
        instrument_type: inst_type,
        total_qty: f.qty,
        frozen_qty: f.frozen_qty,
        avail_qty: f.avail_qty,
        is_from_exch: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        infer_instrument_type, validate_cancel_order_request, validate_send_order_request,
        venue_position_fact_to_proto,
    };
    use crate::proto::zk_gw_v1::{CancelOrderRequest, SendOrderRequest};
    use crate::venue_adapter::VenuePositionFact;
    use zk_proto_rs::zk::common::v1::{InstrumentType, LongShortType};

    #[test]
    fn place_order_validation_rejects_zero_correlation_id() {
        let req = SendOrderRequest {
            correlation_id: 0,
            exch_account_id: "acc".into(),
            instrument: "BTC-USDT".into(),
            scaled_qty: 1.0,
            scaled_price: 1.0,
            ..Default::default()
        };
        assert_eq!(
            validate_send_order_request(&req),
            Err("place_order: correlation_id must be > 0")
        );
    }

    #[test]
    fn place_order_validation_rejects_empty_instrument() {
        let req = SendOrderRequest {
            correlation_id: 1,
            exch_account_id: "acc".into(),
            instrument: " ".into(),
            scaled_qty: 1.0,
            scaled_price: 1.0,
            ..Default::default()
        };
        assert_eq!(
            validate_send_order_request(&req),
            Err("place_order: instrument is required")
        );
    }

    #[test]
    fn cancel_order_validation_rejects_zero_order_id() {
        let req = CancelOrderRequest {
            order_id: 0,
            ..Default::default()
        };
        assert_eq!(
            validate_cancel_order_request(&req),
            Err("cancel_order: order_id must be > 0")
        );
    }

    #[test]
    fn infer_instrument_type_handles_perp_and_spot() {
        assert_eq!(
            infer_instrument_type("BTC-USDT-SWAP"),
            InstrumentType::InstTypePerp as i32
        );
        assert_eq!(
            infer_instrument_type("BTC-USDT"),
            InstrumentType::InstTypeSpot as i32
        );
        assert_eq!(
            infer_instrument_type("BTC_USD"),
            InstrumentType::InstTypeCfd as i32
        );
    }

    #[test]
    fn venue_position_fact_to_proto_maps_fields() {
        let fact = VenuePositionFact {
            instrument: "BTC-USDT-SWAP".into(),
            long_short_type: LongShortType::LsLong as i32,
            qty: 2.5,
            avail_qty: 2.0,
            frozen_qty: 0.5,
            account_id: 9001,
            instrument_type: 0,
        };
        let proto = venue_position_fact_to_proto(&fact);
        assert_eq!(proto.account_id, 9001);
        assert_eq!(proto.instrument_code, "BTC-USDT-SWAP");
        assert_eq!(proto.instrument_type, InstrumentType::InstTypePerp as i32);
        assert_eq!(proto.total_qty, 2.5);
        assert_eq!(proto.avail_qty, 2.0);
        assert_eq!(proto.frozen_qty, 0.5);
        assert!(proto.is_from_exch);
    }

    #[test]
    fn venue_position_fact_explicit_instrument_type_overrides_heuristic() {
        // OANDA-style: underscore symbol would be "spot" by heuristic,
        // but explicit instrument_type=4 (CFD) takes precedence.
        let fact = VenuePositionFact {
            instrument: "BTC_USD".into(),
            long_short_type: LongShortType::LsLong as i32,
            qty: 0.01,
            avail_qty: 0.01,
            frozen_qty: 0.0,
            account_id: 8003,
            instrument_type: InstrumentType::InstTypeCfd as i32,
        };
        let proto = venue_position_fact_to_proto(&fact);
        assert_eq!(proto.instrument_type, InstrumentType::InstTypeCfd as i32);
        assert_eq!(proto.instrument_code, "BTC_USD");
    }
}
