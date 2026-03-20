//! Implements `zk.gateway.v1.GatewayService` — the new gateway API used by `zk-oms-svc`.
//!
//! Delegates to the same `MockGwState` as the legacy `ExchangeGatewayService`.

use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::info;
use uuid::Uuid;

use crate::fill::{publish_booked_report, publish_cancel_report, simulate_fill, system_time_ns};
use crate::proto::zk_gw_v1::{
    gateway_service_server::GatewayService, AccountResponse, BatchCancelOrdersRequest,
    BatchSendOrdersRequest, CancelOrderRequest, FeeResponse, GatewayResponse, GenericRequest,
    GenericResponse, OrderDetailResponse, OrderTradesResponse, PositionResponse,
    QueryAccountRequest, QueryFeeRequest, QueryOrderDetailRequest, QueryOrderTradesRequest,
    QueryPositionRequest, SendOrderRequest,
};
use crate::state::MockOrder;
use crate::MockGwState;
use zk_proto_rs::zk::common::v1::DummyRequest;
use zk_proto_rs::zk::common::v1::ServiceHealthResponse;

fn ok_response() -> GatewayResponse {
    use crate::proto::zk_gw_v1::gateway_response::Status as GwStatus;
    use std::time::{SystemTime, UNIX_EPOCH};
    GatewayResponse {
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        status: GwStatus::GwRespStatusSuccess as i32,
        ..Default::default()
    }
}

fn fail_response(msg: &str) -> GatewayResponse {
    use crate::proto::zk_gw_v1::gateway_response::Status as GwStatus;
    use std::time::{SystemTime, UNIX_EPOCH};
    GatewayResponse {
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        status: GwStatus::GwRespStatusFail as i32,
        message: msg.to_string(),
        ..Default::default()
    }
}

pub struct ZkGatewayHandler {
    pub state: Arc<Mutex<MockGwState>>,
}

#[tonic::async_trait]
impl GatewayService for ZkGatewayHandler {
    async fn place_order(
        &self,
        request: Request<SendOrderRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let t4_ns = system_time_ns(); // t4: GW handler entry timestamp
        let req = request.into_inner();
        let exch_order_ref = format!("mock_{}", Uuid::new_v4().simple());
        let order_id = req.correlation_id;

        info!(
            exch_order_ref,
            order_id,
            instrument = %req.instrument,
            qty = req.scaled_qty,
            price = req.scaled_price,
            "GatewayService.PlaceOrder received"
        );

        let (account_id, qty, nats, gw_id) = {
            let mut s = self.state.lock().await;
            let account_id = s.account_id;
            let qty = req.scaled_qty;
            let order = MockOrder {
                exch_order_ref: exch_order_ref.clone(),
                order_id,
                account_id,
                instrument: req.instrument.clone(),
                side: req.buysell_type,
                qty,
                price: req.scaled_price,
                filled_qty: 0.0,
            };
            s.orders.insert(exch_order_ref.clone(), order);
            let handle = tokio::spawn(simulate_fill(
                Arc::clone(&self.state),
                exch_order_ref.clone(),
                order_id,
            ));
            s.fill_tasks.insert(exch_order_ref.clone(), handle);
            (account_id, qty, s.nats_client.clone(), s.gw_id.clone())
        };

        if let Some(nats) = nats {
            publish_booked_report(
                &nats,
                &gw_id,
                &exch_order_ref,
                order_id,
                account_id,
                qty,
                t4_ns,
            )
            .await;
        }

        Ok(Response::new(ok_response()))
    }

