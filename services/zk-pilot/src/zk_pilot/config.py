"""Environment-variable configuration for zk-pilot."""

import os


class Config:
    nats_url: str
    pg_url: str
    env: str
    pilot_id: str
    http_port: int
    lease_ttl_minutes: int

    def __init__(self) -> None:
        self.nats_url = os.environ.get("ZK_NATS_URL", "nats://localhost:4222")
        self.pg_url = os.environ.get("ZK_PG_URL", "postgres://zk:zk@localhost:5432/zkbot")
        self.env = os.environ.get("ZK_ENV", "dev")
        self.pilot_id = os.environ.get("ZK_PILOT_ID", "pilot_dev_1")
        self.http_port = int(os.environ.get("ZK_HTTP_PORT", "8090"))
        self.lease_ttl_minutes = int(os.environ.get("ZK_LEASE_TTL_MINUTES", "5"))
