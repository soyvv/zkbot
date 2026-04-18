package com.zkbot.pilot.account.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

public record AccountSummaryResponse(
    long accountId,
    String exchAccountId,
    String venue,
    String status,
    String brokerType,
    String accountType,
    String baseCurrency,
    AccountBindingResponse binding,
    @JsonInclude(JsonInclude.Include.NON_NULL) AccountSummaryStats summary
) {}
