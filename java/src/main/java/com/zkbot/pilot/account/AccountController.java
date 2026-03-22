package com.zkbot.pilot.account;

import com.zkbot.pilot.account.dto.CancelOrdersRequest;
import com.zkbot.pilot.account.dto.CreateAccountRequest;
import com.zkbot.pilot.account.dto.UpdateAccountRequest;
import org.springframework.web.bind.annotation.*;

import java.util.List;
import java.util.Map;

@RestController
@RequestMapping("/v1/accounts")
public class AccountController {

    private final AccountService service;

    public AccountController(AccountService service) {
        this.service = service;
    }

    @PostMapping
    public Map<String, Object> createAccount(@RequestBody CreateAccountRequest request) {
        return service.createAccount(request);
    }

    @GetMapping
    public List<Map<String, Object>> listAccounts(
            @RequestParam(required = false) String venue,
            @RequestParam(required = false) String oms_id,
            @RequestParam(required = false) String status,
            @RequestParam(defaultValue = "50") int limit) {
        return service.listAccounts(venue, oms_id, status, limit);
    }

    @PutMapping("/{accountId}")
    public Map<String, Object> updateAccount(@PathVariable long accountId,
                                              @RequestBody UpdateAccountRequest request) {
        return service.updateAccount(accountId, request);
    }

    @GetMapping("/{accountId}")
    public Map<String, Object> getAccount(@PathVariable long accountId) {
        return service.getAccount(accountId);
    }

    @GetMapping("/{accountId}/balances")
    public List<Map<String, Object>> getBalances(@PathVariable long accountId) {
        return service.getBalances(accountId);
    }

    @GetMapping("/{accountId}/positions")
    public List<Map<String, Object>> getPositions(@PathVariable long accountId) {
        return service.getPositions(accountId);
    }

    @GetMapping("/{accountId}/orders/open")
    public List<Map<String, Object>> getOpenOrders(@PathVariable long accountId) {
        return service.getOpenOrders(accountId);
    }

    @GetMapping("/{accountId}/trades")
    public List<Map<String, Object>> getTrades(@PathVariable long accountId,
                                                @RequestParam(defaultValue = "50") int limit) {
        return service.getTrades(accountId, limit);
    }

    @GetMapping("/{accountId}/activities")
    public List<Map<String, Object>> getActivities(@PathVariable long accountId,
                                                    @RequestParam(defaultValue = "50") int limit) {
        return service.getActivities(accountId, limit);
    }

    @GetMapping("/{accountId}/runtime-binding")
    public Map<String, Object> getRuntimeBinding(@PathVariable long accountId) {
        return service.getRuntimeBinding(accountId);
    }

    @PostMapping("/{accountId}/orders/cancel")
    public Map<String, Object> cancelOrders(@PathVariable long accountId,
                                             @RequestBody CancelOrdersRequest request) {
        return service.cancelOrders(accountId, request.orderIds());
    }
}
