package com.zkbot.pilot.manual;

import com.zkbot.pilot.common.SnowflakeIdGen;
import com.zkbot.pilot.grpc.OmsGrpcClient;
import com.zkbot.pilot.manual.dto.*;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Service;
import zk.common.v1.Common;
import zk.oms.v1.Oms;

import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.UUID;

@Service
public class ManualService {

    private static final Logger log = LoggerFactory.getLogger(ManualService.class);
    private static final String SOURCE_ID = "pilot-manual";

    private final OmsGrpcClient omsClient;
    private final JdbcTemplate jdbc;
    private final SnowflakeIdGen idGen;

    public ManualService(OmsGrpcClient omsClient, JdbcTemplate jdbc, SnowflakeIdGen idGen) {
        this.omsClient = omsClient;
        this.jdbc = jdbc;
        this.idGen = idGen;
    }

    public String resolveOmsForAccount(long accountId) {
        List<String> results = jdbc.queryForList(
                "SELECT oms_id FROM cfg.account_binding WHERE account_id = ? LIMIT 1",
                String.class, accountId);
        if (results.isEmpty()) {
            throw new IllegalArgumentException("No OMS binding found for account " + accountId);
        }
        return results.get(0);
    }

    public OrderPreviewResponse previewOrder(OrderRequest request) {
        List<String> warnings = new ArrayList<>();

        List<Map<String, Object>> refdata = jdbc.queryForList(
                "SELECT * FROM cfg.instrument_refdata WHERE instrument_id = ? LIMIT 1",
                request.instrumentId());

        if (refdata.isEmpty()) {
            return new OrderPreviewResponse(false, "Unknown instrument: " + request.instrumentId(),
                    null, null, null, null, null, null, null, null);
        }

        List<Map<String, Object>> accountCfg = jdbc.queryForList(
                "SELECT * FROM cfg.account_instrument_config WHERE account_id = ? AND instrument_id = ? LIMIT 1",
                request.accountId(), request.instrumentId());

        if (accountCfg.isEmpty()) {
            warnings.add("No account-level instrument config found; using defaults");
        }

        if (request.qty() == null || request.qty().isBlank()) {
            return new OrderPreviewResponse(false, "qty is required",
                    null, null, null, null, null, null, null, null);
        }
        if ("LIMIT".equalsIgnoreCase(request.orderType()) &&
                (request.price() == null || request.price().isBlank())) {
            return new OrderPreviewResponse(false, "price is required for LIMIT orders",
                    null, null, null, null, null, null, null, null);
        }

        return new OrderPreviewResponse(true, null,
                request.accountId(), request.instrumentId(), request.side(),
                request.orderType(), request.price(), request.qty(),
                request.timeInForce(), warnings.isEmpty() ? null : warnings);
    }

    public OmsActionResponse submitOrder(OrderRequest request) {
        String omsId = resolveOmsForAccount(request.accountId());
        Oms.PlaceOrderRequest proto = buildPlaceOrderRequest(request);

        log.info("Submitting order for account={} instrument={} via OMS={}",
                request.accountId(), request.instrumentId(), omsId);

        Oms.OMSResponse resp = omsClient.placeOrder(omsId, proto);
        return toOmsActionResponse(resp, omsId);
    }

    public List<OmsActionResponse> submitBatchOrders(List<OrderRequest> requests) {
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
        return List.of(toOmsActionResponse(resp, omsId));
    }

    public CancelOrdersResponse cancelOrders(long accountId, List<String> orderIds) {
        String omsId = resolveOmsForAccount(accountId);
        List<CancelOrdersResponse.CancelResult> results = new ArrayList<>();

        for (String orderIdStr : orderIds) {
            long orderId = Long.parseLong(orderIdStr);
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
            results.add(new CancelOrdersResponse.CancelResult(orderId, toOmsActionResponse(resp, omsId)));
        }

        return new CancelOrdersResponse(omsId, results);
    }

    public OmsActionResponse triggerPanic(long accountId, String omsId) {
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
        return toOmsActionResponse(resp, omsId);
    }

    public OmsActionResponse clearPanic(long accountId, String omsId) {
        if (omsId == null || omsId.isBlank()) {
            omsId = resolveOmsForAccount(accountId);
        }
        Oms.DontPanicRequest proto = Oms.DontPanicRequest.newBuilder()
                .setPanicAccountId(accountId)
                .build();

        log.info("Panic cleared for account={} via OMS={}", accountId, omsId);
        Oms.OMSResponse resp = omsClient.dontPanic(omsId, proto);
        return toOmsActionResponse(resp, omsId);
    }

    public OrderQueryResponse queryOrder(long orderId, String omsId) {
        Oms.QueryOrderDetailRequest proto = Oms.QueryOrderDetailRequest.newBuilder()
                .addOrderRefs(String.valueOf(orderId))
                .build();

        Oms.OrderDetailResponse resp = omsClient.queryOrderDetails(omsId, proto);
        List<OrderDetailEntry> orders = new ArrayList<>();
        for (Oms.Order o : resp.getOrdersList()) {
            orders.add(toOrderDetailEntry(o));
        }

        return new OrderQueryResponse(omsId, orders);
    }

    public List<ManualTradeEntry> queryTrades(long accountId, String omsId, int limit) {
        if (omsId == null || omsId.isBlank()) {
            omsId = resolveOmsForAccount(accountId);
        }

        Oms.QueryTradeDetailRequest proto = Oms.QueryTradeDetailRequest.newBuilder()
                .setPagination(Common.PaginationRequest.newBuilder()
                        .setPageSize(limit)
                        .build())
                .build();

        Oms.TradeDetailResponse resp = omsClient.queryTradeDetails(omsId, proto);
        List<ManualTradeEntry> trades = new ArrayList<>();
        for (Oms.Trade t : resp.getTradesList()) {
            trades.add(new ManualTradeEntry(
                    t.getOrderId(), t.getInstrument(), t.getBuySellType().name(),
                    t.getFilledQty(), t.getFilledPrice(), t.getFilledTs(), t.getAccountId()));
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
                .setOrderId(idGen.nextId())
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

    private static OmsActionResponse toOmsActionResponse(Oms.OMSResponse resp, String omsId) {
        String message = resp.getMessage().isEmpty() ? null : resp.getMessage();
        String errorType = resp.getErrorType() != Oms.OMSErrorType.OMS_ERR_TYPE_UNSPECIFIED
                ? resp.getErrorType().name() : null;
        return new OmsActionResponse(omsId, resp.getStatus().name(), resp.getTimestamp(), message, errorType);
    }

    private static OrderDetailEntry toOrderDetailEntry(Oms.Order o) {
        String errorMsg = o.getErrorMsg().isEmpty() ? null : o.getErrorMsg();
        return new OrderDetailEntry(
                o.getOrderId(), o.getInstrument(), o.getBuySellType().name(),
                o.getOrderStatus().name(), o.getPrice(), o.getQty(),
                o.getFilledQty(), o.getFilledAvgPrice(), o.getAccountId(),
                o.getCreatedAt(), o.getUpdatedAt(), errorMsg);
    }
}
