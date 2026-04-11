package com.zkbot.pilot.bot.dto;

import java.util.List;
import java.util.Map;

public record CreateStrategyRequest(
    String strategyKey,
    String runtimeType,
    String codeRef,
    String strategyTypeKey,
    String description,
    List<Long> defaultAccounts,
    List<String> defaultSymbols,
    Map<String, Object> config
) {}
