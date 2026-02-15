# zk-client

Async client SDK for interacting with zkbot control-plane services (OMS, strategy, risk).

### Install

```bash
uv build
uv pip install dist/zk_client-*.whl
```

### Usage

```python
from zk_client.tqclient import TQClient, TQClientConfig

config = TQClientConfig(nats_url="nats://localhost:4222")
client = TQClient(config)
```
