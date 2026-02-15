from datetime import datetime, timezone

def get_now_ms():
    return int(datetime.utcnow().replace(tzinfo=timezone.utc).timestamp() * 1000)
