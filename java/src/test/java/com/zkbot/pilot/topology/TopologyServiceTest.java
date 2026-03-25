package com.zkbot.pilot.topology;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.zkbot.pilot.bootstrap.TokenService;
import com.zkbot.pilot.config.ConfigDriftService;
import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.schema.ConfigSchemaLocator;
import com.zkbot.pilot.schema.SchemaService;
import com.zkbot.pilot.topology.dto.*;
import io.nats.client.Connection;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Nested;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import org.springframework.web.server.ResponseStatusException;
import zk.discovery.v1.Discovery.TransportEndpoint;
import zk.discovery.v1.Discovery.ServiceRegistration;

import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;
import static org.mockito.ArgumentMatchers.*;
import static org.mockito.Mockito.*;

@ExtendWith(MockitoExtension.class)
class TopologyServiceTest {

    @Mock TopologyRepository repository;
    @Mock DiscoveryCache discoveryCache;
    @Mock TokenService tokenService;
    @Mock PilotProperties props;
    @Mock Connection natsConnection;
    @Mock ConfigDriftService configDriftService;
    @Mock SchemaService schemaService;
    @Mock ConfigSchemaLocator configSchemaLocator;

    TopologyService service;

    @BeforeEach
    void setUp() {
        service = new TopologyService(repository, discoveryCache, tokenService, props,
                natsConnection, configDriftService, schemaService, configSchemaLocator,
                new ObjectMapper());
    }

    // ── Workspace scoping ─────────────────────────────────────────────────

    @Nested
    class WorkspaceScoping {

        @Test
        void scoped_by_oms_includes_bound_gw_and_excludes_unbound() {
            when(props.env()).thenReturn("test");
            var instances = List.of(
                    inst("oms_1", "OMS"),
                    inst("gw_1", "GW"),
                    inst("gw_2", "GW")
            );
            var bindings = List.of(
                    binding("oms_1", "gw_1")
            );
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(bindings);
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology("oms_1");

            var ids = view.nodes().stream().map(ServiceNode::logicalId).toList();
            assertThat(ids).contains("oms_1", "gw_1");
            assertThat(ids).doesNotContain("gw_2");
        }

        @Test
        void scoped_includes_refdata_regardless_of_bindings() {
            when(props.env()).thenReturn("test");
            var instances = List.of(
                    inst("oms_1", "OMS"),
                    inst("refdata_1", "REFDATA")
            );
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(List.of());
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology("oms_1");

            var ids = view.nodes().stream().map(ServiceNode::logicalId).toList();
            assertThat(ids).contains("oms_1", "refdata_1");
        }

        @Test
        void scoped_includes_mdgw_only_when_bound_to_scoped_service() {
            when(props.env()).thenReturn("test");
            var instances = List.of(
                    inst("oms_1", "OMS"),
                    inst("gw_1", "GW"),
                    inst("mdgw_1", "MDGW")
            );
            var bindings = List.of(
                    binding("oms_1", "gw_1"),
                    binding("mdgw_1", "gw_1")  // mdgw bound to gw_1 which is in scope
            );
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(bindings);
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology("oms_1");

            var ids = view.nodes().stream().map(ServiceNode::logicalId).toList();
            assertThat(ids).contains("mdgw_1");
        }

        @Test
        void scoped_excludes_mdgw_bound_to_out_of_scope_service() {
            when(props.env()).thenReturn("test");
            var instances = List.of(
                    inst("oms_1", "OMS"),
                    inst("gw_2", "GW"),
                    inst("mdgw_1", "MDGW")
            );
            var bindings = List.of(
                    binding("mdgw_1", "gw_2")  // gw_2 not bound to oms_1
            );
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(bindings);
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology("oms_1");

            var ids = view.nodes().stream().map(ServiceNode::logicalId).toList();
            assertThat(ids).doesNotContain("mdgw_1");
        }

        @Test
        void unscoped_returns_all_instances() {
            when(props.env()).thenReturn("test");
            var instances = List.of(
                    inst("oms_1", "OMS"),
                    inst("gw_1", "GW"),
                    inst("gw_2", "GW")
            );
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(List.of());
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology(null);

            assertThat(view.nodes()).hasSize(3);
        }

