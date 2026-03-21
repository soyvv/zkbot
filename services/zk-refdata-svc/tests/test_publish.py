"""Tests for change event publishing."""

import json

import pytest

from zk_refdata_svc.jobs.publish_changes import publish_change_events


class MockNATS:
    def __init__(self):
        self.published: list[tuple[str, bytes]] = []

    async def publish(self, subject: str, payload: bytes) -> None:
        self.published.append((subject, payload))


async def test_publish_emits_to_correct_subject():
    nc = MockNATS()
    events = [
        {
            "instrument_id": "BTC/USDT@EX",
            "venue": "EX",
            "change_class": "added",
            "watermark_ms": 1_700_000_000_000,
        },
    ]
    await publish_change_events(nc, events)
    assert len(nc.published) == 1
    subject, _ = nc.published[0]
    assert subject == "zk.control.refdata.updated"


async def test_added_maps_to_invalidate():
    nc = MockNATS()
    events = [
        {
            "instrument_id": "BTC/USDT@EX",
            "venue": "EX",
            "change_class": "added",
            "watermark_ms": 123,
        },
    ]
    await publish_change_events(nc, events)
    payload = json.loads(nc.published[0][1])
    assert payload["change_class"] == "invalidate"


async def test_changed_maps_to_invalidate():
    nc = MockNATS()
    events = [
        {
            "instrument_id": "BTC/USDT@EX",
            "venue": "EX",
            "change_class": "changed",
            "watermark_ms": 123,
        },
    ]
    await publish_change_events(nc, events)
    payload = json.loads(nc.published[0][1])
    assert payload["change_class"] == "invalidate"


async def test_disabled_maps_to_deprecate():
    nc = MockNATS()
    events = [
        {
            "instrument_id": "BTC/USDT@EX",
            "venue": "EX",
            "change_class": "disabled",
            "watermark_ms": 123,
        },
    ]
    await publish_change_events(nc, events)
    payload = json.loads(nc.published[0][1])
    assert payload["change_class"] == "deprecate"


async def test_empty_events_no_publish():
    nc = MockNATS()
    await publish_change_events(nc, [])
    assert len(nc.published) == 0
