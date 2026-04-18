package com.zkbot.pilot.topology.dto;

import java.util.Map;

public record BindingRequest(
    String srcType,
    String srcId,
    String dstType,
    String dstId,
    boolean enabled,
    Map<String, Object> metadata
) {}
