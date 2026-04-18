package com.zkbot.pilot.topology.dto;

public record ServiceNode(
    String logicalId,
    String serviceKind,
    String registrationKind,
    String status,
    String endpoint,
    String version,
    String drift,
    String sessionId,
    boolean desiredEnabled
) {}
