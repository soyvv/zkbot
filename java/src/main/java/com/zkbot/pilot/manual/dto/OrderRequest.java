package com.zkbot.pilot.manual.dto;

public record OrderRequest(
        long accountId,
        String instrumentId,
        String side,        // BUY | SELL
        String orderType,   // LIMIT | MARKET
        String price,
        String qty,
        String timeInForce, // GTC | IOC | FOK
        String note
) {}
