"""Bootstrap handler and KvReconciler unit tests — no DB or NATS required."""

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from nats.js.kv import KeyValueOperation

from zk_pilot.bootstrap import KvReconciler, make_handlers
from zk_pilot.config import Config
from zk_pilot.proto import (
    BootstrapDeregisterRequest,
    BootstrapDeregisterResponse,
    BootstrapRegisterRequest,
    BootstrapRegisterResponse,
)


# ── Helpers ─────────────────────────────────────────────────────────────────────


def make_mock_msg(proto_obj):
    """Return a fake NATS Msg with .data and .respond()."""
    msg = MagicMock()
    msg.data = bytes(proto_obj)
    msg.respond = AsyncMock()
    return msg


def make_register_request(**kwargs) -> BootstrapRegisterRequest:
    defaults = dict(token="tok", logical_id="oms1", instance_type="OMS", env="dev", runtime_info={})
    return BootstrapRegisterRequest(**{**defaults, **kwargs})


def make_pg_pool(conn: AsyncMock) -> MagicMock:
    ctx = MagicMock()
    ctx.__aenter__ = AsyncMock(return_value=conn)
    ctx.__aexit__ = AsyncMock(return_value=None)
    pg_pool = MagicMock()
    pg_pool.acquire.return_value = ctx
    return pg_pool


# ── Handler tests ────────────────────────────────────────────────────────────────


async def test_handle_register_returns_ok_for_valid_request():
    conn = AsyncMock()
    reconciler = MagicMock()
    reconciler.is_kv_live.return_value = False
    with (
        patch("zk_pilot.bootstrap.db.validate_token", AsyncMock(return_value="jti")),
        patch("zk_pilot.bootstrap.db.find_session_for_logical", AsyncMock(return_value=None)),
        patch("zk_pilot.bootstrap.db.record_session", AsyncMock()),
    ):
        handle_register, _ = make_handlers(make_pg_pool(conn), Config(), reconciler)
        msg = make_mock_msg(make_register_request())
        await handle_register(msg)
    reply = BootstrapRegisterResponse().parse(msg.respond.call_args[0][0])
    assert reply.status == "OK"


async def test_handle_register_rejects_invalid_token():
    with patch("zk_pilot.bootstrap.db.validate_token", AsyncMock(return_value=None)):
        handle_register, _ = make_handlers(make_pg_pool(AsyncMock()), Config(), MagicMock())
        msg = make_mock_msg(make_register_request())
        await handle_register(msg)
    reply = BootstrapRegisterResponse().parse(msg.respond.call_args[0][0])
    assert reply.status == "TOKEN_EXPIRED"


async def test_handle_register_rejects_duplicate_when_kv_live():
    existing = {
        "owner_session_id": "old-sid",
        "kv_key": "svc.oms.oms1",
        "instance_type": "OMS",
    }
    reconciler = MagicMock()
    reconciler.is_kv_live.return_value = True
    with (
        patch("zk_pilot.bootstrap.db.validate_token", AsyncMock(return_value="jti")),
        patch("zk_pilot.bootstrap.db.find_session_for_logical", AsyncMock(return_value=existing)),
    ):
        handle_register, _ = make_handlers(make_pg_pool(AsyncMock()), Config(), reconciler)
        msg = make_mock_msg(make_register_request())
        await handle_register(msg)
    reply = BootstrapRegisterResponse().parse(msg.respond.call_args[0][0])
    assert reply.status == "DUPLICATE"


