
import sqlalchemy as sa
from sqlalchemy.ext.declarative import declarative_base

Base = declarative_base()
metadata = sa.MetaData()


class TQHistTrade(Base):
    __tablename__ = "tq_hist_trade"

    id = sa.Column(sa.Integer, primary_key=True)
    order_id = sa.Column(sa.BigInteger, nullable=False)
    ext_order_ref = sa.Column(sa.String(100), nullable=False)
    ext_trade_id = sa.Column(sa.String(100))
    filled_qty = sa.Column(sa.Numeric)
    filled_price = sa.Column(sa.Numeric)
    filled_ts = sa.Column(sa.BigInteger)
    realized_pnl = sa.Column(sa.Numeric)
    buy_sell_type = sa.Column(sa.String(20))
    fill_type = sa.Column(sa.String(20))
    instrument = sa.Column(sa.String(50))
    account_id = sa.Column(sa.Integer)
    source_id = sa.Column(sa.String(80))
    account_name = sa.Column(sa.String(80))
    exch = sa.Column(sa.String(30))
    instrument_mo = sa.Column(sa.String(50))
    instrument_exch = sa.Column(sa.String(50))
    instrument_type = sa.Column(sa.String(20))
    trade_ts = sa.Column(sa.TIMESTAMP)
    record_ts = sa.Column(sa.TIMESTAMP, default=sa.func.current_timestamp())
    fee_symbol = sa.Column(sa.String(20))
    fee_amount = sa.Column(sa.Numeric)
    fee_type = sa.Column(sa.String(20))
    is_backfilled = sa.Column(sa.Boolean, default=False)
    original_json = sa.Column(sa.JSON)


class TQHistExchTrade(Base):
    __tablename__ = "tq_hist_exch_trade"

    id = sa.Column(sa.Integer, primary_key=True)
    ext_order_ref = sa.Column(sa.String(100), nullable=False)
    client_order_id = sa.Column(sa.String(100))
    ext_trade_id = sa.Column(sa.String(100))
    filled_qty = sa.Column(sa.Numeric)
    filled_price = sa.Column(sa.Numeric)
    filled_ts = sa.Column(sa.BigInteger)
    buy_sell_type = sa.Column(sa.String(20))
    fill_type = sa.Column(sa.String(20))
    instrument_exch = sa.Column(sa.String(50))
    account_id = sa.Column(sa.Integer)
    account_name = sa.Column(sa.String(80))
    exch = sa.Column(sa.String(30))
    instrument_type = sa.Column(sa.String(20))
    trade_ts = sa.Column(sa.TIMESTAMP)
    record_ts = sa.Column(sa.TIMESTAMP, default=sa.func.current_timestamp())
    fee_symbol = sa.Column(sa.String(20))
    fee_amount = sa.Column(sa.Numeric)
    fee_type = sa.Column(sa.String(20))
    original_data = sa.Column(sa.JSON)




class TQHistFee(Base):
    __tablename__ = "tq_hist_fee"

    id = sa.Column(sa.Integer, primary_key=True)
    account_id = sa.Column(sa.Integer)
    account_name = sa.Column(sa.String(80))
    fee_symbol = sa.Column(sa.String(20))
    fee_amount = sa.Column(sa.Numeric)
    fee_type = sa.Column(sa.String(20))
    exch = sa.Column(sa.String(30))
    fee_ts = sa.Column(sa.TIMESTAMP)
    instrument = sa.Column(sa.String(50))
    instrument_mo = sa.Column(sa.String(50))
    instrument_exch = sa.Column(sa.String(50))
    order_id = sa.Column(sa.BigInteger)
    ext_order_ref = sa.Column(sa.String(100))
    ext_trade_id = sa.Column(sa.String(100))
    record_ts = sa.Column(sa.TIMESTAMP, default=sa.func.current_timestamp())
    original_data = sa.Column(sa.JSON)



