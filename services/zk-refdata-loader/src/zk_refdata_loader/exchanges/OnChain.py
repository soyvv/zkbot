
import time

from zk_datamodel.common import InstrumentRefData, InstrumentType

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase




import json

from typing import Dict, List, Optional

import requests
from cachetools import TTLCache, cached
from loguru import logger
from pydantic import BaseModel


class Token(BaseModel):
    name: Optional[str]
    symbol: str
    decimals: int
    address: str
    chain_id: int


class InstrumentClient:
    def __init__(self, instrument_url: str, logger=logger) -> None:
        self._instrument_url = instrument_url
        self.logger = logger

    def get_all_tokens(self) -> List[Token]:
        tokens = requests.get(
            f"{self._instrument_url}/evm/tokens/",
            timeout=1,
        ).json()
        return [Token.parse_obj(token) for token in tokens]

    def get_chain_tokens(self, chain_id: int) -> List[Token]:
        r = requests.get(
            f"{self._instrument_url}/evm/chains/{chain_id}/tokens/",
            timeout=1,
        )
        self.logger.info(f"Fetching tokens for chain {r.url}")
        tokens = r.json()
        return [Token.parse_obj(token) for token in tokens]

    def get_chains(self) -> List[dict]:
        r = requests.get(f"{self._instrument_url}/evm/chains/", timeout=1)
        chains = r.json()
        return chains

    def get_chain_ids(self) -> List[int]:
        chains = self.get_chains()
        return [chain["chain_id"] for chain in chains]

    def get_chain_names_map(self) -> List[int]:
        chains = self.get_chains()
        return {chain["chain_id"]: chain["chain"] for chain in chains}

    @cached(TTLCache(maxsize=1, ttl=60))
    def get_all_tokens_symbol_address(self) -> Dict[str, str]:
        chain_ids = self.get_chain_ids()
        """ In case a same token exists on multiple chains, Prioritize the address on the chain with a lower id """
        chain_ids.sort(reverse=True)

        tokens: list[Token] = []
        for chain_id in chain_ids:
            tokens += self.get_chain_tokens(chain_id)
        return {token.symbol: (token.address, token.chain_id) for token in tokens}


def get_all_pairs():
    base_url = "https://prod.stream.api.intra.tokkalabs.com/instrument-api"

    ic = InstrumentClient(base_url)
    chain_ids = ic.get_chain_ids()
    print(chain_ids)

    STABLES = [
        "DAI",
        "USDT",
        "USDC",
        "USDT.e",
        "USDC.e",
        "USDT.ETH",
        "USDC.ETH",
        "USDT.BSC",
        "USDC.BSC",
        "USDbC",
    ]
    STABLES_SET = set(STABLES)
    chain_names_map = ic.get_chain_names_map()
    pairs = []
    for chain_id in chain_ids:
        if chain_id in {123123}:
            continue
        tokens = ic.get_chain_tokens(chain_id)
        token_symbols = set(token.symbol for token in tokens)
        stables_of_chain = token_symbols & STABLES_SET
        print('stables of chian')
        print(stables_of_chain)

        alt_coins = token_symbols - STABLES_SET
        for alt in alt_coins:
            for stable in stables_of_chain:
                pair = {
                    "name": f"{alt}/{stable}",
                    "symbol": f"{alt}{stable}",
                    "base": alt,
                    "quote": stable,
                    "chain": chain_names_map[chain_id],
                    "binancePerpSymbol": f"{alt}USDT",
                }
                pairs.append(pair)

    return pairs


class OnChain(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        rfq_pairs = get_all_pairs()

        update_ts = int(time.time() * 1000)
        records = []
        for item in rfq_pairs:
            '''
            {
                "name": "WETH/USDC.e",
                "symbol": "WETH/USDC.e",
                "base": "WETH",
                "quote": "USDC.e",
                "chain": "optimism",
                "binancePerpSymbol": "WETHUSDT"
            }
            '''
            base_asset = item['base']
            quote_asset = item['quote']
            settlement_asset = None
            type_suffix = ""
            exch_name = item['chain'].upper()
            exch_symbol = item['symbol']
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_code=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=exch_symbol,
                update_ts=update_ts,
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_SPOT,  # S spot
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset=None,
                contract_size=1,
                price_precision=None,
                qty_precision=None,
                price_tick_size=None,
                qty_lot_size=None,
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=None,
                min_order_qty=None,
                max_order_qty=None,
                extra_properties={},
                original_info=str(item)
            )
            records.append(instrument_ref_data)

        return records
