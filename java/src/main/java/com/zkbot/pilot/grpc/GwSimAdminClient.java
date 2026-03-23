package com.zkbot.pilot.grpc;

import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.discovery.DiscoveryResolver;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.stereotype.Component;
import zk.discovery.v1.Discovery.ServiceRegistration;
import zk.gateway.v1.GatewaySimulatorAdmin;
import zk.gateway.v1.GatewaySimulatorAdminServiceGrpc;
import zk.gateway.v1.GatewaySimulatorAdminServiceGrpc.GatewaySimulatorAdminServiceBlockingStub;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

/**
 * gRPC client for the gateway simulator admin service.
 * Resolves the admin address from discovery attrs (admin_grpc_port) or falls back to trading port + 1.
 */
@Component
public class GwSimAdminClient implements DisposableBean {

    private static final Logger log = LoggerFactory.getLogger(GwSimAdminClient.class);

    private final DiscoveryResolver discoveryResolver;
    private final DiscoveryCache discoveryCache;
    private final ConcurrentHashMap<String, ManagedChannel> channelCache = new ConcurrentHashMap<>();
    private final ConcurrentHashMap<String, String> addressCache = new ConcurrentHashMap<>();

    public GwSimAdminClient(DiscoveryResolver discoveryResolver, DiscoveryCache discoveryCache) {
        this.discoveryResolver = discoveryResolver;
        this.discoveryCache = discoveryCache;
    }

    public GatewaySimulatorAdminServiceBlockingStub getStub(String gwId) {
        String adminAddress = resolveAdminAddress(gwId);
        if (adminAddress == null) {
            throw new IllegalStateException("Cannot resolve admin address for simulator: " + gwId);
        }

        String cachedAddress = addressCache.get(gwId);
        if (cachedAddress != null && !cachedAddress.equals(adminAddress)) {
            log.info("Simulator admin {} address changed from {} to {}, rebuilding channel",
                    gwId, cachedAddress, adminAddress);
            ManagedChannel old = channelCache.remove(gwId);
            addressCache.remove(gwId);
            if (old != null) {
                old.shutdown();
            }
        }

        ManagedChannel channel = channelCache.computeIfAbsent(gwId, id -> {
            log.info("Creating gRPC channel to simulator admin {} at {}", id, adminAddress);
            addressCache.put(id, adminAddress);
            return ManagedChannelBuilder.forTarget(adminAddress)
                    .usePlaintext()
                    .build();
        });
        return GatewaySimulatorAdminServiceGrpc.newBlockingStub(channel);
    }

    private String resolveAdminAddress(String gwId) {
        ServiceRegistration reg = discoveryCache.get("svc.gw." + gwId);
        if (reg == null || !reg.hasEndpoint()) {
            return null;
        }
        String tradingAddress = reg.getEndpoint().getAddress();
        if (tradingAddress == null || tradingAddress.isEmpty()) {
            return null;
        }

        // Try attrs["admin_grpc_port"] first
        String adminPort = reg.getAttrsMap().get("admin_grpc_port");
        if (adminPort != null && !adminPort.isEmpty()) {
            String host = tradingAddress.contains(":") ? tradingAddress.substring(0, tradingAddress.lastIndexOf(':')) : tradingAddress;
            return host + ":" + adminPort;
        }

        // Fallback: trading port + 1
        int colonIdx = tradingAddress.lastIndexOf(':');
        if (colonIdx > 0) {
            try {
                int tradingPort = Integer.parseInt(tradingAddress.substring(colonIdx + 1));
                return tradingAddress.substring(0, colonIdx) + ":" + (tradingPort + 1);
            } catch (NumberFormatException e) {
                return null;
            }
        }
        return null;
    }

    // --- RPC wrappers ---

    public GatewaySimulatorAdmin.ForceMatchResponse forceMatch(
            String gwId, GatewaySimulatorAdmin.ForceMatchRequest request) {
        return getStub(gwId).forceMatch(request);
    }

    public GatewaySimulatorAdmin.InjectErrorResponse injectError(
            String gwId, GatewaySimulatorAdmin.InjectErrorRequest request) {
        return getStub(gwId).injectError(request);
    }

    public GatewaySimulatorAdmin.ListInjectedErrorsResponse listInjectedErrors(String gwId) {
        return getStub(gwId).listInjectedErrors(
                GatewaySimulatorAdmin.ListInjectedErrorsRequest.getDefaultInstance());
    }

    public GatewaySimulatorAdmin.ClearInjectedErrorResponse clearInjectedError(
            String gwId, String errorId) {
        return getStub(gwId).clearInjectedError(
                GatewaySimulatorAdmin.ClearInjectedErrorRequest.newBuilder()
                        .setErrorId(errorId).build());
    }

    public GatewaySimulatorAdmin.ResetSimulatorResponse resetSimulator(String gwId, String reason) {
        return getStub(gwId).resetSimulator(
                GatewaySimulatorAdmin.ResetSimulatorRequest.newBuilder()
                        .setReason(reason != null ? reason : "").build());
    }

    public GatewaySimulatorAdmin.SetAccountStateResponse setAccountState(
            String gwId, GatewaySimulatorAdmin.SetAccountStateRequest request) {
        return getStub(gwId).setAccountState(request);
    }

    public GatewaySimulatorAdmin.SubmitSyntheticTickResponse submitSyntheticTick(
            String gwId, GatewaySimulatorAdmin.SubmitSyntheticTickRequest request) {
        return getStub(gwId).submitSyntheticTick(request);
    }

    public GatewaySimulatorAdmin.ListOpenOrdersResponse listOpenOrders(String gwId) {
        return getStub(gwId).listOpenOrders(
                GatewaySimulatorAdmin.ListOpenOrdersRequest.getDefaultInstance());
    }

    public GatewaySimulatorAdmin.MatchingStateResponse pauseMatching(String gwId, String reason) {
        return getStub(gwId).pauseMatching(
                GatewaySimulatorAdmin.PauseMatchingRequest.newBuilder()
                        .setReason(reason != null ? reason : "").build());
    }

    public GatewaySimulatorAdmin.MatchingStateResponse resumeMatching(String gwId, String reason) {
        return getStub(gwId).resumeMatching(
                GatewaySimulatorAdmin.ResumeMatchingRequest.newBuilder()
                        .setReason(reason != null ? reason : "").build());
    }

    public GatewaySimulatorAdmin.SetMatchPolicyResponse setMatchPolicy(
            String gwId, GatewaySimulatorAdmin.SetMatchPolicyRequest request) {
        return getStub(gwId).setMatchPolicy(request);
    }

    public GatewaySimulatorAdmin.GetSimulatorStateResponse getSimulatorState(String gwId) {
        return getStub(gwId).getSimulatorState(
                GatewaySimulatorAdmin.GetSimulatorStateRequest.getDefaultInstance());
    }

    @Override
    public void destroy() {
        log.info("Shutting down {} simulator admin gRPC channels", channelCache.size());
        channelCache.forEach((id, channel) -> {
            try {
                channel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                log.warn("Interrupted while shutting down admin channel for {}", id);
                channel.shutdownNow();
                Thread.currentThread().interrupt();
            }
        });
        channelCache.clear();
    }
}
