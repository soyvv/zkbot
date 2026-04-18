package com.zkbot.pilot.account.dto;

import java.math.BigDecimal;
import java.time.Instant;

public record TradeEntry(
        long fillId,
        long orderId,
        long accountId,
        String omsId,
        String gwId,
        String strategyId,
        String executionId,
        String instrumentId,
        String side,
        String openClose,
        String fillType,
        String extTradeId,
        BigDecimal filledQty,
        BigDecimal filledPrice,
        Instant filledTs,
        BigDecimal feeTotal
) {}