        @Test
        void edges_filtered_to_scoped_endpoints() {
            when(props.env()).thenReturn("test");
            var instances = List.of(
                    inst("oms_1", "OMS"),
                    inst("gw_1", "GW"),
                    inst("gw_2", "GW")
            );
            var bindings = List.of(
                    binding("oms_1", "gw_1"),
                    binding("oms_1", "gw_2")  // gw_2 is not bound to oms_1 (this binding exists but gw_2 isn't a workspace type bound neighbor)
            );
            // With oms_1 as scope: oms_1 + gw_1 + gw_2 (both are direct neighbors of type GW)
            // Actually both gw_1 and gw_2 ARE direct neighbors via bindings, and GW is in OMS_WORKSPACE_TYPES
            // So both edges should be included
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(bindings);
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology("oms_1");

            assertThat(view.edges()).hasSize(2);
        }
    }

    // ── Service node mapping ──────────────────────────────────────────────

    @Nested
    class NodeMapping {

        @Test
        void maps_online_status_from_discovery() {
            when(props.env()).thenReturn("test");
            var instances = List.of(inst("oms_1", "OMS"));
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(List.of());

            var reg = ServiceRegistration.newBuilder()
                    .setServiceId("oms_1")
                    .setServiceType("OMS")
                    .setEndpoint(TransportEndpoint.newBuilder().setAddress("localhost:9090"))
                    .setUpdatedAtMs(System.currentTimeMillis())
                    .build();
            when(discoveryCache.getAll()).thenReturn(Map.of("svc.oms.oms_1", reg));

            TopologyView view = service.getTopology(null);

            assertThat(view.nodes().getFirst().status()).isEqualTo("online");
            assertThat(view.nodes().getFirst().endpoint()).isEqualTo("localhost:9090");
        }

        @Test
        void maps_offline_when_not_in_discovery() {
            when(props.env()).thenReturn("test");
            var instances = List.of(inst("oms_1", "OMS"));
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(List.of());
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology(null);

            assertThat(view.nodes().getFirst().status()).isEqualTo("offline");
            assertThat(view.nodes().getFirst().endpoint()).isNull();
        }

        @Test
        void maps_desired_enabled_from_db() {
            when(props.env()).thenReturn("test");
            var instances = List.of(inst("oms_1", "OMS", false));
            when(repository.listLogicalInstances("test", null)).thenReturn(instances);
            when(repository.listBindings(null, null)).thenReturn(List.of());
            when(discoveryCache.getAll()).thenReturn(Map.of());

            TopologyView view = service.getTopology(null);

            assertThat(view.nodes().getFirst().desiredEnabled()).isFalse();
        }
    }

    // ── Service detail ────────────────────────────────────────────────────

    @Nested
    class ServiceDetail {

        @Test
        void returns_null_when_kind_mismatches_stored_type() {
            when(repository.getLogicalInstance("gw_1")).thenReturn(
                    Map.of("logical_id", "gw_1", "instance_type", "GW", "enabled", true));

            var detail = service.getServiceDetail("OMS", "gw_1");

            assertThat(detail).isNull();
        }

        @Test
        void returns_detail_when_kind_matches_case_insensitive() {
            when(repository.getLogicalInstance("gw_1")).thenReturn(
                    Map.of("logical_id", "gw_1", "instance_type", "GW", "enabled", true));
            when(discoveryCache.getAll()).thenReturn(Map.of());
            when(repository.getBindingsForService("gw_1")).thenReturn(List.of());
            when(repository.listActiveSessions()).thenReturn(List.of());

            var detail = service.getServiceDetail("gw", "gw_1");

            assertThat(detail).isNotNull();
            assertThat(detail.logicalId()).isEqualTo("gw_1");
        }
    }

    // ── Reload ────────────────────────────────────────────────────────────

    @Nested
    class Reload {

