import time
from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentRefData, InstrumentType

ASSETS = [
        {
            'chainflipId': 'Eth',
            'asset': 'ETH',
            'chain': 'Ethereum',
            'contractAddress': None,
            'decimals': 18,
            'name': 'Ether',
            'symbol': 'ETH',
            'isMainnet': False,
            'minimumSwapAmount': '0',
            'maximumSwapAmount': None,
            'minimumEgressAmount': '1'
        },
        {
            'chainflipId': 'Flip',
            'asset': 'FLIP',
            'chain': 'Ethereum',
            'contractAddress': '0xdC27c60956cB065D19F08bb69a707E37b36d8086',
            'decimals': 18,
            'name': 'FLIP',
            'symbol': 'FLIP',
            'isMainnet': False,
            'minimumSwapAmount': '0',
            'maximumSwapAmount': None,
            'minimumEgressAmount': '1'
        },
        {
            'chainflipId': 'Usdt',
            'asset': 'USDT',
            'chain': 'Ethereum',
            'contractAddress': '0x27CEA6Eb8a21Aae05Eb29C91c5CA10592892F584',
            'decimals': 6,
            'name': 'USDT',
            'symbol': 'USDT',
            'isMainnet': False,
            'minimumSwapAmount': '0',
            'maximumSwapAmount': None,
            'minimumEgressAmount': '1'
        },
        {
            'chainflipId': 'Btc',
            'asset': 'BTC',
            'chain': 'Bitcoin',
            'contractAddress': None,
            'decimals': 8,
            'name': 'Bitcoin',
            'symbol': 'BTC',
            'isMainnet': False,
            'minimumSwapAmount': '0',
            'maximumSwapAmount': None,
            'minimumEgressAmount': '600'
        },
        {
            'chainflipId': 'Dot',
            'asset': 'DOT',
            'chain': 'Polkadot',
            'contractAddress': None,
            'decimals': 10,
            'name': 'Polkadot',
            'symbol': 'DOT',
            'isMainnet': False,
            'minimumSwapAmount': '0',
            'maximumSwapAmount': None,
            'minimumEgressAmount': '1'
        }
    ]

def get_order_qty(value):
    if not value:
        return value
    return float(int(value))
class Chainflip(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        default_quote = "USDC"
                
        for asset in ASSETS:
            base_asset = asset["asset"]
            quote_asset =   default_quote if asset["asset"] != default_quote else None
            exch_symbol = f"{base_asset}/{quote_asset}"
            settlement_asset = None
            type_suffix = ""
            exch_name = "CHAINFLIP"
            instrument_ref_data = InstrumentRefData(
                instrument_code=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=exch_symbol,
                exch_symbol=exch_symbol,
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_SPOT, 
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset=settlement_asset,
                contract_size=1, # default
                price_precision=utils.int_precision(asset["decimals"]),
                qty_precision=utils.int_precision(asset["decimals"]),
                price_tick_size=1,
                qty_lot_size=1,
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=None,
                min_order_qty=get_order_qty(asset["minimumSwapAmount"]),
                max_order_qty=get_order_qty(asset["maximumSwapAmount"]),
                extra_properties=asset,
                original_info=str(asset)
            )
        
            records.append(instrument_ref_data)

        return records