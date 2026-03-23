//! ConfigIntrospectionService implementation for OMS.
//!
//! Returns the effective (redacted) OMS config with metadata.
//! OMS has no venue secret refs — only bootstrap_token is redacted.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use zk_infra_rs::config_mgmt::{self, ConfigEnvelope};
use zk_proto_rs::zk::config::v1::{GetCurrentConfigRequest, GetCurrentConfigResponse};

use crate::config::{OmsSvcConfig, RESOLVED_SECRET_PATHS};
use crate::proto::config_svc::config_introspection_service_server::ConfigIntrospectionService;

pub struct OmsConfigIntrospection {
    pub envelope: Arc<ConfigEnvelope<OmsSvcConfig>>,
    pub oms_id: Arc<String>,
}

#[tonic::async_trait]
impl ConfigIntrospectionService for OmsConfigIntrospection {
    async fn get_current_config(
        &self,
        _request: Request<GetCurrentConfigRequest>,
    ) -> Result<Response<GetCurrentConfigResponse>, Status> {
        let response = config_mgmt::build_get_current_config_response(
            &self.envelope,
            RESOLVED_SECRET_PATHS,
            "OMS",
            &self.oms_id,
            vec![], // OMS has no secret_ref_statuses
        );
        Ok(Response::new(response))
    }
}
