import time
import v20
from zk_datamodel.common import InstrumentRefData, InstrumentType

#from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase


# OANDA API Credentials (replace with your own)
ACCESS_TOKEN = "d1a24a373d0281f91a8b2ad8399da060-87a87ac2a39083168d2d3b4854d86988"
ACCOUNT_ID = "101-003-26138765-001"
API_URL = "api-fxpractice.oanda.com"  # Or the live URL: https://api-fxtrade.oanda.com

class Oanda(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.client = v20.Context(
            hostname="api-fxpractice.oanda.com",  # OANDA practice environment
            token=ACCESS_TOKEN
        )

        self.exch_name = "OANDA"

    def fetch(self):
        records = []
        response = self.client.account.instruments(ACCOUNT_ID)

        instruments = response.get("instruments", 200)

        update_ts = int(time.time()*1000)

        for instrument in instruments:
            i_dict = instrument.dict()
            instrument_id_exch = i_dict['name']
            instrument_type = i_dict['type'] # CURRENCY, CFD, METAL

            base_asset, quote_asset = instrument_id_exch.split("_")

            pip_location = int(i_dict["pipLocation"])
            price_tick_size = 10 ** pip_location

            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, "-CFD", quote_asset, self.exch_name),
                instrument_id_exchange=instrument_id_exch,
                update_ts=update_ts,
                exchange_name=self.exch_name,
                instrument_type=InstrumentType.INST_TYPE_CFD,  # P perpetual
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset="USD",
                contract_size=1,  # default
                price_precision=-int(i_dict["pipLocation"]),
                qty_precision=int(i_dict["tradeUnitsPrecision"]),
                price_tick_size=price_tick_size,
                qty_lot_size=float(i_dict['minimumTradeSize']),
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=None,
                min_order_qty=float(i_dict['minimumTradeSize']),
                max_order_qty=float(i_dict['maximumOrderUnits']),
                extra_properties=dict(),
                original_info=str(instrument.json()),
                max_mkt_order_qty=None
            )

            records.append(instrument_ref_data)

        print(f"Total instruments fetched: {len(instruments)}")

        return records

if __name__ == "__main__":
    oanda = Oanda()
    print(oanda.fetch())