package com.zkbot.pilot.config;

import io.nats.client.Connection;
import org.springframework.boot.actuate.health.Health;
import org.springframework.boot.actuate.health.HealthIndicator;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

@Configuration
public class ActuatorHealthConfig {

    @Bean
    public HealthIndicator natsHealthIndicator(Connection natsConnection) {
        return () -> {
            Connection.Status status = natsConnection.getStatus();
            if (status == Connection.Status.CONNECTED) {
                return Health.up()
                        .withDetail("url", natsConnection.getConnectedUrl())
                        .build();
            }
            return Health.down()
                    .withDetail("status", status.name())
                    .build();
        };
    }
}
