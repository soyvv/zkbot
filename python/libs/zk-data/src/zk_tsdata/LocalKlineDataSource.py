"""KlineDataSource backed by a local Parquet fixture.

The Parquet file must have the same column schema as ArcticDBKlineDataSource:
    kline_type, symbol, open, high, low, close, volume, amount,
    timestamp (unix ms int64), kline_end_timestamp (unix ms int64)

Produced by: tools/prepare_local_fixture.py
"""

from datetime import datetime
from pathlib import Path

import pandas as pd

from zk_strategy.datasource import KlineDataSource


class LocalParquetKlineDataSource(KlineDataSource):
    """Reads 1m kline bars from a local Parquet file.

    Interchangeable with ArcticdbKlineDataSource as BacktestConfig.kline_data_source.
    The full dataset is loaded into memory on construction and filtered on each
    get_data() call, so keep fixture files reasonably sized (a few months is fine).
    """

    def __init__(self, path: str | Path) -> None:
        super().__init__()
        self._path = Path(path)
        if not self._path.exists():
            raise FileNotFoundError(
                f"Fixture file not found: {self._path}\n"
                "Run tools/prepare_local_fixture.py first."
            )
        self._df = pd.read_parquet(self._path)

    def get_data(self, symbol: str, start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        """Return rows for *symbol* within [start_dt, end_dt] (inclusive)."""
        start_ms = int(start_dt.timestamp() * 1_000)
        end_ms = int(end_dt.timestamp() * 1_000)

        mask = (
            (self._df["symbol"] == symbol)
            & (self._df["timestamp"] >= start_ms)
            & (self._df["timestamp"] <= end_ms)
        )
        return self._df[mask].copy().reset_index(drop=True)
