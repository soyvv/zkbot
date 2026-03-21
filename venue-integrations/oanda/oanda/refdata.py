"""OANDA refdata loader.

Implements VenueLoader contract for the OANDA v20 API.
Loaded by ``zk-refdata-svc`` through the manifest-driven Python bridge.
"""

from __future__ import annotations

from datetime import datetime, timezone

from loguru import logger

from .client import OandaRestClient

_API_URLS = {
    "practice": "https://api-fxpractice.oanda.com",
    "live": "https://api-fxtrade.oanda.com",
}

# OANDA type → canonical type suffix
_TYPE_SUFFIX = "-CFD"
_INSTRUMENT_TYPE = "CFD"
_VENUE = "OANDA"


class OandaRefdataLoader:
    """OANDA refdata loader for the venue manifest bridge.

    Config keys:
        environment: "practice" or "live"
        account_id: OANDA account ID string
        token: API bearer token (resolved from secret_ref by the host)
        api_base_url: (optional) override REST base URL
    """

    def __init__(self, config: dict | None = None) -> None:
        cfg = config or {}
        env = cfg.get("environment", "practice")
        self._account_id = cfg.get("account_id", "")
        self._token = cfg.get("token") or cfg.get("secret_ref", "")
        self._api_base_url = cfg.get("api_base_url") or _API_URLS.get(env, _API_URLS["practice"])

    async def load_instruments(self) -> list[dict]:
        """Fetch OANDA instruments and return canonical refdata dicts."""
        client = OandaRestClient(
            api_base_url=self._api_base_url,
            token=self._token,
            account_id=self._account_id,
        )
        try:
            resp = await client.get_instruments()
        finally:
            await client.close()

        instruments = resp.get("instruments", [])
        records: list[dict] = []

        for inst in instruments:
            name = inst.get("name", "")  # e.g. "EUR_USD"
            parts = name.split("_", 1)
            if len(parts) != 2:
                logger.warning(f"Skipping instrument with unexpected name format: {name}")
                continue

            base_asset, quote_asset = parts
            pip_location = inst.get("pipLocation", -4)
            price_precision = -pip_location
            price_tick_size = 10.0 ** pip_location
            trade_units_precision = inst.get("tradeUnitsPrecision", 0)
            min_trade_size = inst.get("minimumTradeSize", "1")
            max_order_units = inst.get("maximumOrderUnits", "0")

            instrument_id = f"{base_asset}{_TYPE_SUFFIX}/{quote_asset}@{_VENUE}"

            extra: dict = {}
            oanda_type = inst.get("type", "")
            if oanda_type:
                extra["oanda_type"] = oanda_type
            margin_rate = inst.get("marginRate")
            if margin_rate is not None:
                extra["margin_rate"] = margin_rate
            financing = inst.get("financing")
            if financing:
                extra["financing"] = financing

            records.append({
                "instrument_id": instrument_id,
                "instrument_exch": name,
                "venue": _VENUE,
                "instrument_type": _INSTRUMENT_TYPE,
                "base_asset": base_asset,
                "quote_asset": quote_asset,
                "settlement_asset": None,
                "contract_size": 1.0,
                "price_precision": price_precision,
                "qty_precision": trade_units_precision,
                "price_tick_size": price_tick_size,
                "qty_lot_size": float(min_trade_size),
                "min_notional": None,
                "max_notional": None,
                "min_order_qty": float(min_trade_size),
                "max_order_qty": float(max_order_units) if max_order_units != "0" else None,
                "max_mkt_order_qty": None,
                "extra_properties": extra if extra else None,
                "disabled": False,
            })

        logger.info(f"OANDA refdata loaded {len(records)} instruments")
        return records

    async def load_market_sessions(self) -> list[dict]:
        """Return FX/CFD market session state based on current UTC time.

        OANDA FX is roughly 24x5: Sunday ~21:00 UTC → Friday ~21:00 UTC.
        This is a simplified model; actual hours vary by instrument.
        """
        now = datetime.now(timezone.utc)
        weekday = now.weekday()  # 0=Mon ... 6=Sun
        hour = now.hour

        # Closed: Saturday all day, Sunday before 21:00 UTC, Friday after 21:00 UTC
        if weekday == 5:  # Saturday
            state = "closed"
        elif weekday == 6 and hour < 21:  # Sunday before market open
            state = "closed"
        elif weekday == 4 and hour >= 21:  # Friday after market close
            state = "closed"
        else:
            state = "open"

        return [
            {"venue": _VENUE, "market": "FX", "session_state": state},
            {"venue": _VENUE, "market": "CFD", "session_state": state},
        ]
