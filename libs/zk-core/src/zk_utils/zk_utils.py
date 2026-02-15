import random

import snowflake.snowflake
from snowflake import SnowflakeGenerator
import datetime
import os

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


def create_id_gen(instance_id:int = None):
    if instance_id is None:
        instance_id = random.randint(1, snowflake.snowflake.MAX_INSTANCE-1)
    id_gen = SnowflakeGenerator(instance=instance_id)
    return id_gen


def try_read_envvar(envvar_name: str) -> str:
    try:
        return os.environ[envvar_name]
    except KeyError:
        return None