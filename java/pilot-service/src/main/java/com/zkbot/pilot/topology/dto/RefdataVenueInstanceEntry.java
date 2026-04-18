package com.zkbot.pilot.topology.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

import java.time.Instant;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record RefdataVenueInstanceEntry(
        String logicalId,
        String env,
        String venue,
        String description,
        boolean enabled,
        String providedConfig,
        int configVersion,
        String configHash,
        Instant createdAt,
        Instant updatedAt
) {}
