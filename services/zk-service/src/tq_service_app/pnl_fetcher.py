from abc import abstractmethod
from dataclasses import dataclass
from datetime import datetime
import json
from nats.aio.client import Client as NatsClient
import requests
import logging

from tq_service_app.utils import fetch_balance, sanitiseJsonString

logging.basicConfig(level=logging.WARNING)

@dataclass
class Pnl:
    asset: str = None
    before_settlement: float = None
    unsettled: float = None
    after_settlement: float = None
    details: list[dict] = None

class PnlFetcher:

    @abstractmethod
    def fetch_pnl(self) -> list[Pnl]:
        pass

class ParadexPnlFetcher(PnlFetcher):
    nc: NatsClient = None
    asset_symbol = 'USDC'

    def __init__(self, nc: NatsClient) -> None:
        self.nc = nc
        super().__init__()

    async def fetch_pnl(self, oms_id: str, account_id: int) -> list[Pnl]:
        balance_resp = await fetch_balance(self.nc, oms_id, account_id)
        balances = balance_resp.account_balance_entries

        pnl = Pnl(asset=self.asset_symbol)
        total = [balance for balance in balances if balance.instrument_code == self.asset_symbol]
        if len(total) == 1:
            raw_data: dict = json.loads(sanitiseJsonString(total[0].exch_data_raw))
            price = float(raw_data.get('settlement_asset_price', 0))
            pnl.before_settlement = float(raw_data.get('settlement_asset_balance_before', 0)) * price
            pnl.after_settlement = float(raw_data.get('settlement_asset_balance_after', 0)) * price
            pnl.unsettled= pnl.after_settlement - pnl.before_settlement
        pnl.details = [
            {
                "symbol": balance.instrument_code, 
                "unsettled": json.loads(sanitiseJsonString(balance.exch_data_raw)).get('unrealized_pnl', None)
            } for balance in balances if balance.instrument_code != self.asset_symbol
        ]
        return [pnl]

class BinancePnlFetcher(PnlFetcher):
    nc: NatsClient = None
    asset_symbol = 'USDT'

    def __init__(self, nc: NatsClient) -> None:
        self.nc = nc
        super().__init__()
    
    async def fetch_pnl(self, oms_id: str, account_id: int) -> list[Pnl]:
        balance_resp = await fetch_balance(self.nc, oms_id, account_id)
        balances = balance_resp.account_balance_entries

        pnl = Pnl(asset=self.asset_symbol)
        total = [balance for balance in balances if balance.instrument_code == self.asset_symbol]
        if len(total) == 1:
            pnl.before_settlement = total[0].avail_qty
            pnl.after_settlement = total[0].total_qty
            pnl.unsettled = pnl.after_settlement -pnl.before_settlement
        pnl.details = [
            {
                "symbol": balance.instrument_code, 
                "unsettled": json.loads(sanitiseJsonString(balance.exch_data_raw)).get('info', {}).get('unRealizedProfit', None)
            } for balance in balances if balance.instrument_code != self.asset_symbol
        ]
        return [pnl]
    
class VertexPnlFetcher(PnlFetcher):
    asset_symbol = 'USDC'
    mainnet_url = 'https://prod.vertexprotocol-backend.com'
    subaccount_mapping = {
        7702: '0xe98f4c6dc906f82ee3f38e174e88174e42fba33e64656661756c740000000000'
    }

    def __init__(self) -> None:
        super().__init__()
    
    async def fetch_pnl(self, account_id: int) -> list[Pnl]:
        subaccount = self.subaccount_mapping.get(account_id)
        if subaccount is None:
            return []

        epoch = int(datetime.now().strftime('%s'))
        payload = json.dumps({"summary": {"subaccount":subaccount, "timestamp": [epoch]}})
        response = requests.request(
            "POST", 
            f'{self.mainnet_url}/indexer', 
            headers={'Content-Type': 'application/json'}, 
            data=payload
        )
        balances: list[dict] = json.loads(response.text)['events'][str(epoch)]

        symbols: list[dict] = requests.get(f'{self.mainnet_url}/symbols').json()
        symbols_map = {symbol['product_id']: symbol['symbol'] for symbol in symbols}

        pnl = Pnl(asset=self.asset_symbol, details=[])
        for balance in balances:
            if 'perp' in balance['post_balance'].keys():
                product_id = balance['product_id']
                symbol = symbols_map.get(product_id)
                unsettled = (float(balance['post_balance']['perp']['balance']['v_quote_balance']) 
                                + float(balance['product']['perp']['oracle_price_x18']) / (1000000000000000000 * 1) 
                                    * float(balance['post_balance']['perp']['balance']['amount'])) / 1000000000000000000
                pnl.details.append({'product_id': product_id, 'symbol': symbol, 'unsettled': unsettled})

        pnl.unsettled = sum([unsettled['unsettled'] for unsettled in pnl.details])
        total = [balance for balance in balances if balance['product_id'] == 0]
        if len(total) == 1:
            pnl.before_settlement = int(total[0]['post_balance']['spot']['balance']['amount']) / 1000000000000000000
            pnl.after_settlement = pnl.before_settlement + pnl.unsettled

        return [pnl]
