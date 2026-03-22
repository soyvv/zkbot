package com.zkbot.pilot.topology.dto;

import java.time.Instant;
import java.util.List;
import java.util.Map;

public record ServiceDetail(
    String logicalId,
    String serviceKind,
    String registrationKind,
    boolean desiredEnabled,
    String liveStatus,
    String endpoint,
    String version,
    Instant lastSeenAt,
    String configDrift,
    List<Map<String, Object>> bindings,
    List<Map<String, Object>> sessions
) {}
