package com.zkbot.pilot.bot.dto;

import java.util.List;
import java.util.Map;

public record UpdateStrategyRequest(
    String description,
    List<Long> defaultAccounts,
    List<String> defaultSymbols,
    Map<String, Object> config,
    Boolean enabled
) {}
