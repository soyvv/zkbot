import pandas as pd
import json

from typing import Optional, Tuple, Union

from highcharts_stock.chart import Chart

def create_trade_analysis_chart(
    trades: pd.DataFrame,
    klines: pd.DataFrame,
    signals: pd.DataFrame,
    title: str = "Trade Analysis",
    output_file: str = "trade_analysis.html"
):
    """
    Generate a Highcharts Stock HTML chart showing:
      • OHLC candlesticks
      • Trade flags (buys/sells) on the candlesticks
      • Stacked panels for each signal series

    Parameters
    ----------
    trades : pd.DataFrame
        columns=['ts', 'side', 'qty', 'price']
        ts = datetime or Unix timestamp
    klines : pd.DataFrame
        columns=['ts','open','high','low','close']
        ts = datetime or Unix timestamp
    signals : pd.DataFrame
        columns=['ts', 'signal1', 'signal2', …]
    title : str
        Chart title
    output_file : str
        Path to write the resulting HTML
    """

    # 1) Normalize timestamps to ms since epoch
    for df in (trades, klines, signals):
        if df is None:
            continue
        #df["ts_ms"] = df["ts"]
        df["ts"] = pd.to_datetime(df["ts"])
        df["ts_ms"] = (df["ts"].view("int64") // 10**6).astype(int)

    print(df)

    # 2) Prepare data lists
    ohlc_list = klines[["ts","open","high","low","close"]].values.tolist()
    flag_list = [
        [int(row.ts_ms),
         row.side.lower() == "buy" and "B" or "S",
         f"{row.side.capitalize()} {row.qty} @ {row.price}"]
        for row in trades.itertuples()
    ]
    # Turn each into a flags-style dict
    flags_data = [
        {"x": x, "title": title, "text": text}
        for x, title, text in flag_list
    ]

    # 3) Build series dicts
    series = [
        {
            "type": "candlestick",
            "id": "kline",
            "name": "OHLC",
            "data": ohlc_list
        },
        {
            "type": "flags",
            "onSeries": "kline",
            "shape": "circlepin",
            "width": 16,
            "data": flags_data
        }
    ]

    # 4) Add each signal as its own line series
    if signals:
        for sig in [c for c in signals.columns if c != "ts"]:
            sig_list = signals[["ts_ms", sig]].values.tolist()
            series.append({
                "type": "line",
                "name": sig,
                "data": sig_list,
                "yAxis": len(series)  # each new series goes to next axis
            })

        # 5) Define yAxis layout: top chart 60%, rest share 40%
        num_signals = len(series) - 2
        sig_height = 40 / num_signals if num_signals else 40
        yAxes = [
            {"height": "60%", "title": {"text": "Price"}}
        ] + [
            {
                "top": f"{60 + i*sig_height}%",
                "height": f"{sig_height}%",
                "title": {"text": series[i+2]["name"]}
            }
            for i in range(num_signals)
        ]
    else:
        # 5) Define yAxis layout: single chart
        yAxes = [{"height": "100%", "title": {"text": "Price"}}]

    # 6) Assemble full options dict
    opts = {
        "rangeSelector": {"selected": 1},
        "title": {"text": title},
        "xAxis": {"type": "datetime"},
        "yAxis": yAxes,
        "series": series
    }

    # 7) Create & save chart
    chart = Chart.from_options(opts)

    return chart
    # chart.save_file(output_file)
    # print(f"Chart written to {output_file}")


def filter_data_window(
        trades: pd.DataFrame,
        klines: pd.DataFrame,
        signals: Optional[pd.DataFrame],
        idx: int,
        x_unit_of_time: Union[str, pd.Timedelta],
        y_unit_of_time: Union[str, pd.Timedelta]
) -> Tuple[pd.DataFrame, pd.DataFrame, Optional[pd.DataFrame]]:
    """
    Windowed slice around the trade at position `idx`.

    Window = [ts_trade - x_unit_of_time, ts_trade + y_unit_of_time]

    Returns
    -------
    trades_win, klines_win, signals_win
      trades_win: all trades in the window
      klines_win: all OHLC rows in the window
      signals_win: all signals in the window (or None if signals was None)
    """
    # 1) Copy & convert ts columns once and for all
    trades = trades.copy()
    trades['ts'] = pd.to_datetime(trades['ts'])

    klines = klines.copy()
    klines['ts'] = pd.to_datetime(klines['ts'])

    if signals is not None:
        signals = signals.copy()
        signals['ts'] = pd.to_datetime(signals['ts'])

    # 2) Pick out the center timestamp via iloc (always a scalar Timestamp)
    center_ts = trades.iloc[idx]['ts']

    # 3) Build the window bounds
    left_bound = pd.to_datetime(center_ts) - pd.to_timedelta(x_unit_of_time)
    right_bound = pd.to_datetime(center_ts) + pd.to_timedelta(y_unit_of_time)

    print(left_bound, right_bound)

    # 4) Boolean‐index each DataFrame
    mask_t = (trades['ts'] >= left_bound) & (trades['ts'] <= right_bound)
    mask_k = (klines['ts'] >= left_bound) & (klines['ts'] <= right_bound)

    trades_win = trades.loc[mask_t]
    klines_win = klines.loc[mask_k]

    if signals is None:
        signals_win = None
    else:
        mask_s = (signals['ts'] >= left_bound) & (signals['ts'] <= right_bound)
        signals_win = signals.loc[mask_s]

    return trades_win, klines_win, signals_win