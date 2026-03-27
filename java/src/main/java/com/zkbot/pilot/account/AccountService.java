package com.zkbot.pilot.account;

import com.zkbot.pilot.account.dto.*;
import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.grpc.OmsGrpcClient;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;
import zk.discovery.v1.Discovery.ServiceRegistration;
import zk.oms.v1.Oms;

import java.math.BigDecimal;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;

@Service
public class AccountService {

    private static final Logger log = LoggerFactory.getLogger(AccountService.class);

    private final AccountRepository repository;
    private final OmsGrpcClient omsClient;
    private final DiscoveryCache discoveryCache;

    public AccountService(AccountRepository repository, OmsGrpcClient omsClient,
                           DiscoveryCache discoveryCache) {
        this.repository = repository;
        this.omsClient = omsClient;
        this.discoveryCache = discoveryCache;
    }

    public AccountSummaryResponse createAccount(CreateAccountRequest req) {
        repository.createAccount(req.accountId(), req.exchAccountId(), req.venue(),
                req.brokerType(), req.accountType(), req.baseCurrency());
        return new AccountSummaryResponse(req.accountId(), req.exchAccountId(), req.venue(),
                "ACTIVE", req.brokerType(), req.accountType(), req.baseCurrency(), null, null);
    }

    public List<AccountSummaryResponse> listAccounts(String venue, String omsId, String status,
                                                      int limit, boolean includeSummary,
                                                      boolean includeBinding) {
        var rows = repository.listAccounts(venue, omsId, status, limit);
        return rows.stream()
                .map(row -> {
                    long accountId = ((Number) row.get("account_id")).longValue();
                    AccountSummaryStats stats = includeSummary ? computeSummaryStats(accountId) : null;
                    AccountBindingResponse bindingDto = includeBinding
                            ? Optional.ofNullable(repository.getAccountBinding(accountId))
                                      .map(this::toBindingResponse)
                                      .orElse(null)
                            : null;
                    return toAccountSummary(row, bindingDto, stats);
                })
                .toList();
    }