        @Test
        void validates_kind_against_stored_type() {
            when(repository.getLogicalInstance("gw_1")).thenReturn(
                    Map.of("logical_id", "gw_1", "instance_type", "GW", "enabled", true));

            assertThatThrownBy(() -> service.reloadService("OMS", "gw_1"))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not found");
        }

        @Test
        void returns_409_when_service_exists_but_is_offline() {
            when(repository.getLogicalInstance("oms_1")).thenReturn(
                    Map.of("logical_id", "oms_1", "instance_type", "OMS", "enabled", true));
            when(discoveryCache.getAll()).thenReturn(Map.of());

            assertThatThrownBy(() -> service.reloadService("OMS", "oms_1"))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("offline");
        }

        @Test
        void uses_stored_type_for_nats_subject() {
            when(repository.getLogicalInstance("gw_1")).thenReturn(
                    Map.of("logical_id", "gw_1", "instance_type", "GW", "enabled", true));

            var reg = ServiceRegistration.newBuilder()
                    .setServiceId("gw_1")
                    .setServiceType("GW")
                    .setEndpoint(TransportEndpoint.newBuilder().setAddress("localhost:8080"))
                    .build();
            when(discoveryCache.getAll()).thenReturn(Map.of("svc.gw.gw_1", reg));

            // Use lowercase in path — stored type should be used for subject
            var result = service.reloadService("gw", "gw_1");

            assertThat(result.subject()).isEqualTo("zk.control.GW.gw_1.reload");
            verify(natsConnection).publish(eq("zk.control.GW.gw_1.reload"), any(byte[].class));
        }

        @Test
        void returns_404_when_service_not_found() {
            when(repository.getLogicalInstance("nonexistent")).thenReturn(null);

            assertThatThrownBy(() -> service.reloadService("OMS", "nonexistent"))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not found");
        }
    }

    // ── List services ─────────────────────────────────────────────────────

    @Test
    void list_services_normalizes_kind_to_uppercase() {
        when(props.env()).thenReturn("test");
        when(repository.listLogicalInstances("test", "GW")).thenReturn(List.of());
        when(discoveryCache.getAll()).thenReturn(Map.of());

        service.listServices(null, "gw");

        verify(repository).listLogicalInstances("test", "GW");
    }

    // ── View name validation ──────────────────────────────────────────────

    @Test
    void get_view_rejects_unknown_view_name() {
        assertThatThrownBy(() -> service.getView("nonexistent", null))
                .isInstanceOf(ResponseStatusException.class)
                .hasMessageContaining("Unknown view");
    }

    @Test
    void get_view_accepts_known_view_names() {
        when(props.env()).thenReturn("test");
        when(repository.listLogicalInstances("test", null)).thenReturn(List.of());
        when(repository.listBindings(null, null)).thenReturn(List.of());
        when(discoveryCache.getAll()).thenReturn(Map.of());

        for (String name : List.of("full", "services", "connections")) {
            var view = service.getView(name, null);
            assertThat(view).isNotNull();
        }
    }

    // ── Create service ─────────────────────────────────────────────────────

    @Nested
    class CreateService {

        @Test
        void rejects_refdata_with_400() {
            var request = new CreateServiceRequest("refdata_1", true, Map.of(), null, null, null);

            assertThatThrownBy(() -> service.createService("REFDATA", request))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("refdata-venues");
        }

        @Test
        void rejects_unknown_kind() {
            var request = new CreateServiceRequest("foo_1", true, Map.of(), null, null, null);

            assertThatThrownBy(() -> service.createService("UNKNOWN", request))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("Unknown service kind");
        }

        @Test
        void creates_oms_with_provided_config() {
            when(props.env()).thenReturn("dev");
            when(repository.getLogicalInstance("oms_1")).thenReturn(null);
            when(tokenService.generateToken(eq("oms_1"), eq("OMS"), eq("dev"), eq(30)))
                    .thenReturn(new BootstrapTokenResponse("jti", "tok", null));

            var request = new CreateServiceRequest("oms_1", true,
                    Map.of("grpc_port", 50051), null, null, null);

            CreateServiceResponse resp = service.createService("OMS", request);

            assertThat(resp.status()).isEqualTo("created");
            assertThat(resp.logicalId()).isEqualTo("oms_1");

            // Verify logical instance created without config
            verify(repository).createLogicalInstance("oms_1", "OMS", "dev", null, true);
            // Verify OMS instance created with provided config JSON
            verify(repository).createOmsInstance(eq("oms_1"), contains("grpc_port"),
                    any(), any(), any(), any());
        }