async def test_handle_register_fences_stale_db_session_when_kv_missing():
    existing = {
        "owner_session_id": "old-sid",
        "kv_key": "svc.oms.oms1",
        "instance_type": "OMS",
    }
    reconciler = MagicMock()
    reconciler.is_kv_live.return_value = False
    fence_mock = AsyncMock()
    conn = AsyncMock()
    conn.fetchrow = AsyncMock(return_value=None)  # env lookup for ENGINE (not called for OMS)
    with (
        patch("zk_pilot.bootstrap.db.validate_token", AsyncMock(return_value="jti")),
        patch("zk_pilot.bootstrap.db.find_session_for_logical", AsyncMock(return_value=existing)),
        patch("zk_pilot.bootstrap.db.fence_session", fence_mock),
        patch("zk_pilot.bootstrap.db.record_session", AsyncMock()),
    ):
        handle_register, _ = make_handlers(make_pg_pool(conn), Config(), reconciler)
        msg = make_mock_msg(make_register_request())
        await handle_register(msg)
    fence_mock.assert_called_once_with(conn, "old-sid")
    reply = BootstrapRegisterResponse().parse(msg.respond.call_args[0][0])
    assert reply.status == "OK"


async def test_handle_register_engine_allocates_instance_id():
    conn = AsyncMock()
    with (
        patch("zk_pilot.bootstrap.db.validate_token", AsyncMock(return_value="jti")),
        patch("zk_pilot.bootstrap.db.find_session_for_logical", AsyncMock(return_value=None)),
        patch("zk_pilot.bootstrap.db.acquire_instance_id", AsyncMock(return_value=7)),
        patch("zk_pilot.bootstrap.db.record_session", AsyncMock()),
    ):
        handle_register, _ = make_handlers(make_pg_pool(conn), Config(), MagicMock())
        msg = make_mock_msg(make_register_request(instance_type="ENGINE"))
        await handle_register(msg)
    reply = BootstrapRegisterResponse().parse(msg.respond.call_args[0][0])
    assert reply.status == "OK"
    assert reply.instance_id == 7


async def test_handle_register_engine_rejects_when_no_instance_id_available():
    with (
        patch("zk_pilot.bootstrap.db.validate_token", AsyncMock(return_value="jti")),
        patch("zk_pilot.bootstrap.db.find_session_for_logical", AsyncMock(return_value=None)),
        patch("zk_pilot.bootstrap.db.acquire_instance_id", AsyncMock(return_value=None)),
    ):
        handle_register, _ = make_handlers(make_pg_pool(AsyncMock()), Config(), MagicMock())
        msg = make_mock_msg(make_register_request(instance_type="ENGINE"))
        await handle_register(msg)
    reply = BootstrapRegisterResponse().parse(msg.respond.call_args[0][0])
    assert reply.status == "NO_INSTANCE_ID_AVAILABLE"


async def test_handle_deregister_releases_engine_lease():
    session = {"logical_id": "eng1", "instance_type": "ENGINE"}
    release_mock = AsyncMock()
    conn = AsyncMock()
    conn.fetchrow = AsyncMock(return_value={"env": "dev"})
    with (
        patch("zk_pilot.bootstrap.db.get_session", AsyncMock(return_value=session)),
        patch("zk_pilot.bootstrap.db.deregister_session", AsyncMock()),
        patch("zk_pilot.bootstrap.db.release_instance_id", release_mock),
    ):
        _, handle_deregister = make_handlers(make_pg_pool(conn), Config(), MagicMock())
        msg = make_mock_msg(BootstrapDeregisterRequest(owner_session_id="sid-x"))
        await handle_deregister(msg)
    release_mock.assert_called_once()
    reply = BootstrapDeregisterResponse().parse(msg.respond.call_args[0][0])
    assert reply.success is True


async def test_handle_deregister_oms_skips_instance_id_release():
    session = {"logical_id": "oms1", "instance_type": "OMS"}
    release_mock = AsyncMock()
    with (
        patch("zk_pilot.bootstrap.db.get_session", AsyncMock(return_value=session)),
        patch("zk_pilot.bootstrap.db.deregister_session", AsyncMock()),
        patch("zk_pilot.bootstrap.db.release_instance_id", release_mock),
    ):
        _, handle_deregister = make_handlers(make_pg_pool(AsyncMock()), Config(), MagicMock())
        msg = make_mock_msg(BootstrapDeregisterRequest(owner_session_id="sid-y"))
        await handle_deregister(msg)
    release_mock.assert_not_called()


# ── KvReconciler fake watcher ────────────────────────────────────────────────────


