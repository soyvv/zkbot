package com.zkbot.pilot.schema.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

import java.time.Instant;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record SchemaResourceEntry(
        String resourceType,
        String resourceKey,
        String schemaId,
        int version,
        String contentHash,
        boolean active,
        String manifestJson,
        String configSchema,
        String syncedFrom,
        Instant createdAt,
        Instant updatedAt
) {}
