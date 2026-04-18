package com.zkbot.pilot.manual;

import com.zkbot.pilot.manual.dto.*;
import org.springframework.web.bind.annotation.*;

import java.util.List;

@RestController
@RequestMapping("/v1/manual")
public class ManualController {

    private final ManualService manualService;

    public ManualController(ManualService manualService) {
        this.manualService = manualService;
    }

    @PostMapping("/orders:preview")
    public OrderPreviewResponse previewOrder(@RequestBody OrderRequest request) {
        return manualService.previewOrder(request);
    }

    @PostMapping("/orders")
    public OmsActionResponse submitOrder(@RequestBody OrderRequest request) {
        return manualService.submitOrder(request);
    }

    @PostMapping("/orders:batch")
    public List<OmsActionResponse> submitBatchOrders(@RequestBody List<OrderRequest> requests) {
        return manualService.submitBatchOrders(requests);
    }

    @PostMapping("/cancels")
    public CancelOrdersResponse cancelOrders(@RequestBody CancelRequest request) {
        return manualService.cancelOrders(request.accountId(), request.orderIds());
    }

    @PostMapping("/panic")
    public OmsActionResponse panic(@RequestBody PanicRequest request) {
        return manualService.triggerPanic(request.accountId(), request.omsId());
    }

    @PostMapping("/panic/clear")
    public OmsActionResponse clearPanic(@RequestBody PanicRequest request) {
        return manualService.clearPanic(request.accountId(), request.omsId());
    }

    @GetMapping("/orders/{orderId}")
    public OrderQueryResponse getOrder(
            @PathVariable long orderId,
            @RequestParam String account_id,
            @RequestParam String oms_id) {
        return manualService.queryOrder(orderId, oms_id);
    }

    @GetMapping("/trades")
    public List<ManualTradeEntry> getTrades(
            @RequestParam long account_id,
            @RequestParam(required = false) String oms_id,
            @RequestParam(defaultValue = "50") int limit) {
        return manualService.queryTrades(account_id, oms_id, limit);
    }
}