class FakeEntry:
    def __init__(self, key: str, operation: KeyValueOperation = KeyValueOperation.PUT):
        self.key = key
        self.operation = operation


class FakeWatcher:
    def __init__(self, events):
        self._events = iter(events)

    def __aiter__(self):
        return self

    async def __anext__(self):
        try:
            return next(self._events)
        except StopIteration:
            raise StopAsyncIteration


async def _run_reconciler(events, pg_pool=None, cfg=None) -> KvReconciler:
    """Run KvReconciler.run() for one watch cycle then terminate via CancelledError."""
    if cfg is None:
        cfg = Config()
    if pg_pool is None:
        pg_pool = MagicMock()
    r = KvReconciler(pg_pool, cfg)

    nc = MagicMock()
    js = MagicMock()
    kv = MagicMock()
    # First call: fake events; second call: CancelledError exits the while-True loop.
    kv.watchall = AsyncMock(side_effect=[FakeWatcher(events), asyncio.CancelledError()])
    js.key_value = AsyncMock(return_value=kv)
    nc.jetstream.return_value = js

    try:
        await r.run(nc)
    except asyncio.CancelledError:
        pass
    return r


# ── KvReconciler tests ───────────────────────────────────────────────────────────


async def test_reconciler_initial_snapshot_populates_live_set():
    r = await _run_reconciler([FakeEntry("k1"), FakeEntry("k2"), None])
    assert r._live == {"k1", "k2"}
    assert r._ready.is_set()


async def test_reconciler_live_put_adds_key_after_snapshot():
    r = await _run_reconciler([FakeEntry("k1"), None, FakeEntry("k3")])
    assert "k1" in r._live
    assert "k3" in r._live


async def test_reconciler_live_delete_removes_key_and_calls_on_kv_lost():
    r = await _run_reconciler([FakeEntry("k1"), FakeEntry("k2"), None])
    r._on_kv_lost = AsyncMock()

    nc = MagicMock()
    js = MagicMock()
    kv = MagicMock()
    kv.watchall = AsyncMock(
        side_effect=[
            FakeWatcher([FakeEntry("k1"), FakeEntry("k2"), None, FakeEntry("k1", KeyValueOperation.DEL)]),
            asyncio.CancelledError(),
        ]
    )
    js.key_value = AsyncMock(return_value=kv)
    nc.jetstream.return_value = js

    try:
        await r.run(nc)
    except asyncio.CancelledError:
        pass

    assert "k1" not in r._live
    r._on_kv_lost.assert_called_with("k1")


async def test_reconciler_reconnect_rebuilds_live_set():
    """Second snapshot contains only k2 — k1 should be dropped."""
    r = KvReconciler(MagicMock(), Config())
    r._live = {"k1", "k2"}
    r._ready.set()

    nc = MagicMock()
    js = MagicMock()
    kv = MagicMock()
    kv.watchall = AsyncMock(
        side_effect=[FakeWatcher([FakeEntry("k2"), None]), asyncio.CancelledError()]
    )
    js.key_value = AsyncMock(return_value=kv)
    nc.jetstream.return_value = js

    r._on_kv_lost = AsyncMock()
    try:
        await r.run(nc)
    except asyncio.CancelledError:
        pass

    assert r._live == {"k2"}


async def test_reconciler_reconnect_fences_vanished_keys():
    """Keys present before reconnect but absent from new snapshot get fenced."""
    r = KvReconciler(MagicMock(), Config())
    r._live = {"k1", "k2"}
    r._ready.set()
    r._on_kv_lost = AsyncMock()

    nc = MagicMock()
    js = MagicMock()
    kv = MagicMock()
    kv.watchall = AsyncMock(
        side_effect=[FakeWatcher([FakeEntry("k2"), None]), asyncio.CancelledError()]
    )
    js.key_value = AsyncMock(return_value=kv)
    nc.jetstream.return_value = js

    try:
        await r.run(nc)
    except asyncio.CancelledError:
        pass

    r._on_kv_lost.assert_called_once_with("k1")
