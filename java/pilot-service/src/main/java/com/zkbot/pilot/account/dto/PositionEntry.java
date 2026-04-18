package com.zkbot.pilot.account.dto;

public record PositionEntry(
        String instrument,
        String side,
        double totalQty,
        double frozenQty,
        double availQty
) {}
