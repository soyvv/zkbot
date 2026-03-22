package com.zkbot.pilot.manual;

import com.zkbot.pilot.manual.dto.CancelRequest;
import com.zkbot.pilot.manual.dto.OrderRequest;
import com.zkbot.pilot.manual.dto.PanicRequest;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestBody;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestParam;
import org.springframework.web.bind.annotation.RestController;

import java.util.List;
import java.util.Map;

@RestController
@RequestMapping("/v1/manual")
public class ManualController {

    private final ManualService manualService;

    public ManualController(ManualService manualService) {
        this.manualService = manualService;
    }

    @PostMapping("/orders:preview")
    public Map<String, Object> previewOrder(@RequestBody OrderRequest request) {
        return manualService.previewOrder(request);
    }

    @PostMapping("/orders")
    public Map<String, Object> submitOrder(@RequestBody OrderRequest request) {
        return manualService.submitOrder(request);
    }

    @PostMapping("/orders:batch")
    public List<Map<String, Object>> submitBatchOrders(@RequestBody List<OrderRequest> requests) {
        return manualService.submitBatchOrders(requests);
    }

    @PostMapping("/cancels")
    public Map<String, Object> cancelOrders(@RequestBody CancelRequest request) {
        return manualService.cancelOrders(request.accountId(), request.orderIds());
    }

    @PostMapping("/panic")
    public Map<String, Object> panic(@RequestBody PanicRequest request) {
        return manualService.triggerPanic(request.accountId(), request.omsId());
    }

    @PostMapping("/panic/clear")
    public Map<String, Object> clearPanic(@RequestBody PanicRequest request) {
        return manualService.clearPanic(request.accountId(), request.omsId());
    }

    @GetMapping("/orders/{orderId}")
    public Map<String, Object> getOrder(
            @PathVariable long orderId,
            @RequestParam String account_id,
            @RequestParam String oms_id) {
        return manualService.queryOrder(orderId, oms_id);
    }

    @GetMapping("/trades")
    public List<Map<String, Object>> getTrades(
            @RequestParam long account_id,
            @RequestParam(required = false) String oms_id,
            @RequestParam(defaultValue = "50") int limit) {
        return manualService.queryTrades(account_id, oms_id, limit);
    }
}
