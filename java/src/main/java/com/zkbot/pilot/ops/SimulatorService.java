package com.zkbot.pilot.ops;

import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.grpc.GwSimAdminClient;
import com.zkbot.pilot.ops.dto.sim.*;
import com.zkbot.pilot.topology.TopologyRepository;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;
import zk.discovery.v1.Discovery.ServiceRegistration;
import zk.gateway.v1.GatewaySimulatorAdmin;
import zk.gateway.v1.GatewaySimulatorAdmin.BalanceMutation;
import zk.gateway.v1.GatewaySimulatorAdmin.ErrorEffectType;
import zk.gateway.v1.GatewaySimulatorAdmin.ErrorMatchCriteria;
import zk.gateway.v1.GatewaySimulatorAdmin.ErrorScope;
import zk.gateway.v1.GatewaySimulatorAdmin.FillMode;
import zk.gateway.v1.GatewaySimulatorAdmin.GetSimulatorStateResponse;
import zk.gateway.v1.GatewaySimulatorAdmin.InjectedErrorInfo;
import zk.gateway.v1.GatewaySimulatorAdmin.MutationMode;
import zk.gateway.v1.GatewaySimulatorAdmin.PositionMutation;
import zk.gateway.v1.GatewaySimulatorAdmin.SimOpenOrder;
import zk.gateway.v1.GatewaySimulatorAdmin.TriggerPolicy;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

@Service
public class SimulatorService {

    private static final Logger log = LoggerFactory.getLogger(SimulatorService.class);

    private final GwSimAdminClient adminClient;
    private final TopologyRepository topologyRepo;
    private final DiscoveryCache discoveryCache;

    public SimulatorService(GwSimAdminClient adminClient, TopologyRepository topologyRepo,
                            DiscoveryCache discoveryCache) {
        this.adminClient = adminClient;
        this.topologyRepo = topologyRepo;
        this.discoveryCache = discoveryCache;
    }

    public List<SimulatorSummary> listSimulators() {
        var gateways = topologyRepo.listGatewayInstances();
        List<SimulatorSummary> result = new ArrayList<>();
        for (var gw : gateways) {
            String venue = (String) gw.get("venue");
            if (!"SIM".equalsIgnoreCase(venue) && !"simulator".equalsIgnoreCase(venue)) {
                continue;
            }
            String gwId = (String) gw.get("gw_id");
            ServiceRegistration reg = discoveryCache.get("svc.gw." + gwId);
            boolean live = reg != null;
            String tradingAddr = live && reg.hasEndpoint() ? reg.getEndpoint().getAddress() : null;
            String adminAddr = null;
            Boolean adminEnabled = null;
            if (live) {
                String adminPort = reg.getAttrsMap().get("admin_grpc_port");
                if (adminPort != null) {
                    adminEnabled = true;
                    String host = tradingAddr != null && tradingAddr.contains(":")
                            ? tradingAddr.substring(0, tradingAddr.lastIndexOf(':')) : tradingAddr;
                    adminAddr = host + ":" + adminPort;
                }
            }
            result.add(new SimulatorSummary(gwId, venue, live, tradingAddr, adminAddr, adminEnabled));
        }
        return result;
    }

