"""Runtime configuration — thin wrapper over the typed config model.

Builds RefdataBootstrapConfig from env, then assembles RefdataRuntimeConfig
via the config_loader.
"""

import os

from loguru import logger

from zk_refdata_svc.config_model import RefdataBootstrapConfig, RefdataRuntimeConfig
from zk_refdata_svc.config_loader import assemble_runtime_config


def load_config(
    bootstrap_runtime_config: dict | None = None,
) -> RefdataRuntimeConfig:
    """Load and assemble the full runtime config from environment.

    In pilot mode, bootstrap_runtime_config contains venue configs from Pilot.
    In direct mode, venues and venue_configs_path are read from env vars.
    """
    bootstrap = RefdataBootstrapConfig.from_env()

    venues_str = os.getenv("ZK_REFDATA_VENUES", "")
    venues = [v.strip() for v in venues_str.split(",") if v.strip()]
    venue_configs_path = os.getenv("ZK_REFDATA_VENUE_CONFIGS")

    # Vault client construction (if bootstrap provides Vault inputs)
    vault_client = None
    if bootstrap.vault_addr and (bootstrap.vault_token or bootstrap.vault_role_id):
        try:
            vault_client = _build_vault_client(bootstrap)
            logger.info("vault client initialized for secret resolution")
        except Exception as exc:
            logger.warning(f"vault client init failed, using dev env fallback: {exc}")

    rtc = assemble_runtime_config(
        bootstrap=bootstrap,
        venues=venues,
        venue_configs_path=venue_configs_path,
        vault_client=vault_client,
        bootstrap_runtime_config=bootstrap_runtime_config,
    )

    logger.info(
        f"config assembled | mode={rtc.mode} "
        f"venues={rtc.enabled_venues()} "
        f"logical_id={rtc.logical_id}"
    )
    return rtc


def _build_vault_client(bootstrap: RefdataBootstrapConfig):
    """Build an hvac Vault client from bootstrap inputs."""
    import hvac

    client = hvac.Client(url=bootstrap.vault_addr)
    if bootstrap.vault_token:
        client.token = bootstrap.vault_token
    elif bootstrap.vault_role_id and bootstrap.vault_secret_id:
        resp = client.auth.approle.login(
            role_id=bootstrap.vault_role_id,
            secret_id=bootstrap.vault_secret_id,
        )
        client.token = resp["auth"]["client_token"]

    if not client.is_authenticated():
        raise RuntimeError("vault client not authenticated")

    class _HvacWrapper:
        """Thin wrapper matching the VaultClient protocol."""

        def __init__(self, hvac_client, mount_point="kv"):
            self._client = hvac_client
            self._mount = mount_point

        def read_secret(self, path: str) -> dict:
            resp = self._client.secrets.kv.v2.read_secret_version(
                path=path, mount_point=self._mount
            )
            return resp["data"]["data"]

    return _HvacWrapper(client)
