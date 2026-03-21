"""Runtime configuration loaded from environment variables."""

import os


class Config:
    nats_url: str
    pg_url: str
    grpc_port: int
    logical_id: str
    refresh_interval_s: int
    heartbeat_interval_s: int
    lease_ttl_s: int
    venues: list[str]

    def __init__(self) -> None:
        self.nats_url = os.environ["ZK_NATS_URL"]
        self.pg_url = os.environ["ZK_PG_URL"]
        self.grpc_port = int(os.getenv("ZK_REFDATA_GRPC_PORT", "50052"))
        self.logical_id = os.getenv("ZK_REFDATA_LOGICAL_ID", "refdata_dev_1")
        self.refresh_interval_s = int(os.getenv("ZK_REFDATA_REFRESH_INTERVAL_S", "300"))
        self.heartbeat_interval_s = int(os.getenv("ZK_REFDATA_HEARTBEAT_INTERVAL_S", "5"))
        self.lease_ttl_s = int(os.getenv("ZK_REFDATA_LEASE_TTL_S", "20"))
        self.advertise_host = os.getenv("ZK_REFDATA_ADVERTISE_HOST", "localhost")
        venues_str = os.getenv("ZK_REFDATA_VENUES", "")
        self.venues = [v.strip() for v in venues_str.split(",") if v.strip()]
