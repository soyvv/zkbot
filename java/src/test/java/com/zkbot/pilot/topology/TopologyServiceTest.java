package com.zkbot.pilot.topology;

import com.zkbot.pilot.bootstrap.TokenService;
import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.topology.dto.ServiceNode;
import com.zkbot.pilot.topology.dto.TopologyView;
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

    TopologyService service;

    @BeforeEach
    void setUp() {
        service = new TopologyService(repository, discoveryCache, tokenService, props, natsConnection);
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

            assertThat(result.get("subject")).isEqualTo("zk.control.GW.gw_1.reload");
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
