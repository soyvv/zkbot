package com.zkbot.pilot.topology.dto;

import java.util.Map;

public record UpdateServiceRequest(
    Boolean enabled,
    Map<String, Object> providedConfig
) {}
