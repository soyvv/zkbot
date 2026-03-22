package com.zkbot.pilot.config;

import io.nats.client.Connection;
import io.nats.client.KeyValueManagement;
import io.nats.client.Nats;
import io.nats.client.Options;
import io.nats.client.api.KeyValueConfiguration;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

import java.time.Duration;

@Configuration
public class NatsConfig implements DisposableBean {

    private static final Logger log = LoggerFactory.getLogger(NatsConfig.class);
    private static final String REGISTRY_BUCKET = "zk-svc-registry-v1";

    private Connection connection;

    @Bean
    public Connection natsConnection(PilotProperties props) throws Exception {
        Options opts = new Options.Builder()
                .server(props.natsUrl())
                .reconnectWait(Duration.ofSeconds(2))
                .maxReconnects(-1)
                .build();

        connection = Nats.connect(opts);
        log.info("Connected to NATS at {}", props.natsUrl());

        ensureRegistryBucket();
        return connection;
    }

    private void ensureRegistryBucket() throws Exception {
        KeyValueManagement kvm = connection.keyValueManagement();
        try {
            kvm.getStatus(REGISTRY_BUCKET);
            log.info("KV bucket '{}' already exists", REGISTRY_BUCKET);
        } catch (Exception e) {
            KeyValueConfiguration kvConfig = KeyValueConfiguration.builder()
                    .name(REGISTRY_BUCKET)
                    .ttl(Duration.ofSeconds(60))
                    .build();
            kvm.create(kvConfig);
            log.info("Created KV bucket '{}' with 60s TTL", REGISTRY_BUCKET);
        }
    }

    @Override
    public void destroy() {
        if (connection != null) {
            try {
                connection.close();
                log.info("NATS connection closed");
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                log.warn("Interrupted while closing NATS connection");
            }
        }
    }
}
