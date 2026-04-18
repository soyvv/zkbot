import requests
import logging

from abc import ABC, abstractmethod
import zk_proto_betterproto.common as common

class ExchangeBase(ABC):
    def __init__(self, **kwargs):
        pass

    @abstractmethod
    def fetch(self) -> list[common.InstrumentRefData]:
        pass

    def request(self, url, headers=None, json=None, method="GET"):
        failure_count = 0
        while True:
            try:
                msg = requests.request(
                    method=method,
                    url=url,
                    headers=headers if headers else {},
                    json=json
                ).json()
                return msg
            except Exception as e:
                failure_count += 1
                logging.exception(
                    f"Exchange {self.__class__.__name__} url request failure -> msg: {e}, url: {url}")
            
            if failure_count >= 20:
                return None

    def instrument_id(self, base_asset:str, type_suffix:str, quote_asset:str, exch_name:str):
        return f"{base_asset}{type_suffix}/{quote_asset}@{exch_name}"
    
    def instrument_id_ccxt(self, base_asset:str, quote_asset:str, settlement_asset:str):
        return f"{base_asset}/{quote_asset}:{settlement_asset}" if settlement_asset else f"{base_asset}/{quote_asset}"