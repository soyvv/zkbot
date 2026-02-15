## zk-refdata-loader

Reference data ingestion utilities for zkbot. Each exchange implementation lives under `zk_refdata_loader.exchanges`.

### Usage

```bash
uv run python -m zk_refdata_loader.loader --exchange Binance
```

### Development

- Generate protobuf dependencies with `make gen` at the repo root.
- Tests for individual exchanges can be implemented under `tests/` (create if needed).
