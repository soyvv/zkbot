package com.zkbot.pilot.topology.dto;

import com.fasterxml.jackson.annotation.JsonAlias;

import java.util.List;
import java.util.Map;

public record CreateServiceRequest(
    String logicalId,
    boolean enabled,
    @JsonAlias("runtimeConfig")
    Map<String, Object> providedConfig,
    String venue,
    Map<String, Object> metadata,
    List<BindingSpec> bindings
) {
    public record BindingSpec(String dstType, String dstId, boolean enabled) {}
}
