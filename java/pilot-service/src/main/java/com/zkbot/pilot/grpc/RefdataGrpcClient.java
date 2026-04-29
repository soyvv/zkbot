package com.zkbot.pilot.grpc;

import com.zkbot.pilot.discovery.DiscoveryResolver;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.stereotype.Component;
import zk.refdata.v1.Refdata;
import zk.refdata.v1.RefdataServiceGrpc;
import zk.refdata.v1.RefdataServiceGrpc.RefdataServiceBlockingStub;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

/**
 * Discovery-aware gRPC client for the Refdata service.
 *
 * <p>Refdata-svc registers itself in NATS KV under {@code svc.refdata.<logical_id>}.
 * Pilot reads the logical_id from configuration / discovery and proxies the
 * operator-triggered refresh endpoints (used by the Refdata page).
 */
@Component
public class RefdataGrpcClient implements DisposableBean {

    private static final Logger log = LoggerFactory.getLogger(RefdataGrpcClient.class);

    private static final long TRIGGER_DEADLINE_SECONDS = 5;
    private static final long GET_DEADLINE_SECONDS = 5;

    private final DiscoveryResolver discoveryResolver;
    private final ConcurrentHashMap<String, ManagedChannel> channelCache = new ConcurrentHashMap<>();
    private final ConcurrentHashMap<String, String> addressCache = new ConcurrentHashMap<>();

    public RefdataGrpcClient(DiscoveryResolver discoveryResolver) {
        this.discoveryResolver = discoveryResolver;
    }

    private RefdataServiceBlockingStub getStub(String logicalId, long deadlineSeconds) {
        String currentAddress = discoveryResolver.findRefdata(logicalId);
        if (currentAddress == null) {
            throw new IllegalStateException("refdata service not found in discovery: " + logicalId);
        }

        String cachedAddress = addressCache.get(logicalId);
        if (cachedAddress != null && !cachedAddress.equals(currentAddress)) {
            log.info("refdata {} address changed from {} to {}, rebuilding channel",
                    logicalId, cachedAddress, currentAddress);
            ManagedChannel old = channelCache.remove(logicalId);
            addressCache.remove(logicalId);
            if (old != null) {
                old.shutdown();
            }
        }

        ManagedChannel channel = channelCache.computeIfAbsent(logicalId, id -> {
            log.info("Creating gRPC channel to refdata {} at {}", id, currentAddress);
            addressCache.put(id, currentAddress);
            return ManagedChannelBuilder.forTarget(currentAddress)
                    .usePlaintext()
                    .build();
        });
        return RefdataServiceGrpc.newBlockingStub(channel)
                .withDeadlineAfter(deadlineSeconds, TimeUnit.SECONDS);
    }

    public Refdata.RefreshRunResponse triggerVenueRefresh(String logicalId, String venue) {
        Refdata.TriggerVenueRefreshRequest req =
                Refdata.TriggerVenueRefreshRequest.newBuilder().setVenue(venue).build();
        return getStub(logicalId, TRIGGER_DEADLINE_SECONDS).triggerVenueRefresh(req);
    }

    public Refdata.RefreshRunResponse getRefreshRun(String logicalId, long runId) {
        Refdata.GetRefreshRunRequest req =
                Refdata.GetRefreshRunRequest.newBuilder().setRunId(runId).build();
        return getStub(logicalId, GET_DEADLINE_SECONDS).getRefreshRun(req);
    }

    @Override
    public void destroy() {
        for (ManagedChannel ch : channelCache.values()) {
            ch.shutdown();
        }
    }
}
