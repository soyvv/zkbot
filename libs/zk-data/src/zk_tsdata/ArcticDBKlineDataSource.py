from dataclasses import dataclass
from datetime import datetime
import pandas as pd
import arcticdb as adb

from zk_strategy.datasource import KlineDataSource


#from tq_strategy.datasource import KlineDataSource



@dataclass
class ArcticdbDataSourceConfig:
    minio_auth_key: str = None
    minio_secret_key: str = None
    minio_url: str = None
    minio_bucket_name: str = None
    minio_dataset_name: str = None

class ArcticdbKlineDataSource(KlineDataSource):

    def __init__(self, config: ArcticdbDataSourceConfig):
        super().__init__()
        self.config = config
        minio_auth_key = config.minio_auth_key
        minio_secret_key = config.minio_secret_key
        minio_url = config.minio_url
        bucket_name = config.minio_bucket_name
        self.dataset_name = config.minio_dataset_name
        self.ac = adb.Arctic(f'{minio_url}:{bucket_name}&access={minio_auth_key}&secret={minio_secret_key}')
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
    minio_auth_key = "wL3KT2bQ9DvqohmC71np"
    minio_secret_key = "ynoCRqSkP2jN5CskEJ6WTXHRKbigYH0jA9jNyWJK"
    config = ArcticdbDataSourceConfig(
        minio_auth_key=minio_auth_key,
        minio_secret_key=minio_secret_key,
        minio_url='s3://100.82.246.47:9003',
        minio_bucket_name='mktdata-kline',
        minio_dataset_name='mt4_min_kline'
    )

    source = ArcticdbKlineDataSource(config)
    symbol = 'USDJPY'
    start_dt = datetime(2021, 1, 1)
    end_dt = datetime(2021, 1, 30)
    df = source.get_data(symbol, start_dt, end_dt)

    print(df)

    import talib

    rsi = talib.RSI(df['close'], timeperiod=14)

    df['rsi'] = rsi

    print(rsi)

