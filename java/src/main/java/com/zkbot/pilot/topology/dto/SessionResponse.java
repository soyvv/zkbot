package com.zkbot.pilot.topology.dto;

import java.time.Instant;

public record SessionResponse(
    String ownerSessionId,
    String logicalId,
    String instanceType,
    String kvKey,
    String lockKey,
    String status,
    Instant lastSeenAt,
    Instant expiresAt,
    long leaseTtlMs,
    boolean kvLive
) {}
