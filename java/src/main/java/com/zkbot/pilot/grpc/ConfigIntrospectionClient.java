package com.zkbot.pilot.grpc;

import com.zkbot.pilot.discovery.DiscoveryCache;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import io.grpc.StatusRuntimeException;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Component;
import zk.config.v1.Config.GetCurrentConfigRequest;
import zk.config.v1.Config.GetCurrentConfigResponse;
import zk.config.v1.ConfigIntrospectionServiceGrpc;
import zk.discovery.v1.Discovery.ServiceRegistration;

import java.util.concurrent.TimeUnit;

/**
 * Generic gRPC client for calling ConfigIntrospectionService.GetCurrentConfig
 * on any live service discovered via the service registry.
 */
@Component
public class ConfigIntrospectionClient {

    private static final Logger log = LoggerFactory.getLogger(ConfigIntrospectionClient.class);

    private final DiscoveryCache discoveryCache;

    public ConfigIntrospectionClient(DiscoveryCache discoveryCache) {
        this.discoveryCache = discoveryCache;
    }

    /**
     * Call GetCurrentConfig on a live service.
     * Returns null if service is offline or doesn't support the introspection API.
     */
    public GetCurrentConfigResponse getCurrentConfig(String logicalId) {
        ServiceRegistration reg = findRegistration(logicalId);
        if (reg == null || !reg.hasEndpoint()) {
            log.debug("introspection: service {} not found or no endpoint", logicalId);
            return null;
        }

        String address = reg.getEndpoint().getAddress();
        ManagedChannel channel = ManagedChannelBuilder.forTarget(address)
                .usePlaintext()
                .build();
        try {
            var stub = ConfigIntrospectionServiceGrpc.newBlockingStub(channel)
                    .withDeadlineAfter(5, TimeUnit.SECONDS);
            return stub.getCurrentConfig(
                    GetCurrentConfigRequest.newBuilder().setIncludeDefaults(true).build());
        } catch (StatusRuntimeException e) {
            if (e.getStatus().getCode() == io.grpc.Status.Code.UNIMPLEMENTED) {
                log.debug("introspection: service {} does not implement GetCurrentConfig", logicalId);
            } else {
                log.warn("introspection: failed to call GetCurrentConfig on {}: {}", logicalId, e.getMessage());
            }
            return null;
        } finally {
            channel.shutdownNow();
            try {
                channel.awaitTermination(1, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
    }

    private ServiceRegistration findRegistration(String logicalId) {
        var liveState = discoveryCache.getAll();
        for (var entry : liveState.entrySet()) {
            if (entry.getKey().endsWith("." + logicalId)) {
                return entry.getValue();
            }
            if (logicalId.equals(entry.getValue().getServiceId())) {
                return entry.getValue();
            }
        }
        return null;
    }
}
