use std::sync::Arc;

use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use zk_proto_rs::zk::exch_gw::v1::BalanceUpdate;

use crate::proto::zk_gw_v1::gateway_service_server::GatewayService;
use crate::proto::zk_gw_v1::*;
use crate::reconnect::GatewayState;
use crate::venue_adapter::*;

/// GatewayService gRPC handler.
///
/// Delegates all RPCs to the underlying VenueAdapter (simulator or real venue).
pub struct GrpcHandler {
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

        match self.adapter.place_order(venue_req).await {
            Ok(ack) => {
                let status = if ack.success {
                    gateway_response::Status::GwRespStatusSuccess as i32
                } else {
                    gateway_response::Status::GwRespStatusFail as i32
                };
                Ok(Response::new(GatewayResponse {
                    timestamp: ts,
                    status,
                    message: ack.error_message.unwrap_or_default(),
                    ..Default::default()
                }))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn batch_place_orders(
        &self,
        request: Request<BatchSendOrdersRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let batch = request.into_inner();
        for req in batch.order_requests {
            let wrapped = Request::new(req);
            self.place_order(wrapped).await?;
        }
        Ok(Response::new(GatewayResponse {
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

        let venue_req = VenueCancelOrder {
            exch_order_ref: req.exch_order_ref,
            order_id: req.order_id,
            timestamp: req.timestamp,
        };

        match self.adapter.cancel_order(venue_req).await {
            Ok(ack) => {
                let status = if ack.success {
                    gateway_response::Status::GwRespStatusSuccess as i32
                } else {
                    gateway_response::Status::GwRespStatusFail as i32
                };
                Ok(Response::new(GatewayResponse {
                    timestamp: ts,
                    status,
                    message: ack.error_message.unwrap_or_default(),
                    ..Default::default()
                }))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn batch_cancel_orders(
        &self,
        request: Request<BatchCancelOrdersRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let batch = request.into_inner();
        for req in batch.cancel_requests {
            let wrapped = Request::new(req);
            self.cancel_order(wrapped).await?;
        }
        Ok(Response::new(GatewayResponse {
            status: gateway_response::Status::GwRespStatusSuccess as i32,
            ..Default::default()
        }))
    }

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
