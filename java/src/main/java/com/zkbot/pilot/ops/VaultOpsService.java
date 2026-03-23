package com.zkbot.pilot.ops;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Service;

import java.time.Instant;
import java.util.List;

/**
 * Pilot ops workflows for Vault provisioning.
 *
 * Responsibilities:
 * - AppRole management: create/update Vault policies + AppRole bindings
 * - Runtime identity generation: produce short-lived secret_ids for orchestrator injection
 * - Secret metadata persistence (not raw secret material, which lives in Vault only)
 *
 * Note: stashSecret (operator-facing) is deferred until the operator UI is built.
 * The initial implementation focuses on provisionAppRole and generateRuntimeIdentity.
 */
@Service
public class VaultOpsService {

    private static final Logger log = LoggerFactory.getLogger(VaultOpsService.class);

    private final JdbcTemplate jdbc;

    public VaultOpsService(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    /**
     * Record a secret binding (logical_ref → vault_path mapping) in Pilot DB.
     * Does not write raw secrets — those go directly to Vault.
     */
    public void recordSecretBinding(String logicalRef, String vaultPath, String serviceKind) {
        jdbc.update("""
                INSERT INTO cfg.secret_binding (logical_ref, vault_path, service_kind)
                VALUES (?, ?, ?)
                ON CONFLICT (logical_ref) DO UPDATE
                SET vault_path = EXCLUDED.vault_path,
                    service_kind = EXCLUDED.service_kind,
                    updated_at = now()
                """, logicalRef, vaultPath, serviceKind);
        log.info("vault-ops: recorded secret binding logical_ref={} vault_path={}", logicalRef, vaultPath);
    }

    /**
     * Record an AppRole provisioning in Pilot DB for audit.
     * Actual Vault policy/AppRole creation is done externally or via Vault CLI.
     */
    public void recordAppRoleBinding(String logicalId, String serviceKind,
                                       String vaultRoleName, String vaultPolicy,
                                       List<Long> accountIds) {
        long[] accountArray = accountIds != null
                ? accountIds.stream().mapToLong(Long::longValue).toArray()
                : new long[0];
        jdbc.update("""
                INSERT INTO cfg.vault_approle_binding
                  (logical_id, service_kind, vault_role_name, vault_policy, account_ids)
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT (logical_id, service_kind) DO UPDATE
                SET vault_role_name = EXCLUDED.vault_role_name,
                    vault_policy = EXCLUDED.vault_policy,
                    account_ids = EXCLUDED.account_ids,
                    provisioned_at = now()
                """, logicalId, serviceKind, vaultRoleName, vaultPolicy, accountArray);
        log.info("vault-ops: recorded AppRole binding logical_id={} role={}", logicalId, vaultRoleName);
    }

    /**
     * Generate runtime identity material for orchestrator injection.
     * This is a placeholder — actual secret_id generation requires Vault API calls.
     *
     * In the initial implementation, the operator manually provisions AppRoles in Vault
     * and provides the role_id/secret_id via environment variables or a secure channel.
     */
    public VaultIdentityMaterial generateRuntimeIdentity(String logicalId) {
        // Look up the AppRole binding for this logical_id
        var rows = jdbc.queryForList("""
                SELECT vault_role_name FROM cfg.vault_approle_binding
                WHERE logical_id = ?
                ORDER BY provisioned_at DESC LIMIT 1
                """, logicalId);
        if (rows.isEmpty()) {
            log.warn("vault-ops: no AppRole binding for logical_id={}", logicalId);
            return null;
        }

        // TODO: Call Vault API to generate a short-lived secret_id
        // POST /v1/auth/approle/role/{role_name}/secret-id
        // For now, return a placeholder indicating manual provisioning is required.
        String roleName = (String) rows.getFirst().get("vault_role_name");
        log.info("vault-ops: identity request for logical_id={}, role={}. " +
                "Manual provisioning required until Vault API integration is complete.", logicalId, roleName);

        return new VaultIdentityMaterial(
                null, // vaultAddr — from environment
                null, // roleId — from Vault
                null, // secretId — from Vault
                Instant.now().plusSeconds(300) // placeholder TTL
        );
    }

    public record VaultIdentityMaterial(
            String vaultAddr,
            String roleId,
            String secretId,
            Instant expiresAt
    ) {}
}