class TQHistOrder(Base):
    __tablename__ = "tq_hist_order"

    id = sa.Column(sa.Integer, primary_key=True)
    order_id = sa.Column(sa.BigInteger)
    ext_order_ref = sa.Column(sa.String(100))
    account_id = sa.Column(sa.Integer)
    account_name = sa.Column(sa.String(80))
    exch = sa.Column(sa.String(30))
    instrument = sa.Column(sa.String(50))
    instrument_type = sa.Column(sa.String(20))
    buy_sell_type = sa.Column(sa.String(20))
    basic_order_type = sa.Column(sa.String(20))
    time_inforce_type = sa.Column(sa.String(20))
    order_qty = sa.Column(sa.Numeric)
    order_price = sa.Column(sa.Numeric)
    order_ts = sa.Column(sa.TIMESTAMP)
    order_status = sa.Column(sa.String(20))
    filled_qty = sa.Column(sa.Numeric)
    filled_avg_price = sa.Column(sa.Numeric)
    oms_id = sa.Column(sa.String(20))
    source_id = sa.Column(sa.String(80))
    last_update_ts = sa.Column(sa.TIMESTAMP)
    cancel_attempts = sa.Column(sa.Integer)
    order_snapshot = sa.Column(sa.JSON)
    gw_request = sa.Column(sa.JSON)
    oms_request = sa.Column(sa.JSON)
    oms_trades = sa.Column(sa.JSON)
    error_msgs = sa.Column(sa.JSON)
    record_ts = sa.Column(sa.TIMESTAMP, default=sa.func.current_timestamp())



class TQHistPosition(Base):
    __tablename__ = "tq_hist_position"
    id = sa.Column(sa.Integer, primary_key=True)
    account_id = sa.Column(sa.Integer)
    account_name = sa.Column(sa.String(80))
    exch = sa.Column(sa.String(30))
    instrument = sa.Column(sa.String(50))
    instrument_type = sa.Column(sa.String(20))
    side = sa.Column(sa.String(20))
    contract_size = sa.Column(sa.Numeric, default=1)
    pos_qty = sa.Column(sa.Numeric)
    unsettled_pnl = sa.Column(sa.Numeric)
    avg_entry_price = sa.Column(sa.Numeric)
    index_price = sa.Column(sa.Numeric)
    last_trade_price = sa.Column(sa.Numeric)
    leverage = sa.Column(sa.Numeric)
    liquidation_price = sa.Column(sa.Numeric)
    margin = sa.Column(sa.Numeric)
    instrument_mo = sa.Column(sa.String(50))
    instrument_exch = sa.Column(sa.String(50))
    sync_ts = sa.Column(sa.TIMESTAMP)
    update_ts = sa.Column(sa.TIMESTAMP)
    record_ts = sa.Column(sa.TIMESTAMP, default=sa.func.current_timestamp())
    original_data = sa.Column(sa.JSON)




class TQHistBalance(Base):
    __tablename__ = "tq_hist_balance"

    id = sa.Column(sa.Integer, primary_key=True)
    account_id = sa.Column(sa.Integer)
    account_name = sa.Column(sa.String(80))
    exch = sa.Column(sa.String(30))
    instrument = sa.Column(sa.String(50))
    instrument_type = sa.Column(sa.String(20))
    side = sa.Column(sa.String(20))
    total_qty = sa.Column(sa.Numeric)
    avail_qty = sa.Column(sa.Numeric)
    frozen_qty = sa.Column(sa.Numeric)
    instrument_mo = sa.Column(sa.String(50))
    instrument_exch = sa.Column(sa.String(50))
    sync_ts = sa.Column(sa.TIMESTAMP)
    update_ts = sa.Column(sa.TIMESTAMP)
    record_ts = sa.Column(sa.TIMESTAMP, default=sa.func.current_timestamp())

    original_data = sa.Column(sa.JSON)
