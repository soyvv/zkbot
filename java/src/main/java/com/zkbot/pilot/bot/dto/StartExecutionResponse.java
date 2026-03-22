package com.zkbot.pilot.bot.dto;

import java.util.Map;

public record StartExecutionResponse(
    String executionId,
    String strategyId,
    String status,
    Map<String, Object> enrichedConfig
) {}
