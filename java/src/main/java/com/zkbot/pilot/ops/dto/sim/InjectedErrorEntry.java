package com.zkbot.pilot.ops.dto.sim;

public record InjectedErrorEntry(
        String errorId,
        String scope,
        String effect,
        String triggerPolicy,
        int remainingTriggers,
        int priority,
        boolean enabled,
        long createdAt,
        long lastFiredAt
) {}
