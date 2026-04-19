package com.zkbot.pilot.config;

import org.springframework.boot.context.properties.ConfigurationProperties;

@ConfigurationProperties(prefix = "pilot")
public record PilotProperties(
    String natsUrl,
    String env,
    String pilotId,
    int leaseTtlMinutes,
    String venueIntegrationsRoot,
    String serviceManifestsRoot,
    String engineBinaryPath,
    String engineLogDir
) {
    public PilotProperties {
        if (leaseTtlMinutes <= 0) leaseTtlMinutes = 60;
        if (engineLogDir == null || engineLogDir.isBlank()) engineLogDir = "logs";
        // engineBinaryPath has no default: callers must supply an absolute,
        // executable path via the PILOT_ENGINE_BINARY_PATH env var (Spring
        // relaxed binding) or the pilot.engine-binary-path property in the
        // active Spring profile. See docs/system-arch/dependency-contract.md.
    }
}
