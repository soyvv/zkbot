package com.zkbot.pilot.support;

import org.springframework.test.context.DynamicPropertyRegistry;
import org.springframework.test.context.DynamicPropertySource;
import org.testcontainers.containers.PostgreSQLContainer;
import org.testcontainers.junit.jupiter.Container;
import org.testcontainers.junit.jupiter.Testcontainers;

@Testcontainers
public abstract class PostgresTestBase {

    @Container
    static final PostgreSQLContainer<?> pg =
            new PostgreSQLContainer<>("postgres:16-alpine")
                    .withDatabaseName("zkbot_test")
                    .withUsername("test")
                    .withPassword("test");

    @DynamicPropertySource
    static void pgProps(DynamicPropertyRegistry reg) {
        reg.add("spring.datasource.url", pg::getJdbcUrl);
        reg.add("spring.datasource.username", pg::getUsername);
        reg.add("spring.datasource.password", pg::getPassword);
    }
}
