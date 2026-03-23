package com.zkbot.pilot.config;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import org.springframework.jdbc.core.JdbcTemplate;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;

@ExtendWith(MockitoExtension.class)
class DesiredConfigRepositoryTest {

    @Mock JdbcTemplate jdbc;

    DesiredConfigRepository repo;

    @BeforeEach
    void setUp() {
        repo = new DesiredConfigRepository(jdbc);
    }

    @Test
    void isManagedType_returns_true_for_known_types() {
        assertThat(repo.isManagedType("OMS")).isTrue();
        assertThat(repo.isManagedType("GW")).isTrue();
        assertThat(repo.isManagedType("ENGINE")).isTrue();
        assertThat(repo.isManagedType("MDGW")).isTrue();
        assertThat(repo.isManagedType("oms")).isTrue(); // case insensitive
    }

    @Test
    void isManagedType_returns_false_for_unknown_types() {
        assertThat(repo.isManagedType("REFDATA")).isFalse();
        assertThat(repo.isManagedType("UNKNOWN")).isFalse();
    }

    @Test
    void getDesiredConfig_returns_null_for_unsupported_type() {
        assertThat(repo.getDesiredConfig("foo_1", "UNKNOWN")).isNull();
    }

    @Test
    void setDesiredConfig_throws_for_unsupported_type() {
        assertThatThrownBy(() -> repo.setDesiredConfig("foo_1", "UNKNOWN", "{}"))
                .isInstanceOf(IllegalArgumentException.class)
                .hasMessageContaining("unsupported instance_type");
    }
}
