package com.zkbot.pilot.config;

import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.HttpMethod;
import org.springframework.security.config.annotation.web.builders.HttpSecurity;
import org.springframework.security.config.annotation.web.configuration.EnableWebSecurity;
import org.springframework.security.config.http.SessionCreationPolicy;
import org.springframework.security.web.SecurityFilterChain;

@Configuration
@EnableWebSecurity
public class SecurityConfig {

    @Bean
    public SecurityFilterChain securityFilterChain(HttpSecurity http) throws Exception {
        http
            .csrf(csrf -> csrf.disable())
            .sessionManagement(sm -> sm.sessionCreationPolicy(SessionCreationPolicy.STATELESS))
            .httpBasic(httpBasic -> {})
            .authorizeHttpRequests(auth -> auth
                // Actuator health is public
                .requestMatchers("/actuator/health", "/health").permitAll()
                // Manual trading
                .requestMatchers("/v1/manual/**").hasAnyRole("ADMIN", "OPS", "TRADER")
                // Accounts: read for TRADER, write for OPS+
                .requestMatchers(HttpMethod.GET, "/v1/accounts/**").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers("/v1/accounts/**").hasAnyRole("ADMIN", "OPS")
                // Topology
                .requestMatchers("/v1/topology/**").hasAnyRole("ADMIN", "OPS")
                // Bot management — definitions
                .requestMatchers(HttpMethod.GET, "/v1/bot/definitions/**").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers(HttpMethod.POST, "/v1/bot/definitions/*/start").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers(HttpMethod.POST, "/v1/bot/definitions/*/stop").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers("/v1/bot/definitions/**").hasAnyRole("ADMIN", "OPS")
                // Bot management — executions
                .requestMatchers(HttpMethod.POST, "/v1/bot/executions/start").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers(HttpMethod.POST, "/v1/bot/executions/*/stop").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers(HttpMethod.POST, "/v1/bot/executions/*/pause").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers(HttpMethod.POST, "/v1/bot/executions/*/resume").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers(HttpMethod.GET, "/v1/bot/**").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers("/v1/bot/**").hasAnyRole("ADMIN", "OPS")
                // Legacy compat aliases (same permissions as bot execution endpoints)
                .requestMatchers(HttpMethod.POST, "/v1/strategy-executions/**").hasAnyRole("ADMIN", "OPS", "TRADER")
                .requestMatchers(HttpMethod.GET, "/v1/strategies/**").hasAnyRole("ADMIN", "OPS", "TRADER")
                // Schema — strategy schemas readable by all roles
                .requestMatchers(HttpMethod.GET, "/v1/schema/strategies/**").hasAnyRole("ADMIN", "OPS", "TRADER")
                // Ops
                .requestMatchers("/v1/ops/**").hasAnyRole("ADMIN", "OPS")
                // Everything else requires ADMIN
                .anyRequest().hasRole("ADMIN")
            );
        return http.build();
    }
}
