use std::sync::Arc;
use tonic::{Request, Response, Status};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use crate::fill::{publish_booked_report, publish_cancel_report, simulate_fill};
use crate::proto::exch_gw::{BalanceUpdate, PositionReport};
use crate::proto::tqrpc_exch_gw::{
    exchange_gateway_service_server::ExchangeGatewayService,
    ExchAccountResponse, ExchBatchCancelOrdersRequest, ExchBatchSendOrdersRequest,
    ExchFeeResponse, ExchGenericRequest, ExchGenericResponse, ExchOrderDetailResponse,
    ExchOrderTradesResponse, ExchPositionResponse, ExchQueryAccountRequest, ExchQueryFeeRequest,
    ExchQueryOrderDetailRequest, ExchQueryOrderTradesRequest,
    ExchQueryPositionRequest, ExchSendOrderRequest, ExchCancelOrderRequest, GatewayResponse,
};
use crate::state::MockOrder;
use crate::MockGwState;

fn ok_response() -> GatewayResponse {
    use crate::proto::tqrpc_exch_gw::gateway_response::Status as GwStatus;
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
    use crate::proto::tqrpc_exch_gw::gateway_response::Status as GwStatus;
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

pub struct MockGatewayHandler {
    pub state: Arc<Mutex<MockGwState>>,
}

#[tonic::async_trait]
impl ExchangeGatewayService for MockGatewayHandler {
    async fn place_order(
        &self,
        request: Request<ExchSendOrderRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let req = request.into_inner();
        let exch_order_ref = format!("mock_{}", Uuid::new_v4().simple());
        let order_id = req.correlation_id;

        info!(
            exch_order_ref,
            order_id,
            instrument = %req.instrument,
            qty = req.scaled_qty,
            price = req.scaled_price,
            "PlaceOrder received"
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

        // Publish Linkage + BOOKED immediately — before the fill delay.
        if let Some(nats) = nats {
            publish_booked_report(&nats, &gw_id, &exch_order_ref, order_id, account_id, qty, 0).await;
        }

        Ok(Response::new(ok_response()))
    }

    async fn batch_place_orders(
        &self,
        request: Request<ExchBatchSendOrdersRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let reqs = request.into_inner().order_requests;
        for req in reqs {
            let exch_order_ref = format!("mock_{}", Uuid::new_v4().simple());
            let order_id = req.correlation_id;
            info!(exch_order_ref, order_id, "BatchPlaceOrders entry");

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
                publish_booked_report(&nats, &gw_id, &exch_order_ref, order_id, account_id, qty, 0).await;
            }
        }
        Ok(Response::new(ok_response()))
    }

    async fn cancel_order(
        &self,
        request: Request<ExchCancelOrderRequest>,
    ) -> Result<Response<GatewayResponse>, Status> {
        let req = request.into_inner();
        info!(
            exch_order_ref = %req.exch_order_ref,
            order_id = req.order_id,
            "CancelOrder received"
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
            warn!(exch_order_ref = %req.exch_order_ref, "cancel: order not found");
            Ok(Response::new(fail_response("order not found")))
        }
    }

    async fn batch_cancel_orders(
        &self,
        request: Request<ExchBatchCancelOrdersRequest>,
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
        _request: Request<ExchQueryAccountRequest>,
    ) -> Result<Response<ExchAccountResponse>, Status> {
        use crate::proto::common::InstrumentType;
        let s = self.state.lock().await;
        let balances: Vec<PositionReport> = s
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
        Ok(Response::new(ExchAccountResponse {
            exch_account_code: s.account_id.to_string(),
            balance_update: Some(BalanceUpdate { balances }),
            ..Default::default()
        }))
    }

    async fn query_position(
        &self,
        _request: Request<ExchQueryPositionRequest>,
    ) -> Result<Response<ExchPositionResponse>, Status> {
        Ok(Response::new(ExchPositionResponse {}))
    }

    async fn query_order_details(
        &self,
        _request: Request<ExchQueryOrderDetailRequest>,
    ) -> Result<Response<ExchOrderDetailResponse>, Status> {
        Ok(Response::new(ExchOrderDetailResponse { orders: vec![] }))
    }

    async fn query_account_fees(
        &self,
        _request: Request<ExchQueryFeeRequest>,
    ) -> Result<Response<ExchFeeResponse>, Status> {
        Ok(Response::new(ExchFeeResponse { fees: vec![] }))
    }

    async fn query_order_trades(
        &self,
        _request: Request<ExchQueryOrderTradesRequest>,
    ) -> Result<Response<ExchOrderTradesResponse>, Status> {
        Ok(Response::new(ExchOrderTradesResponse {
            ..Default::default()
        }))
    }

    async fn generic_rpc_call(
        &self,
        _request: Request<ExchGenericRequest>,
    ) -> Result<Response<ExchGenericResponse>, Status> {
        Ok(Response::new(ExchGenericResponse {
            response_json: r#"{"status":"ok"}"#.to_string(),
        }))
    }
}
