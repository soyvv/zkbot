
from datetime import datetime, timedelta
from .models import TimerEvent
from croniter import croniter

import heapq

_K = "clock"
_C = "cron"
class TimerManager:
    def __init__(self, init_ts: datetime=None):
        self._clocks = {} # {timer_key: datetime}
        self._del_clocks = {} # {timer_key: datetime}
        self._crons = {} # {timer_key: croniter instacne}
        self._q = [] # (ts, key, type=(cron|clock))
        self._curr_dt = init_ts if init_ts else datetime.now()

    def subscribe_timer_cron(self, timer_key: str,  cron_expression: str, start_ts: datetime=None,
                             end_ts: datetime=None):
        if timer_key in self._crons:
            raise ValueError(f"Timer {timer_key} already exists")

        start_ts = start_ts if start_ts else self._curr_dt
        self._crons[timer_key] = croniter(cron_expression, start_ts)

        if end_ts:
            self._del_clocks[timer_key] = end_ts

        heapq.heappush(self._q, (self._crons[timer_key].get_next(datetime), timer_key, _C))

    def subscribe_timer_clock(self, timer_key: str, date_time: datetime):
        if timer_key in self._clocks:
            raise ValueError(f"Timer {timer_key} already exists")

        self._clocks[timer_key] = date_time

        heapq.heappush(self._q, (date_time, timer_key, _K))


    def generate_next_batch_events(self, instant_ts: datetime) -> list[TimerEvent]:
        #if self._curr_dt > instant_ts:
            #raise ValueError(f"Instant ts {instant_ts} is earlier than current ts {self._curr_dt}")

        # NOTE: we don't check monotonicity here.
        self._curr_dt = instant_ts
        timer_events = []
        while self._q and self._q[0][0] <= instant_ts:
            ts, key, type = heapq.heappop(self._q)
            if type == _K:
                timer_events.append(TimerEvent(key, ts))
                del self._clocks[key]
            elif type == _C:
                timer_events.append(TimerEvent(key, ts))
                end_ts = self._del_clocks.get(key, None)
                next_ts = self._crons[key].get_next(datetime)
                if end_ts and next_ts > end_ts:
                    del self._crons[key]
                else:
                    heapq.heappush(self._q, (next_ts, key, _C))
                # if key in self._crons:
                #     heapq.heappush(self._q, (self._crons[key].get_next(datetime), key, _C))
            else:
                raise ValueError(f"Unknown type {type}")

        return timer_events



def test():
    tm = TimerManager()
    start_ts = datetime(2023, 3, 31, 8, 0, 0)
    tm.subscribe_timer_clock("k1", datetime(2023, 3, 31, 8, 0, 10))
    tm.subscribe_timer_clock("k2", datetime(2023, 3, 31, 8, 10, 20))
    tm.subscribe_timer_clock("k3", datetime(2023, 3, 31, 8, 30, 0))

    tm.subscribe_timer_cron("c1", "*/1 * * * *", start_ts)
    tm.subscribe_timer_cron("c2", "*/3 * * * *", start_ts)
    tm.subscribe_timer_cron("c3", "* * * * * 0,10,20,30,40,50", start_ts)


    for i in range(100):
        seconds = i * 10
        ts = start_ts + timedelta(seconds=seconds)
        print(tm.generate_next_batch_events(ts))


if __name__ == "__main__":
    test()