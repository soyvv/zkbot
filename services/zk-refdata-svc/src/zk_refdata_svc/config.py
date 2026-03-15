"""Runtime configuration loaded from environment variables."""

import os


class Config:
    nats_url: str
    pg_url: str
    grpc_port: int
    logical_id: str

    def __init__(self) -> None:
        self.nats_url = os.environ["ZK_NATS_URL"]
        self.pg_url = os.environ["ZK_PG_URL"]
        self.grpc_port = int(os.getenv("ZK_REFDATA_GRPC_PORT", "50052"))
        self.logical_id = os.getenv("ZK_REFDATA_LOGICAL_ID", "refdata_dev_1")
