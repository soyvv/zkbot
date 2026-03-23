package com.zkbot.pilot.account.dto;

public record OpenOrderEntry(
        long orderId,
        String exchOrderRef,
        String instrument,
        String side,
        String status,
        double price,
        double qty,
        double filledQty,
        double filledAvgPrice,
        long createdAt,
        long updatedAt
) {}
