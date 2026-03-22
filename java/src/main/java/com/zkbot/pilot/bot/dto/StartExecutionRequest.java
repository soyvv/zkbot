package com.zkbot.pilot.bot.dto;

import java.util.Map;

public record StartExecutionRequest(
    String strategyKey,
    String reason,
    Map<String, Object> runtimeParams,
    boolean orchestrated
) {}
