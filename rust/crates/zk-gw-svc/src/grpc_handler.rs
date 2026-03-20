use std::sync::Arc;

use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use zk_proto_rs::zk::exch_gw::v1::BalanceUpdate;

use crate::gw_executor::{GwExecAction, GwExecPool};
use crate::proto::zk_gw_v1::gateway_service_server::GatewayService;
use crate::proto::zk_gw_v1::*;
use crate::reconnect::GatewayState;
use crate::venue_adapter::*;

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
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let correlation_id = req.correlation_id;
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
            let correlation_id = req.correlation_id;
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
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let order_id = req.order_id;
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
            let order_id = req.order_id;
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
        Ok(Response::new(PositionResponse {}))
    }

    async fn query_order_details(
        &self,
        _request: Request<QueryOrderDetailRequest>,
    ) -> Result<Response<OrderDetailResponse>, Status> {
        Ok(Response::new(OrderDetailResponse { orders: vec![] }))
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
