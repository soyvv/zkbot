package com.zkbot.pilot.account.dto;

import com.fasterxml.jackson.databind.annotation.JsonSerialize;
import com.fasterxml.jackson.databind.ser.std.ToStringSerializer;

import java.math.BigDecimal;
import java.time.Instant;

public record OrderHistoryEntry(
        @JsonSerialize(using = ToStringSerializer.class) long orderId,
        long accountId,
        String omsId,
        String gwId,
        String strategyId,
        String executionId,
        String sourceId,
        String instrumentId,
        String instrumentExch,
        String side,
        String openClose,
        String orderType,
        String orderStatus,
        BigDecimal price,
        BigDecimal qty,
        BigDecimal filledQty,
        BigDecimal filledAvgPrice,
        String extOrderRef,
        String errorMsg,
        Instant terminalAt,
        Instant createdAt
) {}
