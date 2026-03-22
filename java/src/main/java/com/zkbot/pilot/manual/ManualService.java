package com.zkbot.pilot.manual;

import com.zkbot.pilot.grpc.OmsGrpcClient;
import com.zkbot.pilot.manual.dto.OrderRequest;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Service;
import zk.common.v1.Common;
import zk.oms.v1.Oms;

import java.time.Instant;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;

@Service
public class ManualService {

    private static final Logger log = LoggerFactory.getLogger(ManualService.class);
    private static final String SOURCE_ID = "pilot-manual";

    private final OmsGrpcClient omsClient;
    private final JdbcTemplate jdbc;

    public ManualService(OmsGrpcClient omsClient, JdbcTemplate jdbc) {
        this.omsClient = omsClient;
        this.jdbc = jdbc;
    }

    /**
     * Resolve which OMS handles a given account by querying cfg.account_binding.
     */
    public String resolveOmsForAccount(long accountId) {
        List<String> results = jdbc.queryForList(
                "SELECT oms_id FROM cfg.account_binding WHERE account_id = ? LIMIT 1",
                String.class, accountId);
        if (results.isEmpty()) {
            throw new IllegalArgumentException("No OMS binding found for account " + accountId);
        }
        return results.get(0);
    }

    /**
     * Validate order params against instrument refdata and account config.
     * Returns a preview map with warnings (if any).
     */
    public Map<String, Object> previewOrder(OrderRequest request) {
        Map<String, Object> result = new LinkedHashMap<>();
        List<String> warnings = new ArrayList<>();

        // Look up instrument refdata
        List<Map<String, Object>> refdata = jdbc.queryForList(
                "SELECT * FROM cfg.instrument_refdata WHERE instrument_id = ? LIMIT 1",
                request.instrumentId());

        if (refdata.isEmpty()) {
            result.put("valid", false);
            result.put("error", "Unknown instrument: " + request.instrumentId());
            return result;
        }

        Map<String, Object> inst = refdata.get(0);

        // Look up account-level instrument config
        List<Map<String, Object>> accountCfg = jdbc.queryForList(
                "SELECT * FROM cfg.account_instrument_config WHERE account_id = ? AND instrument_id = ? LIMIT 1",
                request.accountId(), request.instrumentId());

        if (accountCfg.isEmpty()) {
            warnings.add("No account-level instrument config found; using defaults");
        }

        // Basic validations
        if (request.qty() == null || request.qty().isBlank()) {
            result.put("valid", false);
            result.put("error", "qty is required");
            return result;
        }
        if ("LIMIT".equalsIgnoreCase(request.orderType()) &&
                (request.price() == null || request.price().isBlank())) {
            result.put("valid", false);
            result.put("error", "price is required for LIMIT orders");
            return result;
        }

        result.put("valid", true);
        result.put("accountId", request.accountId());
        result.put("instrumentId", request.instrumentId());
        result.put("side", request.side());
        result.put("orderType", request.orderType());
        result.put("price", request.price());
        result.put("qty", request.qty());
        result.put("timeInForce", request.timeInForce());
        if (!warnings.isEmpty()) {
            result.put("warnings", warnings);
        }
        return result;
    }

    /**
     * Build a PlaceOrderRequest proto and send to the resolved OMS.
     */
    public Map<String, Object> submitOrder(OrderRequest request) {
        String omsId = resolveOmsForAccount(request.accountId());
        Oms.PlaceOrderRequest proto = buildPlaceOrderRequest(request);

        log.info("Submitting order for account={} instrument={} via OMS={}",
                request.accountId(), request.instrumentId(), omsId);

        Oms.OMSResponse resp = omsClient.placeOrder(omsId, proto);
        return omsResponseToMap(resp, omsId);
    }

    /**
     * Build a BatchPlaceOrdersRequest and send to the resolved OMS.
     * All requests must belong to the same account.
     */
    public List<Map<String, Object>> submitBatchOrders(List<OrderRequest> requests) {
        if (requests.isEmpty()) {
            return List.of();
        }
        long accountId = requests.get(0).accountId();
        String omsId = resolveOmsForAccount(accountId);

        Oms.BatchPlaceOrdersRequest.Builder batch = Oms.BatchPlaceOrdersRequest.newBuilder()
                .setAuditMeta(buildAuditMeta());

        for (OrderRequest req : requests) {
            batch.addOrderRequests(buildOrderRequest(req));
        }

        log.info("Submitting batch of {} orders for account={} via OMS={}",
                requests.size(), accountId, omsId);

        Oms.OMSResponse resp = omsClient.batchPlaceOrders(omsId, batch.build());
        return List.of(omsResponseToMap(resp, omsId));
    }

