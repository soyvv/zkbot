package com.zkbot.pilot.bot.dto;

import java.util.List;

public record ValidationResult(boolean valid, List<String> issues, String strategyKey) {}
