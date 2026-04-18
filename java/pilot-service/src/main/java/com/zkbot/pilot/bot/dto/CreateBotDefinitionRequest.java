package com.zkbot.pilot.bot.dto;

import java.util.Map;

public record CreateBotDefinitionRequest(
        String engineId,
        String strategyId,
        String targetOmsId,
        String description,
        Map<String, Object> providedConfig,
        boolean enabled
) {}
