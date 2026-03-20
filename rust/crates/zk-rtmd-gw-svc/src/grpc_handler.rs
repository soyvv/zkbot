use std::sync::Arc;
use tonic::{Request, Response, Status};
use zk_rtmd_rs::venue_adapter::RtmdVenueAdapter;

use crate::proto::zk_rtmd_v1::rtmd_query_service_server::RtmdQueryService;
use zk_proto_rs::zk::rtmd::v1::{
    HealthCheckRequest, HealthCheckResponse,
    QueryFundingRequest, QueryFundingResponse,
    QueryKlinesRequest, QueryKlinesResponse,
    QueryOrderBookRequest, QueryOrderBookResponse,
    QueryTickRequest, QueryTickResponse,
};

pub struct RtmdQueryHandler {
    pub adapter: Arc<dyn RtmdVenueAdapter>,
}

#[tonic::async_trait]
impl RtmdQueryService for RtmdQueryHandler {
    async fn query_current_tick(
        &self,
        request: Request<QueryTickRequest>,
    ) -> Result<Response<QueryTickResponse>, Status> {
        let code = &request.get_ref().instrument_code;
        match self.adapter.query_current_tick(code).await {
            Ok(tick) => Ok(Response::new(QueryTickResponse { tick: Some(tick) })),
            Err(zk_rtmd_rs::types::RtmdError::NotFound(_)) => {
                Err(Status::not_found(format!("no tick data for {code}")))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn query_current_order_book(
        &self,
        request: Request<QueryOrderBookRequest>,
    ) -> Result<Response<QueryOrderBookResponse>, Status> {
        let req = request.get_ref();
        match self.adapter.query_current_orderbook(&req.instrument_code, req.depth).await {
            Ok(ob) => Ok(Response::new(QueryOrderBookResponse { orderbook: Some(ob) })),
            Err(zk_rtmd_rs::types::RtmdError::NotFound(_)) => {
                Err(Status::not_found(format!("no orderbook for {}", req.instrument_code)))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn query_current_funding(
        &self,
        request: Request<QueryFundingRequest>,
    ) -> Result<Response<QueryFundingResponse>, Status> {
        let code = &request.get_ref().instrument_code;
        match self.adapter.query_current_funding(code).await {
            Ok(f) => Ok(Response::new(QueryFundingResponse { funding: Some(f) })),
            Err(zk_rtmd_rs::types::RtmdError::NotFound(_)) => {
                Err(Status::not_found(format!("no funding data for {code}")))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn query_klines(
        &self,
        request: Request<QueryKlinesRequest>,
    ) -> Result<Response<QueryKlinesResponse>, Status> {
        let req = request.get_ref();
        match self
            .adapter
            .query_klines(&req.instrument_code, &req.interval, req.limit, req.from_ms, req.to_ms)
            .await
        {
            Ok(klines) => Ok(Response::new(QueryKlinesResponse { klines })),
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn health_check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse { status: "ok".to_string() }))
    }
}
