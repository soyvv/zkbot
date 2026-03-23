package com.zkbot.pilot.bot.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

import java.time.Instant;
import java.util.Map;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record ExecutionResponse(
        String executionId,
        String strategyId,
        String targetOmsId,
        String status,
        String errorMessage,
        Instant startedAt,
        Instant endedAt,
        Map<String, Object> configOverride,
        Instant createdAt,
        Instant updatedAt,
        Boolean processRunning,
        Integer processExitCode
) {}
