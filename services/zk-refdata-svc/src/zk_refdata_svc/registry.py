"""Generic NATS KV service registry with CAS heartbeat.

Reusable by any Python service that needs to register in the shared
NATS KV bucket ``zk-svc-registry-v1``.

KV values are protobuf-encoded ``zk.discovery.v1.ServiceRegistration``
to match the Rust-side decoder (``ServiceRegistration::decode``).
"""

from __future__ import annotations

import asyncio
import time

import nats
import nats.js
from loguru import logger

from zk_refdata_svc.discovery_pb2 import ServiceRegistration, TransportEndpoint

REGISTRY_BUCKET = "zk-svc-registry-v1"


class ServiceRegistry:
    """Register a service in NATS KV with periodic CAS heartbeat."""

    def __init__(
        self,
        *,
        service_type: str,
        service_id: str,
        endpoint: str,
        capabilities: list[str],
        heartbeat_interval_s: float = 5.0,
        lease_ttl_s: float = 20.0,
        kv_key: str | None = None,
    ) -> None:
        self._service_type = service_type
        self._service_id = service_id
        self._endpoint = endpoint
        self._capabilities = capabilities
        self._heartbeat_interval_s = heartbeat_interval_s
        self._lease_ttl_s = lease_ttl_s

        self._kv: nats.js.kv.KeyValue | None = None
        self._key = kv_key or f"svc.{service_type}.{service_id}"
        self._revision: int = 0
        self._heartbeat_task: asyncio.Task | None = None
        self._fenced = asyncio.Event()

    # -- public API ----------------------------------------------------------

    async def start(self, nc: nats.NATS) -> None:
        """Perform initial registration and start heartbeat loop."""
        js = nc.jetstream()
        try:
            self._kv = await js.key_value(REGISTRY_BUCKET)
        except Exception:
            from nats.js.api import KeyValueConfig

            self._kv = await js.create_key_value(
                KeyValueConfig(bucket=REGISTRY_BUCKET, ttl=int(self._lease_ttl_s * 3))
            )
            logger.info(f"created registry KV bucket '{REGISTRY_BUCKET}'")

        payload = self._build_payload()
        self._revision = await self._kv.put(self._key, payload)
        logger.info(f"registered in KV: {self._key} (rev={self._revision})")

        self._heartbeat_task = asyncio.create_task(self._heartbeat_loop())

    def wait_fenced(self) -> asyncio.Event:
        """Return an event that is set when a CAS conflict (fencing) is detected."""
        return self._fenced

    async def deregister(self) -> None:
        """Cancel heartbeat and delete the KV key.

        Skips deletion if this instance has been fenced — the key now
        belongs to the winner and must not be removed.
        """
        if self._heartbeat_task and not self._heartbeat_task.done():
            self._heartbeat_task.cancel()
            try:
                await self._heartbeat_task
            except asyncio.CancelledError:
                pass
        if self._kv and not self._fenced.is_set():
            try:
                await self._kv.delete(self._key)
                logger.info(f"deregistered from KV: {self._key}")
            except Exception:
                logger.warning(f"failed to delete KV key {self._key}")

    # -- internals -----------------------------------------------------------

    def _build_payload(self) -> bytes:
        now_ms = int(time.time() * 1000)
        reg = ServiceRegistration(
            service_type=self._service_type,
            service_id=self._service_id,
            instance_id=f"{self._service_type}_{self._service_id}",
            endpoint=TransportEndpoint(
                protocol="grpc",
                address=self._endpoint,
            ),
            capabilities=self._capabilities,
            lease_expiry_ms=now_ms + int(self._lease_ttl_s * 1000),
            updated_at_ms=now_ms,
        )
        return reg.SerializeToString()

    async def _heartbeat_loop(self) -> None:
        while True:
            await asyncio.sleep(self._heartbeat_interval_s)
            try:
                payload = self._build_payload()
                self._revision = await self._kv.update(self._key, payload, self._revision)
            except nats.js.errors.KeyWrongLastSequenceError:
                logger.error(f"CAS conflict on {self._key} -- fenced by another instance")
                self._fenced.set()
                return
            except asyncio.CancelledError:
                return
            except Exception:
                logger.exception(f"heartbeat failed for {self._key}")
