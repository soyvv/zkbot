package com.zkbot.pilot.bot.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

import java.time.Instant;
import java.util.List;
import java.util.Map;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record StrategyResponse(
        String strategyId,
        String runtimeType,
        String codeRef,
        String strategyTypeKey,
        String description,
        List<Long> defaultAccounts,
        List<String> defaultSymbols,
        Map<String, Object> config,
        boolean enabled,
        Instant createdAt,
        Instant updatedAt,
        String executionId,
        String executionStatus,
        Instant executionStartedAt,
        Instant executionEndedAt
) {}
