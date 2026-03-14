# zkbot Project Plan

> Reference: [project_memo.md](project_memo.md) for architecture and current state

---

## Table of Contents

1. [Interactive Brokers (IB) Integration](#1-interactive-brokers-ib-integration)
2. [Oanda Trading Integration](#2-oanda-trading-integration)
3. [TokkaQuant Legacy Naming Removal](#3-tokkaquant-legacy-naming-removal)
4. [Initial Deployment Plan](#4-initial-deployment-plan)
5. [Timeline Overview](#5-timeline-overview)

---

## 1. Interactive Brokers (IB) Integration

### 1.1 Overview

Integrate Interactive Brokers for trading traditional instruments with primary focus on **ETFs**, while supporting other asset classes (stocks, futures, options) where feasible.

### 1.2 Scope

| Asset Class | Priority | Notes |
|-------------|----------|-------|
| ETFs        | P0       | Primary focus (SPY, QQQ, sector ETFs, bond ETFs) |
| Stocks      | P1       | US equities, likely reuse ETF implementation |
| Futures     | P2       | Index futures, commodities |
| Options     | P3       | If time permits, complex order types |

### 1.3 Technical Approach

#### 1.3.1 IB API Options

| Option | Library | Pros | Cons |
|--------|---------|------|------|
| **ib_insync** | [ib_insync](https://github.com/erdewit/ib_insync) | Pythonic, async-friendly, well-maintained | Requires TWS/IB Gateway running |
| **ibapi** | Official IB API | Official support | Callback-heavy, less Pythonic |

**Recommendation**: Use `ib_insync` for cleaner async integration with existing NATS-based architecture.

#### 1.3.2 Components to Implement

```
services/
└── zk-gw-ib/                      # New IB gateway service
    ├── pyproject.toml
    └── src/
        └── ib_gw/
            ├── __init__.py
            ├── gateway.py          # Main gateway implementation
            ├── order_handler.py    # Order submission/cancellation
            ├── market_data.py      # Real-time market data subscription
            ├── account_handler.py  # Account/position sync
            └── utils.py

libs/zk-refdata-loader/
└── src/zk_refdata_loader/exchanges/
    └── InteractiveBrokers.py       # IB reference data fetcher
```

#### 1.3.3 Data Model Mappings

| IB Concept | zkbot Equivalent | Notes |
|------------|------------------|-------|
| Contract   | InstrumentRefData | Map secType, exchange, currency |
| Order      | OrderRequest | Map orderType, tif, action |
| Execution  | Trade | Map execId, shares, price |
| OrderStatus | OrderReport | Map status, filled, remaining |
| Position   | Position | Map position, avgCost |

#### 1.3.4 IB-Specific Considerations

1. **Connection Management**
   - IB allows limited concurrent connections (typically 1-2)
   - Implement connection pooling/recovery
   - Handle TWS/Gateway restarts gracefully

2. **Order Types**
   - Support: LMT, MKT, STP, STP_LMT
   - TIF: DAY, GTC, IOC, FOK
   - Extended hours trading flag

3. **Market Data**
   - Delayed vs real-time data (requires market data subscription)
   - Snapshot vs streaming modes

4. **Account Types**
   - Paper trading (port 7497)
   - Live trading (port 7496)
   - Multiple accounts support

### 1.4 Implementation Tasks

| Task | Description | Estimate |
|------|-------------|----------|
| IB-01 | Create `zk-gw-ib` service scaffold | S |
| IB-02 | Implement IB connection manager with ib_insync | M |
| IB-03 | Implement reference data fetcher (ETFs, stocks) | M |
| IB-04 | Implement order submission/cancellation | L |
| IB-05 | Implement OrderReport translation | M |
| IB-06 | Implement position/account sync | M |
| IB-07 | Implement market data subscription | M |
| IB-08 | Add IB-specific proto extensions if needed | S |
| IB-09 | Integration tests with IB paper account | L |
| IB-10 | Documentation | S |

**Size Legend**: S = Small (1-2 days), M = Medium (3-5 days), L = Large (1-2 weeks)

---

## 2. Oanda Trading Integration

### 2.1 Overview

Extend existing Oanda reference data implementation to full trading support for **CFD instruments** (forex pairs, indices, commodities).

### 2.2 Current State

- Reference data fetcher exists: [Oanda.py](services/zk-refdata-loader/src/zk_refdata_loader/exchanges/Oanda.py)
- Uses `v20` library (Oanda REST API v20)
- Practice environment configured

### 2.3 Technical Approach

#### 2.3.1 Components to Implement

```
services/
└── zk-gw-oanda/                   # New Oanda gateway service
    ├── pyproject.toml
    └── src/
        └── oanda_gw/
            ├── __init__.py
            ├── gateway.py          # Main gateway implementation
            ├── order_handler.py    # Order submission/cancellation
            ├── streaming.py        # Price streaming (v20 streaming API)
            ├── account_handler.py  # Account/position management
            └── utils.py
```

#### 2.3.2 Oanda v20 API Endpoints

| Endpoint | Use Case |
|----------|----------|
| `POST /accounts/{id}/orders` | Place orders |
| `PUT /accounts/{id}/orders/{orderId}/cancel` | Cancel orders |
| `GET /accounts/{id}/orders` | List open orders |
| `GET /accounts/{id}/positions` | Get positions |
| `GET /accounts/{id}/transactions` | Trade history |
| `GET /pricing/stream` | Real-time price streaming |

#### 2.3.3 Data Model Mappings

| Oanda Concept | zkbot Equivalent | Notes |
|---------------|------------------|-------|
| Instrument    | InstrumentRefData | Already implemented |
| Order         | OrderRequest | Map type, units, price |
| Trade         | Trade | Map tradeId, units, price |
| Position      | Position | Map long/short, unrealizedPL |
| Transaction   | OrderReport | Map type, reason |

#### 2.3.4 Oanda-Specific Considerations

1. **Order Types**
   - Market, Limit, Stop, Market-if-Touched, Trailing Stop
   - Take Profit / Stop Loss as separate orders linked to trades

2. **Position Management**
   - Oanda uses "Trade" concept (individual fills)
   - Position is aggregate of all trades for an instrument
   - FIFO closing by default

3. **Pricing**
   - Spread-based pricing (bid/ask)
   - Variable spreads during volatility

4. **Account Types**
   - Practice (demo) environment
   - Live environment
   - Different API hostnames

### 2.4 Implementation Tasks

| Task | Description | Estimate |
|------|-------------|----------|
| OA-01 | Create `zk-gw-oanda` service scaffold | S |
| OA-02 | Implement order submission (market, limit) | M |
| OA-03 | Implement order cancellation/modification | M |
| OA-04 | Implement OrderReport translation from transactions | M |
| OA-05 | Implement position sync | S |
| OA-06 | Implement price streaming | M |
| OA-07 | Handle Oanda-specific order types (TP/SL) | M |
| OA-08 | Migrate credentials from hardcoded to config | S |
| OA-09 | Integration tests with practice account | M |
| OA-10 | Documentation | S |

---

## 3. TokkaQuant Legacy Naming Removal

### 3.1 Overview

Systematically remove all `tq_*` prefixes and rename to `zk_*` for consistency.

### 3.2 Current State Audit

#### Files/Modules with `tq_` prefix:

```
libs/zk-client/src/tqclient/                    # → zk_client (redirect exists)
libs/zk-rpc/src/tqrpc_utils/                    # → zk_rpc_utils
services/zk-refdata-loader/src/tq_refdata_loader/  # → zk_refdata_loader (redirect exists)
services/zk-service/src/tq_service_api/         # → zk_service_api
services/zk-service/src/tq_service_app/         # → zk_service_app
services/zk-service/src/tq_service_helper/      # → zk_service_helper
services/zk-service/src/tq_service_oms/         # → zk_service_oms
services/zk-service/src/tq_service_riskmonitor/ # → zk_service_riskmonitor
services/zk-service/src/tq_service_simulator/   # → zk_service_simulator
services/zk-service/src/tq_service_strategy/    # → zk_service_strategy
```

#### Proto files with `tqrpc_` prefix:

```
protos/rpc-*.proto      # Service names use tqrpc_ in generated code
                        # May need to update package names
```

#### Classes/Variables:

```
TQRpcService            # → ZKRpcService
TokkaQuant              # → ZKQuant or ZKBot
tqutils                 # → zk_utils
```

### 3.3 Migration Strategy

#### Phase 1: Add Compatibility Aliases (Non-breaking)

```python
# In each renamed module's __init__.py
from zk_service_oms import *  # New name

# Deprecated alias
import warnings
def __getattr__(name):
    warnings.warn(f"tq_service_oms is deprecated, use zk_service_oms", DeprecationWarning)
    ...
```

#### Phase 2: Rename Directories/Modules

1. Rename directories from `tq_*` to `zk_*`
2. Update all imports across codebase
3. Update `pyproject.toml` package configurations
4. Update proto package names if needed

#### Phase 3: Rename Classes

1. `TQRpcService` → `ZKRpcService`
2. `TokkaQuant` → `ZKQuant` (or keep as user-facing API name)
3. Update all references

#### Phase 4: Remove Deprecated Aliases

After sufficient migration period, remove backward compatibility.

### 3.4 Implementation Tasks

| Task | Description | Estimate |
|------|-------------|----------|
| NM-01 | Audit all `tq_*` occurrences | S |
| NM-02 | Create rename mapping document | S |
| NM-03 | Rename `libs/zk-rpc/src/tqrpc_utils/` → `zk_rpc/` | M |
| NM-04 | Rename `services/zk-service/src/tq_service_*` modules | L |
| NM-05 | Update all internal imports | M |
| NM-06 | Rename `TQRpcService` → `ZKRpcService` | M |
| NM-07 | Decide on `TokkaQuant` API name (keep or rename) | S |
| NM-08 | Update proto package names if needed | M |
| NM-09 | Add deprecation warnings for old names | S |
| NM-10 | Update documentation | S |

---

## 4. Initial Deployment Plan

### 4.1 Deployment Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Deployment Environment                       │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │   NATS       │  │    Redis     │  │   Postgres   │          │
│  │  (Message)   │  │   (Cache)    │  │  (Optional)  │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
│         │                 │                 │                   │
│  ───────┴─────────────────┴─────────────────┴────────────────   │
│                           │                                      │
│  ┌────────────────────────┼────────────────────────┐            │
│  │                  Service Layer                   │            │
│  ├──────────────┬──────────────┬──────────────────┤            │
│  │  OMS Server  │  ODS Server  │ Strategy Engine  │            │
│  └──────────────┴──────────────┴──────────────────┘            │
│                           │                                      │
│  ┌────────────────────────┼────────────────────────┐            │
│  │                  Gateway Layer                   │            │
│  ├──────────┬──────────┬──────────┬───────────────┤            │
│  │  IB GW   │ Oanda GW │  OKX GW  │  Other GWs    │            │
│  └──────────┴──────────┴──────────┴───────────────┘            │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 4.2 Containerization

#### 4.2.1 Docker Structure

```
docker/
├── Dockerfile.base           # Base image with Python + dependencies
├── Dockerfile.oms            # OMS service
├── Dockerfile.ods            # ODS service
├── Dockerfile.strategy       # Strategy engine
├── Dockerfile.gw-ib          # IB gateway
├── Dockerfile.gw-oanda       # Oanda gateway
├── docker-compose.yml        # Local development
└── docker-compose.prod.yml   # Production template
```

#### 4.2.2 Base Dockerfile Example

```dockerfile
FROM python:3.11-slim

# Install uv
RUN pip install uv

WORKDIR /app
COPY pyproject.toml uv.lock ./
COPY libs/ ./libs/

# Install dependencies
RUN uv sync --frozen

COPY services/ ./services/
```

### 4.3 Infrastructure Requirements

#### 4.3.1 Minimum Requirements (Development/Paper Trading)

| Component | Specs | Notes |
|-----------|-------|-------|
| Compute   | 2 vCPU, 4GB RAM | Single node sufficient |
| NATS      | Single instance | Embedded or standalone |
| Redis     | Single instance | For order state cache |
| Network   | Stable internet | For exchange connectivity |

#### 4.3.2 Production Requirements

| Component | Specs | Notes |
|-----------|-------|-------|
| Compute   | 4+ vCPU, 8GB+ RAM | Consider separate nodes per gateway |
| NATS      | 3-node cluster | For HA |
| Redis     | Redis Sentinel or Cluster | For HA |
| Network   | Low latency, redundant | Co-locate near exchange if possible |

### 4.4 Configuration Management

#### 4.4.1 Environment Configuration

```yaml
# config/env.yaml
environment: production  # or development, staging

nats:
  url: nats://localhost:4222
  cluster_urls: []

redis:
  host: localhost
  port: 6379
  db: 0

gateways:
  ib:
    enabled: true
    host: 127.0.0.1
    port: 7497  # 7496 for live
    client_id: 1
  oanda:
    enabled: true
    environment: practice  # or live
    account_id: ${OANDA_ACCOUNT_ID}
    access_token: ${OANDA_ACCESS_TOKEN}
```

#### 4.4.2 Secrets Management

- Use environment variables for sensitive data
- Consider HashiCorp Vault or AWS Secrets Manager for production
- Never commit credentials to source control

### 4.5 Deployment Phases

#### Phase 1: Local Development Setup

1. Docker Compose for local services (NATS, Redis)
2. Run services directly with `uv run`
3. Paper trading accounts only

#### Phase 2: Staging Environment

1. Deploy to single cloud VM or container
2. Separate config for staging
3. Paper trading with realistic conditions

#### Phase 3: Production Environment

1. Deploy to production infrastructure
2. Enable live trading accounts
3. Monitoring and alerting setup
4. Backup and recovery procedures

### 4.6 Monitoring & Observability

#### 4.6.1 Metrics to Track

| Category | Metrics |
|----------|---------|
| Orders   | Order count, fill rate, latency, rejection rate |
| Positions | P&L, exposure, margin usage |
| System   | CPU, memory, network, queue depth |
| Gateway  | Connection status, message rate, errors |

#### 4.6.2 Tooling Options

| Tool | Purpose |
|------|---------|
| Prometheus | Metrics collection |
| Grafana | Dashboards |
| Loki | Log aggregation |
| AlertManager | Alerting |

### 4.7 Implementation Tasks

| Task | Description | Estimate |
|------|-------------|----------|
| DP-01 | Create Dockerfile templates | M |
| DP-02 | Create docker-compose.yml for local dev | S |
| DP-03 | Implement configuration loading from YAML/env | M |
| DP-04 | Set up NATS and Redis for local dev | S |
| DP-05 | Create deployment scripts | M |
| DP-06 | Set up basic monitoring (health checks) | M |
| DP-07 | Document deployment procedures | M |
| DP-08 | Create staging environment | L |
| DP-09 | Set up CI/CD pipeline (optional) | L |

---

## 5. Timeline Overview

### Recommended Execution Order

```
Phase 1: Foundation (Weeks 1-2)
├── NM-01 to NM-05: Begin naming cleanup
├── DP-01 to DP-04: Local deployment setup
└── OA-01, OA-08: Oanda scaffold, fix credentials

Phase 2: Oanda Trading (Weeks 3-5)
├── OA-02 to OA-07: Oanda trading implementation
├── OA-09 to OA-10: Testing and docs
└── NM-06 to NM-10: Complete naming cleanup

Phase 3: IB Integration (Weeks 6-10)
├── IB-01 to IB-03: IB scaffold and ref data
├── IB-04 to IB-07: IB trading implementation
└── IB-08 to IB-10: Testing and docs

Phase 4: Production Readiness (Weeks 11-12)
├── DP-05 to DP-09: Deployment and monitoring
└── Final integration testing
```

### Dependencies

```
┌─────────────────┐
│ Naming Cleanup  │──────┐
└─────────────────┘      │
                         ▼
┌─────────────────┐    ┌─────────────────┐
│ Oanda Trading   │    │ Deployment Base │
└─────────────────┘    └─────────────────┘
         │                      │
         ▼                      ▼
┌─────────────────┐    ┌─────────────────┐
│ IB Integration  │───►│ Production Env  │
└─────────────────┘    └─────────────────┘
```

---

## Appendix A: Risk Considerations

| Risk | Impact | Mitigation |
|------|--------|------------|
| IB API complexity | Schedule delay | Start with minimal order types, iterate |
| Exchange credential exposure | Security breach | Use secrets manager, never commit creds |
| Naming migration breaks imports | Runtime errors | Comprehensive testing, compatibility aliases |
| Network issues during trading | Failed orders | Implement reconnection, order state recovery |

---

## Appendix B: Open Questions

1. **TokkaQuant API name**: Keep `TokkaQuant` as user-facing API name or rename to `ZKQuant`?
2. **IB data subscriptions**: Budget for real-time market data vs delayed?
3. **Multi-account support**: Priority for managing multiple IB/Oanda accounts?
4. **Backtesting integration**: Include in initial deployment or separate phase?

---

*Last Updated: 2025-01-24*