        @Test
        void creates_mdgw_with_selected_venue_in_provided_config() {
            when(props.env()).thenReturn("dev");
            when(repository.getLogicalInstance("mdgw_okx_1")).thenReturn(null);
            when(tokenService.generateToken(eq("mdgw_okx_1"), eq("MDGW"), eq("dev"), eq(30)))
                    .thenReturn(new BootstrapTokenResponse("jti", "tok", null));

            var request = new CreateServiceRequest(
                    "mdgw_okx_1",
                    true,
                    Map.of("sub_lease_ttl_s", 90, "venue_config", Map.of("demo_mode", true)),
                    "okx",
                    null,
                    null
            );

            CreateServiceResponse resp = service.createService("MDGW", request);

            assertThat(resp.status()).isEqualTo("created");
            verify(repository).createLogicalInstance("mdgw_okx_1", "MDGW", "dev", null, true);
            verify(repository).createMdgwInstance(
                    eq("mdgw_okx_1"),
                    eq("okx"),
                    argThat(json -> json.contains("\"venue\":\"okx\"")
                            && json.contains("\"sub_lease_ttl_s\":90")
                            && json.contains("\"venue_config\":{\"demo_mode\":true}")),
                    any(), any(), any(), any(), any(), any(), any()
            );
        }

        @Test
        void rejects_duplicate_logical_id() {
            when(repository.getLogicalInstance("oms_1")).thenReturn(
                    Map.of("logical_id", "oms_1", "instance_type", "OMS"));

            var request = new CreateServiceRequest("oms_1", true, Map.of(), null, null, null);

            assertThatThrownBy(() -> service.createService("OMS", request))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("already exists");
        }
    }

    // ── Update service config ──────────────────────────────────────────────

    @Nested
    class UpdateServiceConfig {

        @Test
        void updates_oms_config_with_provenance() {
            when(repository.getLogicalInstance("oms_1")).thenReturn(
                    Map.of("logical_id", "oms_1", "instance_type", "OMS", "enabled", true));
            when(discoveryCache.getAll()).thenReturn(Map.of());

            var request = new UpdateServiceRequest(null, Map.of("grpc_port", 50051));
            service.updateService("OMS", "oms_1", request);

            verify(repository).updateOmsConfig(eq("oms_1"), contains("grpc_port"),
                    any(), any(), any(), any());
        }

        @Test
        void updates_gw_config_resolves_venue_from_existing_row() {
            when(repository.getLogicalInstance("gw_1")).thenReturn(
                    Map.of("logical_id", "gw_1", "instance_type", "GW", "enabled", true));
            when(repository.getGatewayInstance("gw_1")).thenReturn(
                    Map.of("gw_id", "gw_1", "venue", "okx"));
            when(discoveryCache.getAll()).thenReturn(Map.of());

            var request = new UpdateServiceRequest(null, Map.of("key", "value"));
            service.updateService("GW", "gw_1", request);

            verify(repository).updateGatewayConfig(eq("gw_1"), contains("key"),
                    any(), any(), any(), any(), any(), any(), any());
            // Verify venue was looked up from existing row, not from request body
            verify(repository).getGatewayInstance("gw_1");
        }

        @Test
        void rejects_refdata_with_400() {
            assertThatThrownBy(() -> service.updateService("REFDATA", "refdata_1",
                    new UpdateServiceRequest(null, Map.of())))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("refdata-venues");
        }

        @Test
        void returns_404_for_unknown_service() {
            when(repository.getLogicalInstance("nonexistent")).thenReturn(null);

            assertThatThrownBy(() -> service.updateService("OMS", "nonexistent",
                    new UpdateServiceRequest(null, Map.of())))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not found");
        }

