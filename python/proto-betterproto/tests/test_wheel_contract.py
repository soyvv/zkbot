"""Regression guard for the PyO3 backtester wheel's betterproto contract.

The compiled wheel ``zk_backtest`` (``rust/crates/zk-pyo3-rs``) deserializes every
Rust prost bar/event into a Python object via runtime imports of the form
``py.import_bound("zk_proto_betterproto.<pkg>").getattr("<Class>")`` (see
``adapter.rs`` / ``py_strategy.rs``). If that betterproto output is retired or its
flat-name surface drifts, the wheel fails silently and strategies receive no
bars/events (0 trades) — exactly the regression that commit ad7c9b2 introduced.

This test pins the (module, class) pairs the wheel imports so the breakage is loud.
Keep ``WHEEL_PROTO_IMPORTS`` in sync with the hardcoded strings in the Rust sources.
"""

from __future__ import annotations

import importlib

import pytest

# (flat betterproto module, class) pairs hardcoded in the PyO3 wheel.
# Source of truth: rust/crates/zk-pyo3-rs/src/{adapter.rs,py_strategy.rs}.
WHEEL_PROTO_IMPORTS = [
    ("zk_proto_betterproto.rtmd", "Kline"),
    ("zk_proto_betterproto.oms", "OrderUpdateEvent"),
    ("zk_proto_betterproto.oms", "BalanceUpdateEvent"),
    ("zk_proto_betterproto.oms", "PositionUpdateEvent"),
    ("zk_proto_betterproto.oms", "Position"),
    ("zk_proto_betterproto.common", "InstrumentRefData"),
]


@pytest.mark.parametrize("module_name, class_name", WHEEL_PROTO_IMPORTS)
def test_wheel_proto_imports_resolve(module_name: str, class_name: str) -> None:
    module = importlib.import_module(module_name)
    cls = getattr(module, class_name)
    # The wheel calls cls.FromString(prost_bytes); the betterproto round-trip must hold.
    assert hasattr(cls, "FromString")
    obj = cls.FromString(bytes(cls()))
    assert isinstance(obj, cls)


def test_flat_alias_is_versioned_class() -> None:
    """Flat-name modules must re-export the *same* class objects as the versioned
    namespace, so isinstance() checks in strategies match what the wheel constructs."""
    import zk_proto_betterproto.rtmd as flat
    import zk_proto_betterproto.zk.rtmd.v1 as versioned

    assert flat.Kline is versioned.Kline


def test_legacy_enum_aliases_present() -> None:
    """Strategies use full proto enum value names (ORDER_STATUS_*, EXEC_TYPE_*);
    betterproto strips the prefix, so the legacy-compat post-pass must add aliases."""
    import zk_proto_betterproto.oms as oms

    assert oms.OrderStatus.ORDER_STATUS_FILLED == oms.OrderStatus.FILLED
    assert oms.OrderStatus.ORDER_STATUS_CANCELLED == oms.OrderStatus.CANCELLED
    assert oms.OrderStatus.ORDER_STATUS_REJECTED == oms.OrderStatus.REJECTED
    assert oms.ExecType.EXEC_TYPE_CANCEL == oms.ExecType.CANCEL
