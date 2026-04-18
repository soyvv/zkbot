package com.zkbot.pilot.topology.dto;

import java.time.Instant;

public record BindingEntry(
        long bindingId,
        String srcType,
        String srcId,
        String dstType,
        String dstId,
        boolean enabled,
        Instant createdAt,
        Instant updatedAt
) {}
