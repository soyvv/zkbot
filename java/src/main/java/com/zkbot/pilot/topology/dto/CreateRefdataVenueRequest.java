package com.zkbot.pilot.topology.dto;

import java.util.Map;

public record CreateRefdataVenueRequest(
        String logicalId,
        String venue,
        String description,
        Boolean enabled,
        Map<String, Object> providedConfig
) {}
