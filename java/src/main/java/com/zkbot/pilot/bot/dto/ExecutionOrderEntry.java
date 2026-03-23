package com.zkbot.pilot.bot.dto;

public record ExecutionOrderEntry(
        long orderId,
        String instrument,
        String side,
        double price,
        double qty,
        String status
) {}
