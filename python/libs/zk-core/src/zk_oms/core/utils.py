
from datetime import datetime
import time
import orjson as json
import betterproto


def gen_timestamp() -> int:
    # todo
    #.mktime(date_time.timetuple())
    dt = datetime.now()
    dt_ts = time.mktime(dt.timetuple())
    return int(dt_ts * 1000)


def pb_to_json(msg: betterproto.Message, pretty=False) -> bytes:
    d = msg.to_dict(casing=betterproto.Casing.SNAKE)
    return json.dumps(d, option=json.OPT_INDENT_2 if pretty else 0)


# python logger helper