    public AccountSummaryResponse getAccount(long accountId) {
        Map<String, Object> account = repository.getAccount(accountId);
        if (account == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, "Account not found: " + accountId);
        }
        Map<String, Object> binding = repository.getAccountBinding(accountId);
        AccountBindingResponse bindingDto = binding != null ? toBindingResponse(binding) : null;
        return toAccountSummary(account, bindingDto, null);
    }

    public AccountSummaryResponse updateAccount(long accountId, UpdateAccountRequest req) {
        Map<String, Object> account = repository.getAccount(accountId);
        if (account == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, "Account not found: " + accountId);
        }
        repository.updateAccount(accountId, req.status());

        if (req.omsId() != null && req.gwId() != null) {
            repository.deleteAccountBindings(accountId);
            repository.upsertAccountBinding(accountId, req.omsId(), req.gwId());
        }

        Map<String, Object> binding = repository.getAccountBinding(accountId);
        AccountBindingResponse bindingDto = binding != null ? toBindingResponse(binding) : null;
        // re-read account in case status changed
        account = repository.getAccount(accountId);
        return toAccountSummary(account, bindingDto, null);
    }

    public List<BalanceEntry> getBalances(long accountId) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return List.of();
        }

        Oms.QueryBalancesRequest req = Oms.QueryBalancesRequest.newBuilder()
                .setAccountId(accountId)
                .build();
        Oms.QueryBalancesResponse resp = omsClient.queryBalances(omsId, req);

        return resp.getBalancesList().stream()
                .map(b -> new BalanceEntry(b.getAsset(), b.getTotalQty(), b.getFrozenQty(), b.getAvailQty()))
                .toList();
    }

    public List<PositionEntry> getPositions(long accountId) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return List.of();
        }

        Oms.QueryPositionRequest req = Oms.QueryPositionRequest.newBuilder()
                .setAccountId(accountId)
                .setQueryGw(true)
                .build();
        Oms.PositionResponse resp = omsClient.queryPosition(omsId, req);

        return resp.getPositionsList().stream()
                .map(p -> new PositionEntry(
                        p.getInstrumentCode(),
                        shortLongShort(p.getLongShortType()),
                        p.getTotalQty(), p.getFrozenQty(), p.getAvailQty()))
                .toList();
    }

    public List<OpenOrderEntry> getOpenOrders(long accountId) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return List.of();
        }

        Oms.QueryOpenOrderRequest req = Oms.QueryOpenOrderRequest.newBuilder()
                .setAccountId(accountId)
                .build();
        Oms.OrderDetailResponse resp = omsClient.queryOpenOrders(omsId, req);

        List<OpenOrderEntry> orders = new ArrayList<>();
        for (Oms.Order o : resp.getOrdersList()) {
            orders.add(new OpenOrderEntry(
                    o.getOrderId(), o.getExchOrderRef(), o.getInstrument(),
                    shortSide(o.getBuySellType()), shortOrderStatus(o.getOrderStatus()),
                    o.getPrice(), o.getQty(),
                    o.getFilledQty(), o.getFilledAvgPrice(),
                    o.getCreatedAt(), o.getUpdatedAt()));
        }
        return orders;
    }

    public List<TradeEntry> getTrades(long accountId, int limit) {
        return repository.listTradesForAccount(accountId, limit).stream()
                .map(AccountService::toTradeEntry)
                .toList();
    }

    public List<ActivityEntry> getActivities(long accountId, int limit) {
        return repository.listActivitiesForAccount(accountId, limit).stream()
                .map(AccountService::toActivityEntry)
                .toList();
    }

    public AccountBindingResponse getRuntimeBinding(long accountId) {
        Map<String, Object> binding = repository.getAccountBinding(accountId);
        if (binding == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, "No binding for account: " + accountId);
        }
        return toBindingResponse(binding);
    }

    public CancelOrdersResponse cancelOrders(long accountId, List<Long> orderIds) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, "No OMS binding for account: " + accountId);
        }

        List<CancelOrderResult> results = new ArrayList<>();
        for (long orderId : orderIds) {
            try {
                Oms.CancelOrderRequest req = Oms.CancelOrderRequest.newBuilder()
                        .setOrderCancelRequest(Oms.OrderCancelRequest.newBuilder()
                                .setOrderId(orderId)
                                .build())
                        .build();
                Oms.OMSResponse resp = omsClient.cancelOrder(omsId, req);
                results.add(new CancelOrderResult(orderId, resp.getStatus().name(), resp.getMessage()));
            } catch (Exception e) {
                log.error("Failed to cancel order {} for account {}", orderId, accountId, e);
                results.add(new CancelOrderResult(orderId, "ERROR", e.getMessage()));
            }
        }

        return new CancelOrdersResponse(accountId, results);
    }

    // --- summary stats (gRPC to OMS) ---

    private AccountSummaryStats computeSummaryStats(long accountId) {
        String omsId = resolveOmsId(accountId);
        if (omsId == null) {
            return new AccountSummaryStats(0, 0, "unknown");
        }

        int positionCount = 0;
        int openOrderCount = 0;
        try {
            Oms.QueryPositionRequest posReq = Oms.QueryPositionRequest.newBuilder()
                    .setAccountId(accountId)
                    .setQueryGw(true)
                    .build();
            positionCount = omsClient.queryPosition(omsId, posReq).getPositionsCount();
        } catch (Exception e) {
            log.warn("Failed to query positions for account {} from OMS {}", accountId, omsId, e);
        }

        try {
            Oms.QueryOpenOrderRequest orderReq = Oms.QueryOpenOrderRequest.newBuilder()
                    .setAccountId(accountId)
                    .build();
            openOrderCount = omsClient.queryOpenOrders(omsId, orderReq).getOrdersCount();
        } catch (Exception e) {
            log.warn("Failed to query open orders for account {} from OMS {}", accountId, omsId, e);
        }

        return new AccountSummaryStats(positionCount, openOrderCount, "ok");
    }

    // --- mapping helpers ---

    private AccountSummaryResponse toAccountSummary(Map<String, Object> row,
                                                     AccountBindingResponse binding,
                                                     AccountSummaryStats summary) {
        return new AccountSummaryResponse(
                ((Number) row.get("account_id")).longValue(),
                (String) row.get("exch_account_id"),
                (String) row.get("venue"),
                (String) row.get("status"),
                (String) row.get("broker_type"),
                (String) row.get("account_type"),
                (String) row.get("base_currency"),
                binding,
                summary
        );
    }

    private AccountBindingResponse toBindingResponse(Map<String, Object> binding) {
        String omsId = objToString(binding.get("oms_id"));
        String gwId = objToString(binding.get("gw_id"));

        boolean omsLive = false;
        String omsAddress = null;
        if (omsId != null) {
            ServiceRegistration reg = discoveryCache.get("svc.oms." + omsId);
            omsLive = reg != null;
            if (reg != null && reg.hasEndpoint()) {
                omsAddress = reg.getEndpoint().getAddress();
            }
        }

        boolean gwLive = false;
        String gwAddress = null;
        if (gwId != null) {
            ServiceRegistration reg = discoveryCache.get("svc.gw." + gwId);
            gwLive = reg != null;
            if (reg != null && reg.hasEndpoint()) {
                gwAddress = reg.getEndpoint().getAddress();
            }
        }

        return new AccountBindingResponse(omsId, gwId, omsLive, omsAddress, gwLive, gwAddress);
    }

    private String resolveOmsId(long accountId) {
        Map<String, Object> binding = repository.getAccountBinding(accountId);
        if (binding == null) {
            return null;
        }
        Object omsId = binding.get("oms_id");
        return omsId != null ? omsId.toString() : null;
    }

    private static String objToString(Object obj) {
        return obj != null ? obj.toString() : null;
    }

    private static Instant toInstant(Object obj) {
        if (obj == null) return null;
        if (obj instanceof Instant i) return i;
        if (obj instanceof java.sql.Timestamp ts) return ts.toInstant();
        if (obj instanceof java.time.OffsetDateTime odt) return odt.toInstant();
        return null;
    }

    private static BigDecimal toBigDecimal(Object obj) {
        if (obj == null) return null;
        if (obj instanceof BigDecimal bd) return bd;
        if (obj instanceof Number n) return BigDecimal.valueOf(n.doubleValue());
        return null;
    }

    private static Long toLong(Object obj) {
        if (obj == null) return null;
        if (obj instanceof Number n) return n.longValue();
        return null;
    }

    private static TradeEntry toTradeEntry(Map<String, Object> row) {
        return new TradeEntry(
                ((Number) row.get("fill_id")).longValue(),
                ((Number) row.get("order_id")).longValue(),
                ((Number) row.get("account_id")).longValue(),
                (String) row.get("oms_id"),
                (String) row.get("gw_id"),
                (String) row.get("strategy_id"),
                (String) row.get("execution_id"),
                (String) row.get("instrument_id"),
                (String) row.get("side"),
                (String) row.get("open_close"),
                (String) row.get("fill_type"),
                (String) row.get("ext_trade_id"),
                toBigDecimal(row.get("filled_qty")),
                toBigDecimal(row.get("filled_price")),
                toInstant(row.get("filled_ts")),
                toBigDecimal(row.get("fee_total"))
        );
    }

    private static String shortOrderStatus(Oms.OrderStatus s) {
        return switch (s) {
            case ORDER_STATUS_PENDING -> "PENDING";
            case ORDER_STATUS_BOOKED -> "BOOKED";
            case ORDER_STATUS_PARTIALLY_FILLED -> "PARTIAL";
            case ORDER_STATUS_FILLED -> "FILLED";
            case ORDER_STATUS_CANCELLED -> "CANCELLED";
            case ORDER_STATUS_REJECTED -> "REJECTED";
            default -> s.name();
        };
    }

    private static String shortSide(zk.common.v1.Common.BuySellType s) {
        return switch (s) {
            case BS_BUY -> "BUY";
            case BS_SELL -> "SELL";
            default -> s.name();
        };
    }

    private static String shortLongShort(zk.common.v1.Common.LongShortType s) {
        return switch (s) {
            case LS_LONG -> "LONG";
            case LS_SHORT -> "SHORT";
            default -> s.name();
        };
    }

    private static ActivityEntry toActivityEntry(Map<String, Object> row) {
        return new ActivityEntry(
                (String) row.get("activity_type"),
                toInstant(row.get("ts")),
                toLong(row.get("order_id")),
                (String) row.get("instrument_id"),
                (String) row.get("side"),
                toBigDecimal(row.get("qty")),
                toBigDecimal(row.get("price")),
                (String) row.get("asset"),
                toBigDecimal(row.get("balance_change"))
        );
    }
}
