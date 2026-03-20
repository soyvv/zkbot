# Instrument Convention

This note summarizes the effective instrument convention used across the current
architecture docs, Python utilities, and the Rust OMS implementation.

## Core Rule

The canonical tradable instrument key inside zkbot is `instrument_id`.

- client-facing APIs should use `instrument_id`
- OMS order requests should use `instrument_code` / `instrument_id` in this canonical form
- venue-native symbols are separate transport fields and should not be used as the
  primary internal identifier

In refdata terms:

- `instrument_id`: zkbot canonical instrument identifier
- `instrument_id_exchange` / `instrument_exch`: venue-native symbol
- `venue`: venue namespace used to resolve transport-facing subjects and routing

## Practical Shape

From the implemented OMS usage, the canonical `instrument_id` is a venue-qualified
 market identifier, typically of the form:

- `<market>@<venue>`

Examples seen in `zk-oms-rs` tests:

- `ETH-P/USDC@EX1`
- `BTC-P/USDC@EX1`
- `ETH/USD@EX2`
- `BTC-USDT@EX`

The venue-native symbol is separate and usually shorter, for example:

- canonical `instrument_id`: `ETH-P/USDC@EX1`
- venue-native `instrument_id_exchange`: `ETH`

So the working model is:

- internal/canonical: venue-qualified market identifier
- external/transport/native: exchange symbol

## Instrument Type Encoding

The Python helper [`instrument_utils.py`](/Users/zzk/workspace/zklab/zkbot/libs/zk-core/src/zk_utils/instrument_utils.py)
captures the current type inference convention for the base symbol portion:

- no suffix: spot-like
- `-P`: perp
- `-F`: future
- `-CFD`: CFD

This is used for instrument-type classification, not as a full standalone parsing
spec for the entire `instrument_id`.

## Domain Semantics

From the OMS position/balance refactor:

- `Position` means instrument exposure
- `Balance` means asset inventory / cash ledger state

Examples:

- `AAPL`, `SPY`, `BTC-PERP`, `EURUSD CFD` belong in `Position`
- `USD`, `USDT`, `BTC`, `JPY` belong in `Balance`

Spot assets can legitimately appear in both domains:

- `Balance(asset=BTC)` means wallet or collateral inventory
- `Position(instrument=BTC-USDT spot instrument)` means tradable market exposure

These must not be collapsed into one concept.

## Spot Special Case

There is one important OMS bookkeeping nuance:

- canonical tradable spot instrument remains `instrument_id` such as `BTC-USDT@EX`
- but OMS may derive an operational spot position keyed by `base_asset`, such as `BTC`

This derived spot position is:

- an operational projection for sell checks / fill handling
- not canonical exchange truth
- not a replacement for the tradable instrument identifier

So:

- tradable identity: `BTC-USDT@EX`
- derived OMS spot inventory position key: `BTC`

## Recommended Interpretation

When writing new code:

- use `instrument_id` as the canonical internal symbol
- use refdata to resolve `(venue, instrument_exch)` for gateway and RTMD transport
- do not treat venue-native symbols as globally unique
- do not confuse derived spot asset positions with canonical tradable instruments

## Source Alignment

This summary is consistent with:

- [sdk.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- [rtmd_subscription_protocol.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rtmd_subscription_protocol.md)
- [proto.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/proto.md)
- [oms_position_balance_refactor.md](/Users/zzk/workspace/zklab/zkbot/docs/domains/oms_position_balance_refactor.md)
- [instrument_utils.py](/Users/zzk/workspace/zklab/zkbot/libs/zk-core/src/zk_utils/instrument_utils.py)
- `zk-oms-rs` refdata / metadata / tests
