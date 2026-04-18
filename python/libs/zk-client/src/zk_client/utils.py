
import datetime

def from_unix_ts_to_datetime(ts: int):
    seconds = ts / 1000
    milliseconds = ts % 1000
    dt = datetime.datetime.fromtimestamp(seconds)
    if milliseconds > 0:
        dt = dt.replace(microsecond=milliseconds*1000)

    return dt


def from_datetime_to_unix_ts(dt: datetime.datetime):
    seconds = int(dt.timestamp())
    milliseconds = dt.microsecond // 1000
    ts = seconds * 1000 + milliseconds

    return ts