        @Test
        void returns_404_for_kind_mismatch() {
            when(repository.getLogicalInstance("gw_1")).thenReturn(
                    Map.of("logical_id", "gw_1", "instance_type", "GW", "enabled", true));

            assertThatThrownBy(() -> service.updateService("OMS", "gw_1",
                    new UpdateServiceRequest(null, Map.of())))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not found");
        }

        @Test
        void rejects_online_service_with_409() {
            when(repository.getLogicalInstance("oms_1")).thenReturn(
                    Map.of("logical_id", "oms_1", "instance_type", "OMS", "enabled", true));
            var reg = ServiceRegistration.newBuilder()
                    .setServiceId("oms_1")
                    .setServiceType("OMS")
                    .setEndpoint(TransportEndpoint.newBuilder().setAddress("localhost:9090"))
                    .build();
            when(discoveryCache.getAll()).thenReturn(Map.of("svc.oms.oms_1", reg));

            assertThatThrownBy(() -> service.updateService("OMS", "oms_1",
                    new UpdateServiceRequest(null, Map.of("k", "v"))))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("online");
        }

        @Test
        void rejects_invalid_config_with_400() {
            when(repository.getLogicalInstance("oms_1")).thenReturn(
                    Map.of("logical_id", "oms_1", "instance_type", "OMS", "enabled", true));
            when(discoveryCache.getAll()).thenReturn(Map.of());
            when(configSchemaLocator.resolveConfigSchema("oms_1", "OMS")).thenReturn("""
                    {"type":"object","properties":{"port":{"type":"integer"}},"required":["port"]}
                    """);

            assertThatThrownBy(() -> service.updateService("OMS", "oms_1",
                    new UpdateServiceRequest(null, Map.of("wrong_field", "value"))))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("validation failed");
        }
    }

    // ── Update refdata venue config ──────────────────────────────────────

    @Nested
    class UpdateRefdataVenueConfig {

        @Test
        void updates_refdata_config_with_venue_provenance() {
            when(repository.getRefdataVenueInstance("refdata_okx_1")).thenReturn(
                    Map.of("logical_id", "refdata_okx_1", "venue", "okx",
                            "env", "dev", "enabled", true,
                            "config_version", 1, "config_hash", "abc"));
            when(discoveryCache.getAll()).thenReturn(Map.of());

            service.updateRefdataVenueConfig("refdata_okx_1", Map.of("key", "val"));

            verify(repository).updateRefdataVenueConfig(eq("refdata_okx_1"),
                    contains("key"), any(), any(), any());
        }

        @Test
        void returns_404_for_missing_instance() {
            when(repository.getRefdataVenueInstance("nonexistent")).thenReturn(null);

            assertThatThrownBy(() -> service.updateRefdataVenueConfig("nonexistent", Map.of()))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not found");
        }

        @Test
        void rejects_online_refdata_with_409() {
            when(repository.getRefdataVenueInstance("refdata_1")).thenReturn(
                    Map.of("logical_id", "refdata_1", "venue", "okx",
                            "env", "dev", "enabled", true,
                            "config_version", 1, "config_hash", "abc"));
            var reg = ServiceRegistration.newBuilder()
                    .setServiceId("refdata_1")
                    .setServiceType("REFDATA")
                    .setEndpoint(TransportEndpoint.newBuilder().setAddress("localhost:9090"))
                    .build();
            when(discoveryCache.getAll()).thenReturn(Map.of("svc.refdata.refdata_1", reg));

            assertThatThrownBy(() -> service.updateRefdataVenueConfig("refdata_1", Map.of("k", "v")))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("online");
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    private static Map<String, Object> inst(String logicalId, String type) {
        return Map.of("logical_id", logicalId, "instance_type", type, "enabled", true);
    }

    private static Map<String, Object> inst(String logicalId, String type, boolean enabled) {
        return Map.of("logical_id", logicalId, "instance_type", type, "enabled", enabled);
    }

    private static Map<String, Object> binding(String srcId, String dstId) {
        return Map.of("src_id", srcId, "dst_id", dstId, "src_type", "X", "dst_type", "Y",
                "enabled", true);
    }
}
