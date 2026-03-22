package com.zkbot.pilot.account.dto;

public record CreateAccountRequest(
        long accountId,
        String exchAccountId,
        String venue,
        String brokerType,
        String accountType,
        String baseCurrency) {}