    async fn batch_place_orders(
        &self,
        request: Request<BatchSendOrdersRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let reqs = request.into_inner().order_requests;
        for req in reqs {
            let exch_order_ref = format!("mock_{}", Uuid::new_v4().simple());
            let order_id = req.correlation_id;
            info!(
                exch_order_ref,
                order_id, "GatewayService.BatchPlaceOrders entry"
            );

            let (account_id, qty, nats, gw_id) = {
                let mut s = self.state.lock().await;
                let account_id = s.account_id;
                let qty = req.scaled_qty;
                let order = MockOrder {
                    exch_order_ref: exch_order_ref.clone(),
                    order_id,
                    account_id,
                    instrument: req.instrument.clone(),
                    side: req.buysell_type,
                    qty,
                    price: req.scaled_price,
                    filled_qty: 0.0,
                };
                s.orders.insert(exch_order_ref.clone(), order);
                let handle = tokio::spawn(simulate_fill(
                    Arc::clone(&self.state),
                    exch_order_ref.clone(),
                    order_id,
                ));
                s.fill_tasks.insert(exch_order_ref.clone(), handle);
                (account_id, qty, s.nats_client.clone(), s.gw_id.clone())
            };

            if let Some(nats) = nats {
                // batch: no per-order t4 capture; use 0
                publish_booked_report(&nats, &gw_id, &exch_order_ref, order_id, account_id, qty, 0)
                    .await;
            }
        }
        Ok(Response::new(ok_response()))
    }

    async fn cancel_order(
        &self,
        request: Request<CancelOrderRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let req = request.into_inner();
        info!(
            exch_order_ref = %req.exch_order_ref,
            order_id = req.order_id,
            "GatewayService.CancelOrder received"
        );

        let (account_id, nats, gw_id) = {
            let mut s = self.state.lock().await;
            let nats = s.nats_client.clone();
            let gw_id = s.gw_id.clone();
            if let Some(handle) = s.fill_tasks.remove(&req.exch_order_ref) {
                handle.abort();
            }
            let account_id = s.orders.remove(&req.exch_order_ref).map(|o| o.account_id);
            (account_id, nats, gw_id)
        };

        if let Some(account_id) = account_id {
            if let Some(nats) = nats {
                publish_cancel_report(&nats, &gw_id, &req.exch_order_ref, req.order_id, account_id)
                    .await;
            }
            Ok(Response::new(ok_response()))
        } else {
            tracing::warn!(exch_order_ref = %req.exch_order_ref, "cancel: order not found");
            Ok(Response::new(fail_response("order not found")))
        }
    }

    async fn batch_cancel_orders(
        &self,
        request: Request<BatchCancelOrdersRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let reqs = request.into_inner().cancel_requests;
        for req in reqs {
            let mut s = self.state.lock().await;
            if let Some(handle) = s.fill_tasks.remove(&req.exch_order_ref) {
                handle.abort();
            }
            s.orders.remove(&req.exch_order_ref);
        }
        Ok(Response::new(ok_response()))
    }

    async fn query_account_balance(
        &self,
        _request: Request<QueryAccountRequest>,
    ) -> Result<Response<AccountResponse>, Status> {
        use zk_proto_rs::zk::common::v1::InstrumentType;
        use zk_proto_rs::zk::exch_gw::v1::{BalanceUpdate, PositionReport};
        let s = self.state.lock().await;
        let entries: Vec<PositionReport> = s
            .balances
            .iter()
            .map(|(symbol, &qty)| PositionReport {
                instrument_code: symbol.clone(),
                instrument_type: InstrumentType::InstTypeSpot as i32,
                qty,
                avail_qty: qty,
                account_id: s.account_id,
                ..Default::default()
            })
            .collect();
        Ok(Response::new(AccountResponse {
            exch_account_code: s.account_id.to_string(),
            balance_update: Some(BalanceUpdate { balances: entries }),
            timestamp: system_time_ns() / 1_000_000,
            ..Default::default()
        }))
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
        Ok(Response::new(OrderTradesResponse {
            ..Default::default()
        }))
    }

    async fn generic_rpc_call(
        &self,
        _request: Request<GenericRequest>,
    ) -> Result<Response<GenericResponse>, Status> {
        Ok(Response::new(GenericResponse {
            response_json: r#"{"status":"ok"}"#.to_string(),
        }))
    }

    async fn health_check(
        &self,
        _request: Request<DummyRequest>,
    ) -> Result<Response<ServiceHealthResponse>, Status> {
        Ok(Response::new(ServiceHealthResponse {
            service_id: "mock-gw".to_string(),
            status: "healthy".to_string(),
            ..Default::default()
        }))
    }
}
