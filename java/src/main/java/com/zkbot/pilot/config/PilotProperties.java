package com.zkbot.pilot.config;

import org.springframework.boot.context.properties.ConfigurationProperties;

@ConfigurationProperties(prefix = "pilot")
public record PilotProperties(
    String natsUrl,
    String env,
    String pilotId,
    int leaseTtlMinutes,
    String venueIntegrationsRoot
) {
    public PilotProperties {
        if (leaseTtlMinutes <= 0) leaseTtlMinutes = 60;
    }
}