    /**
     * Cancel one or more orders for an account.
     */
    public Map<String, Object> cancelOrders(long accountId, List<Long> orderIds) {
        String omsId = resolveOmsForAccount(accountId);
        List<Map<String, Object>> results = new ArrayList<>();

        for (long orderId : orderIds) {
            Oms.CancelOrderRequest proto = Oms.CancelOrderRequest.newBuilder()
                    .setOrderCancelRequest(Oms.OrderCancelRequest.newBuilder()
                            .setOrderId(orderId)
                            .setSourceId(SOURCE_ID)
                            .setTimestamp(Instant.now().toEpochMilli())
                            .build())
                    .setAuditMeta(buildAuditMeta())
                    .setIdempotencyKey(UUID.randomUUID().toString())
                    .build();

            Oms.OMSResponse resp = omsClient.cancelOrder(omsId, proto);
            results.add(Map.of("orderId", orderId, "response", omsResponseToMap(resp, omsId)));
        }

        Map<String, Object> out = new LinkedHashMap<>();
        out.put("omsId", omsId);
        out.put("results", results);
        return out;
    }

    public Map<String, Object> triggerPanic(long accountId, String omsId) {
        if (omsId == null || omsId.isBlank()) {
            omsId = resolveOmsForAccount(accountId);
        }
        Oms.PanicRequest proto = Oms.PanicRequest.newBuilder()
                .setPanicAccountId(accountId)
                .setPanicMessage("Manual panic from Pilot")
                .setPanicSource(SOURCE_ID)
                .build();

        log.warn("PANIC triggered for account={} via OMS={}", accountId, omsId);
        Oms.OMSResponse resp = omsClient.panic(omsId, proto);
        return omsResponseToMap(resp, omsId);
    }

    public Map<String, Object> clearPanic(long accountId, String omsId) {
        if (omsId == null || omsId.isBlank()) {
            omsId = resolveOmsForAccount(accountId);
        }
        Oms.DontPanicRequest proto = Oms.DontPanicRequest.newBuilder()
                .setPanicAccountId(accountId)
                .build();

        log.info("Panic cleared for account={} via OMS={}", accountId, omsId);
        Oms.OMSResponse resp = omsClient.dontPanic(omsId, proto);
        return omsResponseToMap(resp, omsId);
    }

    public Map<String, Object> queryOrder(long orderId, String omsId) {
        Oms.QueryOrderDetailRequest proto = Oms.QueryOrderDetailRequest.newBuilder()
                .addOrderRefs(String.valueOf(orderId))
                .build();

        Oms.OrderDetailResponse resp = omsClient.queryOrderDetails(omsId, proto);
        List<Map<String, Object>> orders = new ArrayList<>();
        for (Oms.Order o : resp.getOrdersList()) {
            orders.add(orderToMap(o));
        }

        Map<String, Object> result = new LinkedHashMap<>();
        result.put("omsId", omsId);
        result.put("orders", orders);
        return result;
    }

    public List<Map<String, Object>> queryTrades(long accountId, String omsId, int limit) {
        if (omsId == null || omsId.isBlank()) {
            omsId = resolveOmsForAccount(accountId);
        }

        Oms.QueryTradeDetailRequest proto = Oms.QueryTradeDetailRequest.newBuilder()
                .setPagination(Common.PaginationRequest.newBuilder()
                        .setPageSize(limit)
                        .build())
                .build();

        Oms.TradeDetailResponse resp = omsClient.queryTradeDetails(omsId, proto);
        List<Map<String, Object>> trades = new ArrayList<>();
        for (Oms.Trade t : resp.getTradesList()) {
            Map<String, Object> m = new LinkedHashMap<>();
            m.put("orderId", t.getOrderId());
            m.put("instrument", t.getInstrument());
            m.put("side", t.getBuySellType().name());
            m.put("filledQty", t.getFilledQty());
            m.put("filledPrice", t.getFilledPrice());
            m.put("filledTs", t.getFilledTs());
            m.put("accountId", t.getAccountId());
            trades.add(m);
        }
        return trades;
    }

