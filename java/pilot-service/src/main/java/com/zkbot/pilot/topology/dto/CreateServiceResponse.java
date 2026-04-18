package com.zkbot.pilot.topology.dto;

public record CreateServiceResponse(
        String status,
        String logicalId,
        String serviceKind,
        BootstrapTokenResponse token
) {}
