# venue-integrations/legacy

Reference-only snapshots of pre-refactor venue artifacts. **Not imported by active code**, not a uv workspace member, not installable.

## vendored-proto/

Snapshots of venue-local `zk/*_pb2.py` trees that existed before zb-00028 refactor unified proto codegen into `python/proto-pb/` (exposing the shared `zk.*` namespace).

- `oanda/` — was `venue-integrations/oanda/oanda/proto/`
- `ibkr/` — was `venue-integrations/ibkr/ibkr/proto/`

These are kept solely for historical reference. Active venue code now imports from `zk_proto_pb` (pyproject dep `zk-proto-pb`).
