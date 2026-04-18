"""Tests for OANDA normalization functions."""

from oanda import normalize as norm

# Proto stubs for deserializing payload_bytes
from zk.exch_gw.v1 import exch_gw_pb2 as gw_pb


def _deserialize(event: dict) -> gw_pb.OrderReport:
    """Deserialize payload_bytes from an order_report event."""
    assert event["event_type"] == "order_report"
    assert "payload_bytes" in event
    report = gw_pb.OrderReport()
    report.ParseFromString(event["payload_bytes"])
    return report


class TestNormalize:
    def test_order_status_mapping(self):
        assert norm.order_status("PENDING") == "booked"
        assert norm.order_status("FILLED") == "filled"
        assert norm.order_status("CANCELLED") == "cancelled"
        assert norm.order_status("UNKNOWN") == "rejected"

    def test_order_status_enum(self):
        assert norm.order_status_enum("PENDING") == gw_pb.EXCH_ORDER_STATUS_BOOKED
        assert norm.order_status_enum("FILLED") == gw_pb.EXCH_ORDER_STATUS_FILLED
        assert norm.order_status_enum("CANCELLED") == gw_pb.EXCH_ORDER_STATUS_CANCELLED
        assert norm.order_status_enum("UNKNOWN") == gw_pb.EXCH_ORDER_STATUS_EXCH_REJECTED

    def test_extract_client_order_id(self):
        assert norm.extract_client_order_id({"clientExtensions": {"id": "42"}}) == 42
        assert norm.extract_client_order_id({"clientExtensions": {"id": "abc"}}) == 0
        assert norm.extract_client_order_id({}) == 0

    def test_order_create_event(self):
        txn = {
            "id": "100",
            "instrument": "EUR_USD",
            "units": "1000",
            "price": "1.1050",
            "clientExtensions": {"id": "42"},
        }
        event = norm.order_create_event(txn)
        report = _deserialize(event)
        assert report.exch_order_ref == "100"
        assert report.order_id == 42
        assert len(report.order_report_entries) == 2
        linkage = report.order_report_entries[0]
        assert linkage.report_type == gw_pb.ORDER_REP_TYPE_LINKAGE
        assert linkage.order_id_linkage_report.exch_order_ref == "100"
        assert linkage.order_id_linkage_report.order_id == 42
        state = report.order_report_entries[1]
        assert state.report_type == gw_pb.ORDER_REP_TYPE_STATE
        assert state.order_state_report.exch_order_status == gw_pb.EXCH_ORDER_STATUS_BOOKED
        assert state.order_state_report.unfilled_qty == 1000.0

    def test_order_fill_event(self):
        txn = {
            "id": "101",
            "orderID": "100",
            "instrument": "EUR_USD",
            "units": "1000",
            "price": "1.1050",
            "clientExtensions": {"id": "42"},
        }
        event = norm.order_fill_event(txn)
        report = _deserialize(event)
        assert report.exch_order_ref == "100"
        assert report.order_id == 42
        state_entry = report.order_report_entries[0]
        assert state_entry.order_state_report.exch_order_status == gw_pb.EXCH_ORDER_STATUS_FILLED
        assert state_entry.order_state_report.filled_qty == 1000.0
        trade_entry = report.order_report_entries[1]
        assert trade_entry.report_type == gw_pb.ORDER_REP_TYPE_TRADE
        assert trade_entry.trade_report.exch_trade_id == "101"
        assert trade_entry.trade_report.filled_qty == 1000.0
        assert trade_entry.trade_report.filled_price == 1.1050

    def test_order_cancel_event(self):
        txn = {"id": "102", "orderID": "100", "clientExtensions": {"id": "42"}}
        event = norm.order_cancel_event(txn)
        report = _deserialize(event)
        assert report.exch_order_ref == "100"
        entry = report.order_report_entries[0]
        assert entry.order_state_report.exch_order_status == gw_pb.EXCH_ORDER_STATUS_CANCELLED

    def test_order_reject_event(self):
        txn = {"clientExtensions": {"id": "42"}, "rejectReason": "INSUFFICIENT_MARGIN"}
        event = norm.order_reject_event(txn)
        report = _deserialize(event)
        assert report.order_id == 42
        entry = report.order_report_entries[0]
        assert entry.report_type == gw_pb.ORDER_REP_TYPE_EXEC
        assert entry.exec_report.exec_type == gw_pb.EXCH_EXEC_TYPE_REJECTED
        assert entry.exec_report.exec_message == "INSUFFICIENT_MARGIN"

    def test_order_state_event(self):
        event = norm.order_state_event(
            exch_order_ref="100",
            order_id=42,
            status=gw_pb.EXCH_ORDER_STATUS_FILLED,
            filled_qty=1000.0,
            unfilled_qty=0.0,
            avg_price=1.1050,
            instrument="EUR_USD",
        )
        report = _deserialize(event)
        assert report.exch_order_ref == "100"
        entry = report.order_report_entries[0]
        assert entry.order_state_report.exch_order_status == gw_pb.EXCH_ORDER_STATUS_FILLED

    def test_normalize_balance(self):
        summary = {
            "account": {
                "currency": "USD",
                "balance": "100000",
                "marginAvailable": "95000",
                "marginUsed": "5000",
            }
        }
        facts = norm.normalize_balance(summary)
        assert len(facts) == 1
        assert facts[0]["asset"] == "USD"
        assert facts[0]["total_qty"] == 100000.0

    def test_normalize_positions(self):
        resp = {
            "positions": [
                {
                    "instrument": "EUR_USD",
                    "long": {"units": "1000"},
                    "short": {"units": "-500"},
                },
                {
                    "instrument": "GBP_USD",
                    "long": {"units": "0"},
                    "short": {"units": "0"},
                },
            ]
        }
        facts = norm.normalize_positions(resp, account_id=9002)
        assert len(facts) == 2  # EUR_USD long + short, GBP_USD skipped
        instruments = {f["instrument"] for f in facts}
        assert instruments == {"EUR_USD"}
        # All OANDA positions are CFDs (instrument_type=4)
        assert all(f["instrument_type"] == 4 for f in facts)

    def test_normalize_trades(self):
        resp = {
            "trades": [
                {"id": "500", "instrument": "EUR_USD", "currentUnits": "1000", "price": "1.1050"},
            ]
        }
        facts = norm.normalize_trades(resp)
        assert len(facts) == 1
        assert facts[0]["exch_trade_id"] == "500"
        assert facts[0]["buysell_type"] == 1  # positive units = buy
