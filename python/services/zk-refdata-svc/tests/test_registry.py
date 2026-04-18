"""Tests for ServiceRegistry with CAS heartbeat."""

import asyncio

import pytest

from zk.discovery.v1.discovery_pb2 import ServiceRegistration
from zk_refdata_svc.registry import ServiceRegistry


class MockKV:
    """Mock NATS KV store for testing."""

    def __init__(self):
        self._data: dict[str, tuple[bytes, int]] = {}
        self._revision = 0
        self._fail_update = False

    async def put(self, key: str, value: bytes) -> int:
        self._revision += 1
        self._data[key] = (value, self._revision)
        return self._revision

    async def update(self, key: str, value: bytes, last: int) -> int:
        if self._fail_update:
            from nats.js.errors import KeyWrongLastSequenceError
            raise KeyWrongLastSequenceError()
        stored = self._data.get(key)
        if stored is None or stored[1] != last:
            from nats.js.errors import KeyWrongLastSequenceError
            raise KeyWrongLastSequenceError()
        self._revision += 1
        self._data[key] = (value, self._revision)
        return self._revision

    async def delete(self, key: str) -> None:
        self._data.pop(key, None)


class MockJetStream:
    def __init__(self, kv: MockKV):
        self._kv = kv

    async def key_value(self, bucket: str):
        return self._kv


class MockNATS:
    def __init__(self, kv: MockKV):
        self._kv = kv

    def jetstream(self):
        return MockJetStream(self._kv)


@pytest.fixture
def mock_kv():
    return MockKV()


@pytest.fixture
def mock_nc(mock_kv):
    return MockNATS(mock_kv)


def _make_registry(**kwargs) -> ServiceRegistry:
    defaults = {
        "service_type": "refdata",
        "service_id": "test_1",
        "endpoint": "localhost:50052",
        "capabilities": ["query_instrument_refdata"],
        "heartbeat_interval_s": 0.1,
        "lease_ttl_s": 1.0,
    }
    defaults.update(kwargs)
    return ServiceRegistry(**defaults)


async def test_register_creates_kv_entry(mock_nc, mock_kv):
    reg = _make_registry()
    await reg.start(mock_nc)
    assert "svc.refdata.test_1" in mock_kv._data
    raw = mock_kv._data["svc.refdata.test_1"][0]
    payload = ServiceRegistration()
    payload.ParseFromString(raw)
    assert payload.service_type == "refdata"
    assert payload.service_id == "test_1"
    assert payload.instance_id == "refdata_test_1"
    assert payload.endpoint.protocol == "grpc"
    assert payload.endpoint.address == "localhost:50052"
    assert payload.lease_expiry_ms > 0
    await reg.deregister()


async def test_heartbeat_updates_revision(mock_nc, mock_kv):
    reg = _make_registry(heartbeat_interval_s=0.05)
    await reg.start(mock_nc)
    initial_rev = reg._revision
    await asyncio.sleep(0.15)
    assert reg._revision > initial_rev
    await reg.deregister()


async def test_cas_failure_sets_fenced(mock_nc, mock_kv):
    reg = _make_registry(heartbeat_interval_s=0.05)
    await reg.start(mock_nc)
    # Simulate another writer by bumping revision directly.
    mock_kv._fail_update = True
    await asyncio.sleep(0.15)
    assert reg._fenced.is_set()
    await reg.deregister()


async def test_deregister_deletes_key(mock_nc, mock_kv):
    reg = _make_registry()
    await reg.start(mock_nc)
    assert "svc.refdata.test_1" in mock_kv._data
    await reg.deregister()
    assert "svc.refdata.test_1" not in mock_kv._data


async def test_fenced_instance_skips_delete(mock_nc, mock_kv):
    """A fenced instance must NOT delete the KV key (it belongs to the winner)."""
    reg = _make_registry(heartbeat_interval_s=0.05)
    await reg.start(mock_nc)
    mock_kv._fail_update = True
    await asyncio.sleep(0.15)
    assert reg._fenced.is_set()
    # Key should still exist after deregister because we were fenced.
    await reg.deregister()
    assert "svc.refdata.test_1" in mock_kv._data
