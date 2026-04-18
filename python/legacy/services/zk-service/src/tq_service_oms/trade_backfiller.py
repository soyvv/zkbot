from dataclasses import dataclass
import traceback
from datetime import datetime

import betterproto
from pymongo import MongoClient
import nats
from loguru import logger

from zk_proto_betterproto import tqrpc_exch_gw as gw_rpc, oms
from zk_proto_betterproto import exch_gw

from .config import DBConfigLoader
from .utils.enrich_data_with_config import enrich_data_with_config


@dataclass
class AppConfig:
    nats_url: str = None
    mongo_url_base: str = None
    mongo_uri: str = None
    mongo_config_dbname: str = None
    mongo_dbname: str = None

@dataclass
class OrderRequest:
    exch_order_ref: str
    instrument_tq: str
    source_id: str = None

class TradeBackfiller:
    def __init__(self, app_config: AppConfig, logger=logger):
        self.nats_url = app_config.nats_url
        self.mongo_url = app_config.mongo_url_base + "/" + app_config.mongo_uri
        self.logger = logger

        self.account_details: dict[int, dict] = {}
        self.symbol_details: dict[str, dict] = {}
        self._resolve_db_config(app_config.mongo_config_dbname)

        self.mongo = MongoClient(self.mongo_url)
        self.trade_collection = self.mongo[app_config.mongo_dbname]['trade']
        self.fee_collection = self.mongo[app_config.mongo_dbname]['fee']

        self.nc = None

    def _resolve_db_config(self, mongo_config_dbname: str):
        db_config_loader = DBConfigLoader(mongo_url=self.mongo_url, db_name=mongo_config_dbname)
        self.account_details = db_config_loader.load_account_details()
        self.symbol_details = db_config_loader.load_instrument_details()
        self.logger.info(f'account_details: {self.account_details}')
        self.logger.info(f"symbol_details: {self.symbol_details}")

    async def connect(self):
        self.nc = await nats.connect(self.nats_url)

    async def disconnect(self):
        if self.nc:
            await self.nc.close()
        self.mongo.close()

    def _convert_ts_to_datetime(self, ts: int):
        return datetime.fromtimestamp(float(ts) / 1000)

    async def query_order_trades(self, endpoint: str, exch_order_ref: str, instrument_exch: str):
        request = gw_rpc.ExchQueryOrderTradesRequest(exch_order_ref=exch_order_ref, 
                                                     instrument_exch=instrument_exch)
        message = await self.nc.request(
            subject=endpoint,
            headers={"rpc_method": "QueryOrderTrades"},
            payload=bytes(request),
            timeout=2
        )
        order_trades = gw_rpc.ExchOrderTradesResponse().parse(message.data)
        return order_trades
    
    def convert_trade_report(self, 
                             order_trades: gw_rpc.ExchOrderTradesResponse,
                             trade: gw_rpc.ExchTrade,
                             instrument_tq: str,
                             account_id: int,
                             source_id: str):
        trade_report = trade.trade_report
        trade_data = {'order_id': order_trades.order_id,
                      'ext_order_ref': order_trades.exch_order_ref,
                      'ext_trade_id': trade_report.exch_trade_id,
                      'filled_qty': trade_report.filled_qty,
                      'filled_price': trade_report.filled_price,
                      'filled_ts': str(trade_report.filled_ts),
                      'buy_sell_type': order_trades.to_dict()['buysellType'],
                      'fill_type': trade_report.fill_type,
                      'instrument': instrument_tq}
        trade_data['account_id'] = account_id
        trade_data['source_id'] = source_id

        data_from_config = enrich_data_with_config(instrument_tq=instrument_tq,
                                                   account_detail=self.account_details.get(account_id, None),
                                                   symbol_detail=self.symbol_details.get(instrument_tq, None))
        trade_data.update(data_from_config.__dict__)

        trade_data['timestamp'] = trade_report.filled_ts
        trade_data['datetime'] = self._convert_ts_to_datetime(trade_data['timestamp'])
        trade_data['record_datetime'] = datetime.utcnow()

        trade_data['is_backfill'] = True

        # trade_data['fee'] = [{'fee_symbol': fee.fee_symbol,
        #                       'fee_amount': fee.fee_qty} for fee in trade.fee_reports]

        fee_reports = [
            oms.Fee(
                fee_symbol=f.fee_symbol,
                fee_amount=f.fee_qty,
                fee_type=f.fee_type,
                fee_ts=f.fee_ts
            ).to_dict(casing=betterproto.Casing.SNAKE) for f in trade .fee_reports
        ]

        if len(fee_reports) > 0:
            trade_data['fee'] = fee_reports[0]

        if len(fee_reports) > 1:
            trade_data['fee_extra'] = fee_reports

        return trade_data

    def convert_fee_report(self, order_id: str, fee_report: exch_gw.FeeReport, account_id: int):
        fee_data = {'order_id': order_id,
                    'fee_symbol': fee_report.fee_symbol,
                    'fee_amount': fee_report.fee_qty,
                    'fee_type': fee_report.to_dict()['feeType']}

        fee_data['account_id'] = account_id

        account_detail = self.account_details.get(account_id, {})
        fee_data['account_name'] = account_detail.get('exch_account_id', None)
        fee_data['exch'] = account_detail.get('exch', None)

        fee_data['timestamp'] = fee_report.fee_ts
        fee_data['datetime'] = self._convert_ts_to_datetime(fee_data['timestamp'])
        fee_data['record_datetime'] = datetime.utcnow()
        return fee_data

    def convert_order_trades(self, 
                             order_trades: gw_rpc.ExchOrderTradesResponse,
                             instrument_tq: str,
                             account_id: int,
                             source_id: str):
        trade_reports = []
        fee_reports = []

        for trade in order_trades.exch_trades:
            trade_report = self.convert_trade_report(order_trades=order_trades, 
                                                     trade=trade, 
                                                     instrument_tq=instrument_tq, 
                                                     account_id=account_id, 
                                                     source_id=source_id)
            trade_reports.append(trade_report)

            for fee_report in trade.fee_reports:
                fee_report = self.convert_fee_report(order_id=order_trades.order_id, 
                                                     fee_report=fee_report, 
                                                     account_id=account_id)
                fee_reports.append(fee_report)
        return trade_reports, fee_reports

    def store_trades(self, account_id: int, order_id: str, exch_order_ref: str, trade_reports: list[dict], is_dry_run: bool):
        query_filter = {'account_id': account_id, 'order_id': order_id, 'ext_order_ref': exch_order_ref}
        existing_trades = list(self.trade_collection.find(query_filter))
        self.logger.info(f"Replacing trades: existing_count={len(existing_trades)}, new_count={len(trade_reports)}")
        for idx, trade in enumerate(trade_reports):
            self.logger.info(f'New trade [{idx}]: {trade}')

        if is_dry_run:
            self.logger.info('Skipping db writes due to dry run')
        else:
            self.trade_collection.delete_many(query_filter)
            self.trade_collection.insert_many(trade_reports)

    def store_fees(self, account_id: int, order_id: str, fee_reports: list[dict], is_dry_run: bool):
        query_filter = {'account_id': account_id, 'order_id': order_id, 'fee_type': 'FEE_EXCH'}
        existing_fees = list(self.fee_collection.find(query_filter))
        self.logger.info(f"Replacing fees: existing_count={len(existing_fees)}, new_count={len(fee_reports)}")
        for idx, fee in enumerate(fee_reports):
            self.logger.info(f'New fee [{idx}]: {fee}')

        if is_dry_run:
            self.logger.info('Skipping db writes due to dry run')
        else:
            self.fee_collection.delete_many(query_filter)
            self.fee_collection.insert_many(fee_reports)

    async def run(self, account_id: int, orders: list[OrderRequest], is_dry_run: bool):
        rpc_endpoint = self.account_details.get(account_id, {}).get('gw_detail', {}).get('rpc_endpoint')
        if not rpc_endpoint:
            self.logger.error(f"No rpc endpoint found: account_id={account_id}")
            return
 
        for order in orders:
            try:
                self.logger.info(f"Querying order trades: account_id={account_id}, order={order}")
                symbol_detail = self.symbol_details.get(order.instrument_tq)
                if not symbol_detail:
                    self.logger.error(f"No instrument found: instrument_code={order.instrument_tq}")
                    continue

                order_trades = await self.query_order_trades(rpc_endpoint, order.exch_order_ref, symbol_detail['exch_symbol'])
                self.logger.info(f"Received order trades: {order_trades}")

                trade_reports, fee_reports = self.convert_order_trades(order_trades=order_trades, 
                                                                        instrument_tq=order.instrument_tq, 
                                                                        account_id=account_id, 
                                                                        source_id=order.source_id)

                self.store_trades(account_id, order_trades.order_id, order_trades.exch_order_ref, trade_reports, is_dry_run)
                self.store_fees(account_id, order_trades.order_id, fee_reports, is_dry_run)
            except:
                self.logger.error(traceback.format_exc())
