from loguru import logger
import traceback
import pymongo
import time
import json

from zk_proto_betterproto.common import InstrumentRefData, InstrumentType
import betterproto

def _enrich_record(record: InstrumentRefData) -> InstrumentRefData:
    record.update_ts = int(time.time() * 1000)
    record.price_precision = int(record.price_precision) if record.price_precision else 0
    record.qty_precision = int(record.qty_precision) if record.qty_precision else 0
    record.contract_size = float(record.contract_size) if record.contract_size else 1
    record.min_price = float(record.min_price) if record.min_price else 0
    record.max_price = float(record.max_price) if record.max_price else 1_000_000
    record.min_order_qty = float(record.min_order_qty) if record.min_order_qty else 0
    record.max_order_qty = float(record.max_order_qty) if record.max_order_qty else 1_000_000
    record.min_notional = float(record.min_notional) if record.min_notional else 0
    record.max_notional = float(record.max_notional) if record.max_notional else 0
    record.max_mkt_order_qty = float(record.max_mkt_order_qty) if record.max_mkt_order_qty else 0
    record.extra_properties = {str(k): str(v) if not isinstance(v, str) else v for k, v in record.extra_properties.items()} if record.extra_properties else dict()
    return record



def load(exchange_names: list, 
         mongo_url: str,
         mongo_dbname: str, 
         mongo_collection: str,
         environment: str):
    records = list()
    for exchange in exchange_names:
        try:
            exch_loader = getattr(__import__(f"zk_refdata_loader.exchanges.{exchange}", fromlist=[exchange]), exchange)
            exch_loader_instance = exch_loader()
            new_records = exch_loader_instance.fetch()
            logger.info(f"Loaded {len(new_records)} new records for exchange {exchange}")
            new_records = [_enrich_record(record) for record in new_records]
            records.extend(new_records)
        except Exception as e:
            logger.exception(f"Error loading {exchange} records: {e}")
            print(f"Error loading {exchange} records: {traceback.format_exc()}")

    write_to_db(mongo_url=mongo_url, mongo_dbname=mongo_dbname, mongo_collection=mongo_collection, records=records)

    return len(records)


def write_to_db(mongo_url: str, mongo_dbname: str, mongo_collection: str, records: list[InstrumentRefData]):
    client = pymongo.MongoClient(mongo_url)
    db = client[mongo_dbname]
    collection = db[mongo_collection]

    for record in records:
        record_dict = record.to_dict(
            casing=betterproto.Casing.SNAKE,
            include_default_values=True)
        
        for field, field_type in InstrumentRefData.__annotations__.items():
            if field_type == int and field in record_dict:
                try:
                    record_dict[field] = int(record_dict[field])
                except Exception as e:
                    logger.exception(f"Error converting {field} with value {record_dict[field]} to int: {e}")
        
        collection.update_one({'instrument_id': record.instrument_id},
                              {"$set": record_dict},
                              upsert=True)


def debug_clear_db_collection(mongo_url: str, mongo_dbname: str, mongo_collection: str):
    client = pymongo.MongoClient(mongo_url)
    db = client[mongo_dbname]
    collection = db[mongo_collection]
    collection.delete_many({})
    logger.info("DEBUG: cleared local db")


if __name__ == "__main__":
    
    EXCHANGE_NAMES = [
        "Oanda",
        # "Binancedm",
        # "Kucoin",
        # "Kucoindm",
        # "Okx",
        # "Bluefin",
        # "BluefinSui",
        # "Injective",
        # "Dexalot",
        # "Dydx",
        # "Hyperliquid",
        # "Paradex",
        # "Vertex"
    ]
    MONGO_URL = "mongodb://p3dev:27018/" #Change this to your mongo url to test
    MONGO_DBNAME = "zklab-trading"
    MONGO_COLLECTION = "refdata_instrument"
    
    #debug_clear_db_collection(MONGO_URL, MONGO_DBNAME, MONGO_COLLECTION)
    record_count = load(EXCHANGE_NAMES, MONGO_URL, MONGO_DBNAME, MONGO_COLLECTION, "uat")

    logger.info(f"Loaded {record_count} records in total.")
