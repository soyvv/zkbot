# Venue Integrations

Active venue modules live here. This is not a placeholder area.

## Current Contract

- one venue = one directory under `zkbot/venue-integrations/`
- each venue owns its own `manifest.yaml`
- schemas live under `schemas/`
- Python capabilities live under the venue package itself, for example `oanda/` or `ibkr/`
- Rust capabilities, when present, live under `rust/`
- tests stay venue-local under `tests/`

Examples:

- `oanda/` and `ibkr/` are Python-first venue packages
- `okx/` is mixed: Rust for `gw` and `rtmd`, Python for `refdata`
- `legacy/` contains reference-only material and should not be used as the active integration shape

## Discovery Model

- runtime hosts read venue manifests and schemas from the venue-integrations root on disk
- Python modules are imported as installed packages, not via `sys.path` injection
- see `zkbot/docs/system-arch/venue_integration.md` for the venue module shape
- see `zkbot/docs/system-arch/dependency-contract.md` for the cross-language loading contract
