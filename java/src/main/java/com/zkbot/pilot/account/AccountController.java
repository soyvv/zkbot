package com.zkbot.pilot.account;

import com.zkbot.pilot.account.dto.*;
import org.springframework.web.bind.annotation.*;

import java.util.List;

@RestController
@RequestMapping("/v1/accounts")
public class AccountController {

    private final AccountService service;

    public AccountController(AccountService service) {
        this.service = service;
    }

    @PostMapping
    public AccountSummaryResponse createAccount(@RequestBody CreateAccountRequest request) {
        return service.createAccount(request);
    }

    @GetMapping
    public List<AccountSummaryResponse> listAccounts(
            @RequestParam(required = false) String venue,
            @RequestParam(required = false) String oms_id,
            @RequestParam(required = false) String status,
            @RequestParam(required = false) String include,
            @RequestParam(defaultValue = "50") int limit) {
        boolean includeSummary = include != null && include.contains("summary");
        boolean includeBinding = include != null && include.contains("binding");
        return service.listAccounts(venue, oms_id, status, limit, includeSummary, includeBinding);
    }

    @PutMapping("/{accountId}")
    public AccountSummaryResponse updateAccount(@PathVariable long accountId,
                                                 @RequestBody UpdateAccountRequest request) {
        return service.updateAccount(accountId, request);
    }

    @GetMapping("/{accountId}")
    public AccountSummaryResponse getAccount(@PathVariable long accountId) {
        return service.getAccount(accountId);
    }

    @GetMapping("/{accountId}/balances")
    public List<BalanceEntry> getBalances(@PathVariable long accountId) {
        return service.getBalances(accountId);
    }

    @GetMapping("/{accountId}/positions")
    public List<PositionEntry> getPositions(@PathVariable long accountId) {
        return service.getPositions(accountId);
    }

    @GetMapping("/{accountId}/orders/open")
    public List<OpenOrderEntry> getOpenOrders(@PathVariable long accountId) {
        return service.getOpenOrders(accountId);
    }

    @GetMapping("/{accountId}/trades")
    public List<TradeEntry> getTrades(@PathVariable long accountId,
                                      @RequestParam(defaultValue = "50") int limit) {
        return service.getTrades(accountId, limit);
    }

    @GetMapping("/{accountId}/activities")
    public List<ActivityEntry> getActivities(@PathVariable long accountId,
                                              @RequestParam(defaultValue = "50") int limit) {
        return service.getActivities(accountId, limit);
    }

    @GetMapping("/{accountId}/runtime-binding")
    public AccountBindingResponse getRuntimeBinding(@PathVariable long accountId) {
        return service.getRuntimeBinding(accountId);
    }

    @PostMapping("/{accountId}/orders/cancel")
    public CancelOrdersResponse cancelOrders(@PathVariable long accountId,
                                              @RequestBody CancelOrdersRequest request) {
        return service.cancelOrders(accountId, request.orderIds());
    }
}
