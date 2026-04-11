package com.zkbot.pilot.bot.dto;

import java.util.Map;

public record UpdateBotDefinitionRequest(
        String description,
        Map<String, Object> providedConfig,
        Boolean enabled
) {}
