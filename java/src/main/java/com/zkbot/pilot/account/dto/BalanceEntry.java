package com.zkbot.pilot.account.dto;

public record BalanceEntry(
        String asset,
        double totalQty,
        double frozenQty,
        double availQty
) {}
