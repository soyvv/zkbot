-- Secret binding metadata: maps logical refs to Vault paths.
-- Raw secret material lives in Vault only — this table stores metadata for ops audit.

CREATE TABLE IF NOT EXISTS cfg.secret_binding (
    logical_ref      text NOT NULL,
    vault_path       text NOT NULL,
    service_kind     text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (logical_ref)
);

-- AppRole binding metadata: tracks which Vault AppRole is provisioned for each service.
CREATE TABLE IF NOT EXISTS cfg.vault_approle_binding (
    logical_id       text NOT NULL,
    service_kind     text NOT NULL,
    vault_role_name  text NOT NULL,
    vault_policy     text NOT NULL,
    account_ids      bigint[],
    provisioned_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (logical_id, service_kind)
);