    // ---- helpers ----

    private Oms.PlaceOrderRequest buildPlaceOrderRequest(OrderRequest request) {
        return Oms.PlaceOrderRequest.newBuilder()
                .setOrderRequest(buildOrderRequest(request))
                .setAuditMeta(buildAuditMeta())
                .setIdempotencyKey(UUID.randomUUID().toString())
                .build();
    }

    private Oms.OrderRequest buildOrderRequest(OrderRequest request) {
        Oms.OrderRequest.Builder builder = Oms.OrderRequest.newBuilder()
                .setAccountId(request.accountId())
                .setInstrumentCode(request.instrumentId())
                .setBuySellType(parseSide(request.side()))
                .setOrderType(parseOrderType(request.orderType()))
                .setQty(Double.parseDouble(request.qty()))
                .setSourceId(SOURCE_ID)
                .setTimestamp(Instant.now().toEpochMilli());

        if (request.price() != null && !request.price().isBlank()) {
            builder.setPrice(Double.parseDouble(request.price()));
        }
        if (request.timeInForce() != null && !request.timeInForce().isBlank()) {
            builder.setTimeInforceType(parseTimeInForce(request.timeInForce()));
        }
        return builder.build();
    }

    private Common.AuditMeta buildAuditMeta() {
        return Common.AuditMeta.newBuilder()
                .setSourceId(SOURCE_ID)
                .setRequestId(UUID.randomUUID().toString())
                .setTimestampMs(Instant.now().toEpochMilli())
                .build();
    }

    private static Common.BuySellType parseSide(String side) {
        return switch (side.toUpperCase()) {
            case "BUY" -> Common.BuySellType.BS_BUY;
            case "SELL" -> Common.BuySellType.BS_SELL;
            default -> throw new IllegalArgumentException("Invalid side: " + side);
        };
    }

    private static Common.BasicOrderType parseOrderType(String orderType) {
        return switch (orderType.toUpperCase()) {
            case "LIMIT" -> Common.BasicOrderType.ORDERTYPE_LIMIT;
            case "MARKET" -> Common.BasicOrderType.ORDERTYPE_MARKET;
            default -> throw new IllegalArgumentException("Invalid order type: " + orderType);
        };
    }

    private static Common.TimeInForceType parseTimeInForce(String tif) {
        return switch (tif.toUpperCase()) {
            case "GTC" -> Common.TimeInForceType.TIMEINFORCE_GTC;
            case "IOC" -> Common.TimeInForceType.TIMEINFORCE_IOC;
            case "FOK" -> Common.TimeInForceType.TIMEINFORCE_FOK;
            default -> throw new IllegalArgumentException("Invalid time in force: " + tif);
        };
    }

    private static Map<String, Object> omsResponseToMap(Oms.OMSResponse resp, String omsId) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("omsId", omsId);
        m.put("status", resp.getStatus().name());
        m.put("timestamp", resp.getTimestamp());
        if (!resp.getMessage().isEmpty()) {
            m.put("message", resp.getMessage());
        }
        if (resp.getErrorType() != Oms.OMSErrorType.OMS_ERR_TYPE_UNSPECIFIED) {
            m.put("errorType", resp.getErrorType().name());
        }
        return m;
    }

    private static Map<String, Object> orderToMap(Oms.Order o) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("orderId", o.getOrderId());
        m.put("instrument", o.getInstrument());
        m.put("side", o.getBuySellType().name());
        m.put("status", o.getOrderStatus().name());
        m.put("price", o.getPrice());
        m.put("qty", o.getQty());
        m.put("filledQty", o.getFilledQty());
        m.put("filledAvgPrice", o.getFilledAvgPrice());
        m.put("accountId", o.getAccountId());
        m.put("createdAt", o.getCreatedAt());
        m.put("updatedAt", o.getUpdatedAt());
        if (!o.getErrorMsg().isEmpty()) {
            m.put("errorMsg", o.getErrorMsg());
        }
        return m;
    }
}