    public SimulatorStateResponse getSimulatorState(String gwId) {
        requireSimulator(gwId);
        try {
            var resp = adminClient.getSimulatorState(gwId);
            return toStateResponse(gwId, resp);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse forceMatch(String gwId, ForceMatchRequest req) {
        requireSimulator(gwId);
        try {
            var builder = GatewaySimulatorAdmin.ForceMatchRequest.newBuilder()
                    .setOrderId(req.orderId())
                    .setPublishBalanceUpdate(true)
                    .setPublishPositionUpdate(true);

            if ("FULL".equalsIgnoreCase(req.fillMode())) {
                builder.setFillMode(FillMode.FILL_MODE_FULL);
            } else if ("PARTIAL".equalsIgnoreCase(req.fillMode())) {
                builder.setFillMode(FillMode.FILL_MODE_PARTIAL);
            }
            if (req.fillQty() != null) builder.setFillQty(req.fillQty());
            if (req.fillPrice() != null) builder.setFillPrice(req.fillPrice());
            if (req.reason() != null) builder.setReason(req.reason());

            var resp = adminClient.forceMatch(gwId, builder.build());
            return new SimActionResponse(resp.getAccepted(), resp.getMessage(), null, null, null, null,
                    resp.getMatchedOrderId(), resp.getGeneratedTradeCount(), resp.getRemainingOpenQty(),
                    null, null, null, null, null, null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse injectError(String gwId, InjectErrorRequest req) {
        requireSimulator(gwId);
        try {
            var builder = GatewaySimulatorAdmin.InjectErrorRequest.newBuilder();
            if (req.errorId() != null) builder.setErrorId(req.errorId());
            if (req.scope() != null) builder.setScope(parseErrorScope(req.scope()));
            if (req.effect() != null) builder.setEffect(parseErrorEffect(req.effect()));
            if (req.grpcStatusCode() != null) builder.setGrpcStatusCode(req.grpcStatusCode());
            if (req.errorMessage() != null) builder.setErrorMessage(req.errorMessage());
            if (req.delayMs() != null) builder.setDelayMs(req.delayMs());
            if (req.duplicateCount() != null) builder.setDuplicateCount(req.duplicateCount());
            if (req.triggerPolicy() != null) builder.setTriggerPolicy(parseTriggerPolicy(req.triggerPolicy()));
            if (req.triggerTimes() != null) builder.setTriggerTimes(req.triggerTimes());
            if (req.priority() != null) builder.setPriority(req.priority());
            if (req.enabled() != null) builder.setEnabled(req.enabled());

            if (req.matchCriteria() != null) {
                var mc = ErrorMatchCriteria.newBuilder();
                if (req.matchCriteria().containsKey("account_id"))
                    mc.setAccountId(((Number) req.matchCriteria().get("account_id")).longValue());
                if (req.matchCriteria().containsKey("instrument"))
                    mc.setInstrument((String) req.matchCriteria().get("instrument"));
                if (req.matchCriteria().containsKey("order_id"))
                    mc.setOrderId(((Number) req.matchCriteria().get("order_id")).longValue());
                builder.setMatchCriteria(mc);
            }

            var resp = adminClient.injectError(gwId, builder.build());
            return new SimActionResponse(resp.getAccepted(), resp.getMessage(), resp.getErrorId(),
                    null, null, null, null, null, null, null, null, null, null, null, null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public List<InjectedErrorEntry> listInjectedErrors(String gwId) {
        requireSimulator(gwId);
        try {
            var resp = adminClient.listInjectedErrors(gwId);
            return resp.getErrorsList().stream().map(SimulatorService::toErrorEntry).toList();
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse clearInjectedError(String gwId, String errorId) {
        requireSimulator(gwId);
        try {
            var resp = adminClient.clearInjectedError(gwId, errorId != null ? errorId : "");
            return new SimActionResponse(resp.getAccepted(), resp.getMessage(), null,
                    resp.getClearedCount(), null, null, null, null, null, null, null, null, null,
                    null, null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse pauseMatching(String gwId, String reason) {
        requireSimulator(gwId);
        try {
            var resp = adminClient.pauseMatching(gwId, reason);
            return new SimActionResponse(true, resp.getMessage(), null, null, null, null,
                    null, null, null, null, null, null, null, resp.getMatchingPaused(), null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse resumeMatching(String gwId, String reason) {
        requireSimulator(gwId);
        try {
            var resp = adminClient.resumeMatching(gwId, reason);
            return new SimActionResponse(true, resp.getMessage(), null, null, null, null,
                    null, null, null, null, null, null, null, resp.getMatchingPaused(), null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse setMatchPolicy(String gwId, SetMatchPolicyRequest req) {
        requireSimulator(gwId);
        try {
            var builder = GatewaySimulatorAdmin.SetMatchPolicyRequest.newBuilder()
                    .setPolicyName(req.policyName());
            if (req.policyConfig() != null) builder.setPolicyConfig(req.policyConfig());
            if (req.reason() != null) builder.setReason(req.reason());

            var resp = adminClient.setMatchPolicy(gwId, builder.build());
            return new SimActionResponse(resp.getAccepted(), resp.getMessage(), null, null, null, null,
                    null, null, null, null, null, null, null, null,
                    resp.getPreviousPolicy(), resp.getCurrentPolicy());
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse setAccountState(String gwId, SetAccountStateRequest req) {
        requireSimulator(gwId);
        try {
            var builder = GatewaySimulatorAdmin.SetAccountStateRequest.newBuilder()
                    .setAccountId(req.accountId())
                    .setClearExistingBalances(req.clearExistingBalances())
                    .setClearExistingPositions(req.clearExistingPositions())
                    .setPublishUpdates(req.publishUpdates());
            if (req.reason() != null) builder.setReason(req.reason());

            if (req.balanceMutations() != null) {
                for (var bm : req.balanceMutations()) {
                    builder.addBalanceMutations(BalanceMutation.newBuilder()
                            .setAsset(bm.asset())
                            .setTotalQty(bm.totalQty())
                            .setAvailQty(bm.availQty())
                            .setFrozenQty(bm.frozenQty())
                            .setMode(parseMutationMode(bm.mode()))
                            .build());
                }
            }
            if (req.positionMutations() != null) {
                for (var pm : req.positionMutations()) {
                    builder.addPositionMutations(PositionMutation.newBuilder()
                            .setInstrumentCode(pm.instrumentCode())
                            .setLongShortType(pm.longShortType())
                            .setTotalQty(pm.totalQty())
                            .setAvailQty(pm.availQty())
                            .setFrozenQty(pm.frozenQty())
                            .setMode(parseMutationMode(pm.mode()))
                            .build());
                }
            }

            var resp = adminClient.setAccountState(gwId, builder.build());
            return new SimActionResponse(resp.getAccepted(), resp.getMessage(), null, null, null, null,
                    null, null, null, resp.getAppliedBalanceCount(), resp.getAppliedPositionCount(),
                    null, null, null, null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse resetSimulator(String gwId, String reason) {
        requireSimulator(gwId);
        try {
            var resp = adminClient.resetSimulator(gwId, reason);
            return new SimActionResponse(resp.getAccepted(), resp.getMessage(), null, null,
                    resp.getClearedOrders(), resp.getClearedErrorRules(),
                    null, null, null, null, null, null, null, null, null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public SimActionResponse submitSyntheticTick(String gwId, SubmitSyntheticTickRequest req) {
        requireSimulator(gwId);
        try {
            var tickBuilder = zk.rtmd.v1.Rtmd.TickData.newBuilder()
                    .setInstrumentCode(req.instrumentCode());
            if (req.tickType() != null) {
                tickBuilder.setTickType(switch (req.tickType().toUpperCase()) {
                    case "T", "TRADE" -> zk.rtmd.v1.Rtmd.TickUpdateType.T;
                    case "OB", "ORDERBOOK" -> zk.rtmd.v1.Rtmd.TickUpdateType.OB;
                    case "M", "MIXED" -> zk.rtmd.v1.Rtmd.TickUpdateType.M;
                    default -> zk.rtmd.v1.Rtmd.TickUpdateType.TICK_TYPE_UNSPECIFIED;
                });
            }
            if (req.exchange() != null) tickBuilder.setExchange(req.exchange());
            if (req.originalTimestamp() != null) tickBuilder.setOriginalTimestamp(req.originalTimestamp());
            if (req.volume() != null) tickBuilder.setVolume(req.volume());
            if (req.latestTradePrice() != null) tickBuilder.setLatestTradePrice(req.latestTradePrice());
            if (req.latestTradeQty() != null) tickBuilder.setLatestTradeQty(req.latestTradeQty());
            if (req.latestTradeSide() != null) tickBuilder.setLatestTradeSide(req.latestTradeSide());
            if (req.buyPriceLevels() != null) {
                for (var pl : req.buyPriceLevels()) {
                    tickBuilder.addBuyPriceLevels(zk.rtmd.v1.Rtmd.PriceLevel.newBuilder()
                            .setPrice(pl.price()).setQty(pl.qty()).build());
                }
            }
            if (req.sellPriceLevels() != null) {
                for (var pl : req.sellPriceLevels()) {
                    tickBuilder.addSellPriceLevels(zk.rtmd.v1.Rtmd.PriceLevel.newBuilder()
                            .setPrice(pl.price()).setQty(pl.qty()).build());
                }
            }

            var protoReq = GatewaySimulatorAdmin.SubmitSyntheticTickRequest.newBuilder()
                    .setTick(tickBuilder.build());
            if (req.sourceTag() != null) protoReq.setSourceTag(req.sourceTag());
            if (req.reason() != null) protoReq.setReason(req.reason());

            var resp = adminClient.submitSyntheticTick(gwId, protoReq.build());
            return new SimActionResponse(resp.getAccepted(), resp.getMessage(), null, null, null, null,
                    null, null, null, null, null, resp.getAffectedOrderCount(),
                    resp.getGeneratedReportCount(), null, null, null);
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    public List<SimOpenOrderEntry> listOpenOrders(String gwId) {
        requireSimulator(gwId);
        try {
            var resp = adminClient.listOpenOrders(gwId);
            return resp.getOrdersList().stream().map(SimulatorService::toOpenOrderEntry).toList();
        } catch (io.grpc.StatusRuntimeException e) {
            throw mapGrpcError(gwId, e);
        }
    }

    // --- helpers ---

    private void requireSimulator(String gwId) {
        var gw = topologyRepo.getGatewayInstance(gwId);
        if (gw == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, "Gateway not found: " + gwId);
        }
        String venue = (String) gw.get("venue");
        if (!"SIM".equalsIgnoreCase(venue) && !"simulator".equalsIgnoreCase(venue)) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "Gateway " + gwId + " is not a simulator (venue=" + venue + ")");
        }
        if (discoveryCache.get("svc.gw." + gwId) == null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "Simulator gateway is offline: " + gwId);
        }
    }

    private ResponseStatusException mapGrpcError(String gwId, io.grpc.StatusRuntimeException e) {
        log.error("gRPC error calling simulator admin for {}: {}", gwId, e.getStatus(), e);
        return new ResponseStatusException(HttpStatus.BAD_GATEWAY,
                "Simulator admin call failed for " + gwId + ": " + e.getStatus().getDescription());
    }

    // --- proto to DTO mappers ---

    private static SimulatorStateResponse toStateResponse(String gwId, GetSimulatorStateResponse resp) {
        return new SimulatorStateResponse(
                gwId,
                resp.getOpenOrdersList().stream().map(SimulatorService::toOpenOrderEntry).toList(),
                resp.getBalancesList().stream().map(b ->
                        new com.zkbot.pilot.ops.dto.sim.SimBalanceEntry(
                                b.getAsset(), b.getTotalQty(), b.getAvailQty(), b.getFrozenQty())).toList(),
                resp.getPositionsList().stream().map(p ->
                        new com.zkbot.pilot.ops.dto.sim.SimPositionEntry(
                                p.getInstrumentCode(), p.getLongShortType(),
                                p.getTotalQty(), p.getAvailQty(), p.getFrozenQty())).toList(),
                resp.getActiveErrorRulesList().stream().map(SimulatorService::toErrorEntry).toList(),
                resp.getCurrentMatchPolicy(),
                resp.getMatchingPaused(),
                resp.getLastTickTs() != 0 ? resp.getLastTickTs() : null,
                resp.getLastEventTs() != 0 ? resp.getLastEventTs() : null
        );
    }

    private static SimOpenOrderEntry toOpenOrderEntry(SimOpenOrder o) {
        return new SimOpenOrderEntry(o.getOrderId(), o.getExchOrderRef(), o.getAccountId(),
                o.getInstrument(), o.getSide(), o.getRemainingQty(), o.getFilledQty(),
                o.getLimitPrice(), o.getCreatedTs(), o.getUpdatedTs());
    }

    private static InjectedErrorEntry toErrorEntry(InjectedErrorInfo e) {
        return new InjectedErrorEntry(e.getErrorId(), e.getScope().name(), e.getEffect().name(),
                e.getTriggerPolicy().name(), e.getRemainingTriggers(), e.getPriority(),
                e.getEnabled(), e.getCreatedAt(), e.getLastFiredAt());
    }

    private static ErrorScope parseErrorScope(String scope) {
        return switch (scope.toUpperCase()) {
            case "PLACE_ORDER" -> ErrorScope.ERROR_SCOPE_PLACE_ORDER;
            case "CANCEL_ORDER" -> ErrorScope.ERROR_SCOPE_CANCEL_ORDER;
            case "QUERY_ACCOUNT_BALANCE" -> ErrorScope.ERROR_SCOPE_QUERY_ACCOUNT_BALANCE;
            case "QUERY_ORDER_DETAIL" -> ErrorScope.ERROR_SCOPE_QUERY_ORDER_DETAIL;
            default -> ErrorScope.ERROR_SCOPE_UNSPECIFIED;
        };
    }

    private static ErrorEffectType parseErrorEffect(String effect) {
        return switch (effect.toUpperCase()) {
            case "RPC_ERROR" -> ErrorEffectType.ERROR_EFFECT_RPC_ERROR;
            case "REJECT_ORDER" -> ErrorEffectType.ERROR_EFFECT_REJECT_ORDER;
            case "DELAY_RESPONSE" -> ErrorEffectType.ERROR_EFFECT_DELAY_RESPONSE;
            case "DROP_REPORT" -> ErrorEffectType.ERROR_EFFECT_DROP_REPORT;
            case "DUPLICATE_REPORT" -> ErrorEffectType.ERROR_EFFECT_DUPLICATE_REPORT;
            case "DISCONNECT" -> ErrorEffectType.ERROR_EFFECT_DISCONNECT;
            default -> ErrorEffectType.ERROR_EFFECT_UNSPECIFIED;
        };
    }

    private static TriggerPolicy parseTriggerPolicy(String policy) {
        return switch (policy.toUpperCase()) {
            case "ONCE" -> TriggerPolicy.TRIGGER_POLICY_ONCE;
            case "TIMES" -> TriggerPolicy.TRIGGER_POLICY_TIMES;
            case "UNTIL_CLEARED" -> TriggerPolicy.TRIGGER_POLICY_UNTIL_CLEARED;
            default -> TriggerPolicy.TRIGGER_POLICY_UNSPECIFIED;
        };
    }

    private static MutationMode parseMutationMode(String mode) {
        if (mode == null) return MutationMode.MUTATION_MODE_UPSERT;
        return switch (mode.toUpperCase()) {
            case "REMOVE" -> MutationMode.MUTATION_MODE_REMOVE;
            default -> MutationMode.MUTATION_MODE_UPSERT;
        };
    }
}
