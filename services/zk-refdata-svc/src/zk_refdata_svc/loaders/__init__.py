"""Venue loaders registry."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader
from zk_refdata_svc.loaders.binance import Binance
from zk_refdata_svc.loaders.okx import Okx
from zk_refdata_svc.loaders.kucoin import Kucoin
from zk_refdata_svc.loaders.kucoindm import Kucoindm
from zk_refdata_svc.loaders.dydx import Dydx
from zk_refdata_svc.loaders.injective import Injective
from zk_refdata_svc.loaders.bluefin import Bluefin
from zk_refdata_svc.loaders.bluefin_sui import BluefinSui
from zk_refdata_svc.loaders.paradex import Paradex
from zk_refdata_svc.loaders.vertex import Vertex
from zk_refdata_svc.loaders.hyperliquid import Hyperliquid
from zk_refdata_svc.loaders.dexalot import Dexalot
from zk_refdata_svc.loaders.chainflip import Chainflip
from zk_refdata_svc.loaders.blitz import Blitz

VENUE_LOADERS: dict[str, type[VenueLoader]] = {
    "Binance": Binance,
    "Okx": Okx,
    "Kucoin": Kucoin,
    "Kucoindm": Kucoindm,
    "Dydx": Dydx,
    "Injective": Injective,
    "Bluefin": Bluefin,
    "BluefinSui": BluefinSui,
    "Paradex": Paradex,
    "Vertex": Vertex,
    "Hyperliquid": Hyperliquid,
    "Dexalot": Dexalot,
    "Chainflip": Chainflip,
    "Blitz": Blitz,
}

__all__ = [
    "VENUE_LOADERS",
    "VenueLoader",
    "Binance",
    "Okx",
    "Kucoin",
    "Kucoindm",
    "Dydx",
    "Injective",
    "Bluefin",
    "BluefinSui",
    "Paradex",
    "Vertex",
    "Hyperliquid",
    "Dexalot",
    "Chainflip",
    "Blitz",
]
