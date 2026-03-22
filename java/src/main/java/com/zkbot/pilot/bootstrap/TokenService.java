package com.zkbot.pilot.bootstrap;

import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Service;

import java.security.SecureRandom;
import java.time.OffsetDateTime;
import java.util.HexFormat;
import java.util.Map;
import java.util.UUID;

@Service
public class TokenService {

    private final BootstrapRepository repository;
    private final JdbcTemplate jdbc;
    private final SecureRandom random = new SecureRandom();

    public TokenService(BootstrapRepository repository, JdbcTemplate jdbc) {
        this.repository = repository;
        this.jdbc = jdbc;
    }

    /**
     * Validate a plaintext token. Returns tokenJti if valid, null otherwise.
     */
    public String validate(String token, String logicalId, String instanceType, String env) {
        return repository.validateToken(token, logicalId, instanceType, env);
    }

    /**
     * Generate a new token, store its hash, and return the plaintext token once.
     * The plaintext token is NOT stored.
     */
    public Map<String, Object> generateToken(String logicalId, String instanceType,
                                              String env, int expiresDays) {
        byte[] tokenBytes = new byte[32];
        random.nextBytes(tokenBytes);
        String token = HexFormat.of().formatHex(tokenBytes);
        String jti = UUID.randomUUID().toString();
        String tokenHash = BootstrapRepository.sha256Hex(token);

        OffsetDateTime expiresAt = jdbc.queryForObject("""
                INSERT INTO cfg.instance_token
                  (token_jti, logical_id, instance_type, token_hash, status, expires_at)
                VALUES (?, ?, ?, ?, 'active', now() + ? * interval '1 day')
                RETURNING expires_at
                """, OffsetDateTime.class,
                jti, logicalId, instanceType, tokenHash, expiresDays);

        return Map.of(
                "tokenJti", jti,
                "token", token,
                "expiresAt", expiresAt != null ? expiresAt.toString() : ""
        );
    }
}
