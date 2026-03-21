# Venue Integration Designs

Venue-specific integration design notes that apply the shared architecture to concrete venues.

These docs are grounded in:

- [Venue Integration Modules](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/venue_integration.md)
- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
- [Refdata Loader Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/refdata_loader_service.md)
- [Phase 12A: Python Venue Adaptor Bridge](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md)
- [Phase 12: Full Refdata Service](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/14-refdata-service.md)

Available venue designs:

- [OKX](/Users/zzk/workspace/zklab/zkbot/docs/integrations/okx.md)
- [OANDA](/Users/zzk/workspace/zklab/zkbot/docs/integrations/oanda.md)
- [IBKR](/Users/zzk/workspace/zklab/zkbot/docs/integrations/ibkr.md)

Default client-library guidance from the current design:

- Rust native venues: `reqwest` + `tokio-tungstenite`
- Python API-driven venues: `httpx` + explicit streaming client handling
- IBKR: `ib_async`

Refdata integration rule:

- venue-specific refdata adaptors live under `zkbot/venue-integrations/<venue>/`
- the refdata host loads them from the venue manifest and validates `schemas/refdata_config.schema.json`
- venue modules should provide both instrument loading and, where relevant, market-session loading
