package com.zkbot.pilot.account;

import com.zkbot.pilot.account.dto.CreateAccountRequest;
import com.zkbot.pilot.account.dto.UpdateAccountRequest;
import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.grpc.OmsGrpcClient;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.data.redis.core.StringRedisTemplate;
import org.springframework.stereotype.Service;
import zk.discovery.v1.Discovery.ServiceRegistration;
import zk.oms.v1.Oms;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

@Service
public class AccountService {

    private static final Logger log = LoggerFactory.getLogger(AccountService.class);

    private final AccountRepository repository;
    private final OmsGrpcClient omsClient;
    private final DiscoveryCache discoveryCache;
    private final StringRedisTemplate redisTemplate;

    public AccountService(AccountRepository repository, OmsGrpcClient omsClient,
                           DiscoveryCache discoveryCache, StringRedisTemplate redisTemplate) {
        this.repository = repository;
        this.omsClient = omsClient;
        this.discoveryCache = discoveryCache;
        this.redisTemplate = redisTemplate;
    }

    public Map<String, Object> createAccount(CreateAccountRequest req) {
        repository.createAccount(req.accountId(), req.exchAccountId(), req.venue(),
                req.brokerType(), req.accountType(), req.baseCurrency());
        return Map.of("status", "created", "account_id", req.accountId());
    }

    public List<Map<String, Object>> listAccounts(String venue, String omsId, String status, int limit) {
        return repository.listAccounts(venue, omsId, status, limit);
    }

    public Map<String, Object> getAccount(long accountId) {
        Map<String, Object> account = repository.getAccount(accountId);
        if (account == null) {
            return Map.of("error", "account_not_found", "account_id", accountId);
        }
        Map<String, Object> result = new LinkedHashMap<>(account);
        Map<String, Object> binding = repository.getAccountBinding(accountId);
        if (binding != null) {
            result.put("binding", binding);
        }
        return result;
    }

    public Map<String, Object> updateAccount(long accountId, UpdateAccountRequest req) {
        repository.updateAccount(accountId, req.status());
        return Map.of("status", "updated", "account_id", accountId);
    }

    public List<Map<String, Object>> getBalances(long accountId) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return List.of(Map.of("error", "no_oms_binding", "account_id", accountId));
        }

        Set<String> keys = redisTemplate.keys("oms:" + omsId + ":balance:" + accountId + ":*");
        if (keys == null || keys.isEmpty()) {
            return List.of();
        }

        List<Map<String, Object>> balances = new ArrayList<>();
        for (String key : keys) {
            String value = redisTemplate.opsForValue().get(key);
            if (value == null) continue;

            // Key format: oms:<oms_id>:balance:<account_id>:<asset>
            String[] parts = key.split(":");
            String asset = parts.length >= 5 ? parts[4] : "unknown";
            Map<String, Object> entry = new LinkedHashMap<>();
            entry.put("asset", asset);
            entry.put("raw", value);
            balances.add(entry);
        }
        return balances;
    }

    public List<Map<String, Object>> getPositions(long accountId) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return List.of(Map.of("error", "no_oms_binding", "account_id", accountId));
        }

        Set<String> keys = redisTemplate.keys("oms:" + omsId + ":position:" + accountId + ":*");
        if (keys == null || keys.isEmpty()) {
            return List.of();
        }

        List<Map<String, Object>> positions = new ArrayList<>();
        for (String key : keys) {
            String value = redisTemplate.opsForValue().get(key);
            if (value == null) continue;

            // Key format: oms:<oms_id>:position:<account_id>:<instrument>:<side>
            String[] parts = key.split(":");
            Map<String, Object> entry = new LinkedHashMap<>();
            entry.put("instrument", parts.length >= 5 ? parts[4] : "unknown");
            entry.put("side", parts.length >= 6 ? parts[5] : "unknown");
            entry.put("raw", value);
            positions.add(entry);
        }
        return positions;
    }

    public List<Map<String, Object>> getOpenOrders(long accountId) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return List.of(Map.of("error", "no_oms_binding", "account_id", accountId));
        }

        Oms.QueryOpenOrderRequest req = Oms.QueryOpenOrderRequest.newBuilder()
                .setAccountId(accountId)
                .build();
        Oms.OrderDetailResponse resp = omsClient.queryOpenOrders(omsId, req);

        List<Map<String, Object>> orders = new ArrayList<>();
        for (Oms.Order o : resp.getOrdersList()) {
            Map<String, Object> entry = new LinkedHashMap<>();
            entry.put("order_id", o.getOrderId());
            entry.put("exch_order_ref", o.getExchOrderRef());
            entry.put("instrument", o.getInstrument());
            entry.put("side", o.getBuySellType().name());
            entry.put("price", o.getPrice());
            entry.put("qty", o.getQty());
            entry.put("filled_qty", o.getFilledQty());
            entry.put("filled_avg_price", o.getFilledAvgPrice());
            orders.add(entry);
        }
        return orders;
    }

    public List<Map<String, Object>> getTrades(long accountId, int limit) {
        return repository.listTradesForAccount(accountId, limit);
    }

    public List<Map<String, Object>> getActivities(long accountId, int limit) {
        return repository.listActivitiesForAccount(accountId, limit);
    }

    public Map<String, Object> getRuntimeBinding(long accountId) {
        Map<String, Object> binding = repository.getAccountBinding(accountId);
        if (binding == null) {
            return Map.of("error", "no_binding", "account_id", accountId);
        }

        Map<String, Object> result = new LinkedHashMap<>(binding);

        Object omsId = binding.get("oms_id");
        if (omsId != null) {
            String omsKey = "svc.oms." + omsId;
            ServiceRegistration reg = discoveryCache.get(omsKey);
            result.put("oms_live", reg != null);
            if (reg != null && reg.hasEndpoint()) {
                result.put("oms_address", reg.getEndpoint().getAddress());
            }
        }

        Object gwId = binding.get("gw_id");
        if (gwId != null) {
            String gwKey = "svc.gw." + gwId;
            ServiceRegistration reg = discoveryCache.get(gwKey);
            result.put("gw_live", reg != null);
            if (reg != null && reg.hasEndpoint()) {
                result.put("gw_address", reg.getEndpoint().getAddress());
            }
        }

        return result;
    }

    public Map<String, Object> cancelOrders(long accountId, List<Long> orderIds) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return Map.of("error", "no_oms_binding", "account_id", accountId);
        }

        List<Map<String, Object>> results = new ArrayList<>();
        for (long orderId : orderIds) {
            try {
                Oms.CancelOrderRequest req = Oms.CancelOrderRequest.newBuilder()
                        .setOrderCancelRequest(Oms.OrderCancelRequest.newBuilder()
                                .setOrderId(orderId)
                                .build())
                        .build();
                Oms.OMSResponse resp = omsClient.cancelOrder(omsId, req);
                results.add(Map.of(
                        "order_id", orderId,
                        "status", resp.getStatus().name(),
                        "message", resp.getMessage()));
            } catch (Exception e) {
                log.error("Failed to cancel order {} for account {}", orderId, accountId, e);
                results.add(Map.of(
                        "order_id", orderId,
                        "status", "ERROR",
                        "message", e.getMessage()));
            }
        }

        return Map.of("account_id", accountId, "results", results);
    }

    private String resolveOmsId(long accountId) {
        Map<String, Object> binding = repository.getAccountBinding(accountId);
        if (binding == null) {
            return null;
        }
        Object omsId = binding.get("oms_id");
        return omsId != null ? omsId.toString() : null;
    }
}
