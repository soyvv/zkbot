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
        if (engineBinaryPath == null || engineBinaryPath.isBlank()) engineBinaryPath = "zk-engine-svc";
        if (engineLogDir == null || engineLogDir.isBlank()) engineLogDir = "logs";
    }
}
