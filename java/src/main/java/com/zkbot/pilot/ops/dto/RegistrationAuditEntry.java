package com.zkbot.pilot.ops.dto;

import java.time.Instant;

public record RegistrationAuditEntry(
    String tokenJti,
    String logicalId,
    String instanceType,
    String ownerSessionId,
    String decision,
    String reason,
    Instant observedAt
) {}
