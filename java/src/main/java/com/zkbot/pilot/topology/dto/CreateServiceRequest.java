package com.zkbot.pilot.topology.dto;

import java.util.List;
import java.util.Map;

public record CreateServiceRequest(
    String logicalId,
    boolean enabled,
    Map<String, Object> runtimeConfig,
    Map<String, Object> metadata,
    List<BindingSpec> bindings
) {
    public record BindingSpec(String dstType, String dstId, boolean enabled) {}
}
