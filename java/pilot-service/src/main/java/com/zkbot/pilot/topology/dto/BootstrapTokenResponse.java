package com.zkbot.pilot.topology.dto;

import java.time.Instant;

public record BootstrapTokenResponse(
    String tokenJti,
    String token,
    Instant expiresAt
) {}
