package com.zkbot.pilot.bot.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

import java.time.Instant;
import java.util.Map;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record BotDefinitionResponse(
        String engineId,
        String strategyId,
        String targetOmsId,
        String description,
        boolean enabled,
        Map<String, Object> providedConfig,
        String runtimeType,
        String codeRef,
        String currentExecutionId,
        String currentExecutionStatus,
        Instant currentExecutionStartedAt,
        Instant currentExecutionEndedAt,
        Instant createdAt,
        Instant updatedAt
) {}
