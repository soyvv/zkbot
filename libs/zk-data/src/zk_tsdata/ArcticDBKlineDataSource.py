from dataclasses import dataclass
from datetime import datetime
from typing import Optional
import pandas as pd
import arcticdb as adb
from urllib.parse import urlparse

from zk_strategy.datasource import KlineDataSource


#from tq_strategy.datasource import KlineDataSource



@dataclass
class ArcticdbDataSourceConfig:
    minio_auth_key: Optional[str] = None
    minio_secret_key: Optional[str] = None
    minio_url: Optional[str] = None
    minio_bucket_name: Optional[str] = None
    minio_dataset_name: Optional[str] = None

class ArcticdbKlineDataSource(KlineDataSource):

    @staticmethod
    def _build_arctic_s3_uri(
        *,
        minio_url: str,
        bucket_name: str,
        access_key: str,
        secret_key: str,
        region: str = "us-east-1",
    ) -> str:
        """Build an ArcticDB S3 URI for MinIO.

        Expected format:
          s3://ENDPOINT:BUCKET?port=9000&region=...&access=...&secret=...
        Note: the port is a query param, not part of ENDPOINT.
        """
        parsed = urlparse(minio_url)

        if parsed.scheme in {"s3", "s3s"}:
            endpoint = parsed.hostname or ""
            port = parsed.port
            scheme = parsed.scheme
        elif parsed.scheme in {"http", "https"}:
            endpoint = parsed.hostname or ""
            port = parsed.port
            scheme = "s3s" if parsed.scheme == "https" else "s3"
        else:
            raw = minio_url.replace("s3://", "").replace("s3s://", "")
            if ":" in raw:
                endpoint, port_str = raw.split(":", 1)
                port = int(port_str)
            else:
                endpoint, port = raw, None
            scheme = "s3"

        if not endpoint:
            raise ValueError(f"Invalid minio_url (missing host): {minio_url}")

        query_parts = [f"region={region}", f"access={access_key}", f"secret={secret_key}"]
        if port is not None:
            query_parts.insert(0, f"port={port}")

        return f"{scheme}://{endpoint}:{bucket_name}?" + "&".join(query_parts)

    def __init__(self, config: ArcticdbDataSourceConfig):
        super().__init__()
        self.config = config
        if not config.minio_auth_key or not config.minio_secret_key:
            raise ValueError("minio_auth_key/minio_secret_key must be provided")
        if not config.minio_url or not config.minio_bucket_name or not config.minio_dataset_name:
            raise ValueError("minio_url/minio_bucket_name/minio_dataset_name must be provided")

        minio_auth_key: str = config.minio_auth_key
        minio_secret_key: str = config.minio_secret_key
        minio_url: str = config.minio_url
        bucket_name: str = config.minio_bucket_name
        self.dataset_name: str = config.minio_dataset_name
        uri = self._build_arctic_s3_uri(
            minio_url=minio_url,
            bucket_name=bucket_name,
            access_key=minio_auth_key,
            secret_key=minio_secret_key,
        )
        self.ac = adb.Arctic(uri)
        self.library = self.ac[self.dataset_name]

    def get_data(self, symbol: str, start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        '''
        Get kline data from ArcticDB
        kline data format: index: DATETIME;
        columnes: ['TICKER', 'OPEN', 'HIGH', 'LOW', 'CLOSE', 'VOL'];

        target columns:
        class BAR_COLS(enum.Enum):
            kline_type = "kline_type" # 1m.
            open = "open"
            close = "close"
            high = "high"
            low = "low"
            volume = "volume"
            amount = "amount"
            timestamp = "timestamp"
            symbol = "symbol"
        '''

        try:
            # Connect to the dataset
            library = self.library

            # Query the data for the specified symbol and time range
            dt_range = (pd.to_datetime(start_dt), pd.to_datetime(end_dt))
            df = library.read(symbol, date_range=dt_range).data

            df = df.rename(
                columns={
                    'TICKER': 'symbol',
                    'OPEN': 'open',
                    'HIGH': 'high',
                    'LOW': 'low',
                    'CLOSE': 'close',
                    'VOL': 'volume'
                }
            )

            # Add additional columns if needed
            #df['timestamp'] = df.index - pd.Timedelta("1min")
            df['timestamp'] = df.index.astype(int) // 10 ** 6
            df['kline_end_timestamp'] = df['timestamp']
            df['kline_type'] = 'KLINE_1MIN'
            df['amount'] = .0

            print(df)

            # Ensure the resulting DataFrame has the expected structure
            required_columns = [
                'kline_type', 'open', 'close', 'high', 'low',
                'volume', 'amount', 'timestamp', 'kline_end_timestamp', 'symbol'
            ]

            df = df.reset_index()
            df = df[required_columns]

            return df

        except Exception as e:
            print(f"Error retrieving data: {e}")
            raise


if __name__ == '__main__':

    minio_auth_key="fQBg0wlcr60WQZYFxwUl"
    minio_secret_key="B8P2U7ULUywRnBxwfFvY05taU2pnarBeK4rWHWsc"

    # import_data(
    #     minio_url="s3://100.77.61.77:9000",
    #     directory="/mnt/zzk_disk1/mt_data/mt_data_20241231/全套数据TXT",
    #     dataset_name="mt4_min_kline",
    #     bucket_name="mktdata-hist",
    #     minio_auth_key=minio_auth_key,
    #     minio_secret_key=minio_secret_key
    # )

    # minio_auth_key="lDLT6pMYETWDDNvMVzx4"
    # minio_secret_key="OI56GeS4tIhPCH2rmz6aBMFg4OY9Eo0acvyqyeu8"

    print("starting test...")

    minio_auth_key="fQBg0wlcr60WQZYFxwUl"
    minio_secret_key="B8P2U7ULUywRnBxwfFvY05taU2pnarBeK4rWHWsc"

    config = ArcticdbDataSourceConfig(
        minio_auth_key=minio_auth_key,
        minio_secret_key=minio_secret_key,
        minio_url='s3://100.77.61.77:9000',
        minio_bucket_name='mktdata-hist',
        minio_dataset_name='mt4_min_kline'
    )

    source = ArcticdbKlineDataSource(config)
    symbol = 'USDJPY'
    start_dt = datetime(2021, 1, 1)
    end_dt = datetime(2021, 1, 30)
    df = source.get_data(symbol, start_dt, end_dt)

    print(df)

    # minio_url = "s3://100.77.61.77:9000"
    # bucket_name = "mktdata-hist"
    # dataset_name = "mt4_min_kline"
    # uri = ArcticdbKlineDataSource._build_arctic_s3_uri(
    #     minio_url=minio_url,
    #     bucket_name=bucket_name,
    #     access_key=minio_auth_key,
    #     secret_key=minio_secret_key,
    # )
    # ac = adb.Arctic(uri)
    # library = ac[dataset_name]

    # print("library.list_symbols()")
    # print(ac.get_library(dataset_name).list_symbols())


