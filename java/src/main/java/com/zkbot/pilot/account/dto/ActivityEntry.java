package com.zkbot.pilot.account.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

import java.math.BigDecimal;
import java.time.Instant;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record ActivityEntry(
        String activityType,
        Instant ts,
        Long orderId,
        String instrumentId,
        String side,
        BigDecimal qty,
        BigDecimal price,
        String asset,
        BigDecimal balanceChange
) {}
