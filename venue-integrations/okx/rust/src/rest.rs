use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::{Quota, RateLimiter as GovRateLimiter};
use reqwest::{Client, Method, Response};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

use crate::auth;
use crate::config::OkxConfig;

// ── Rate limiter type alias ─────────────────────────────────────────────────
type RateLimiter = GovRateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
    governor::middleware::NoOpMiddleware,
>;

fn make_limiter(per_second: u32) -> Arc<RateLimiter> {
    let quota = Quota::per_second(NonZeroU32::new(per_second).unwrap());
    Arc::new(GovRateLimiter::direct(quota))
}

// ── OKX JSON envelope ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct OkxResponse<T> {
    pub code: String,
    pub msg: String,
    pub data: Vec<T>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct OkxErrorContext {
    code: String,
    msg: String,
    item_code: Option<String>,
    item_msg: Option<String>,
    ord_id: Option<String>,
    cl_ord_id: Option<String>,
}

fn first_non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_error_context(text: &str) -> Option<OkxErrorContext> {
    let value: Value = serde_json::from_str(text).ok()?;
    let data0 = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first());

    Some(OkxErrorContext {
        code: first_non_empty(value.get("code")).unwrap_or_else(|| "?".to_string()),
        msg: first_non_empty(value.get("msg")).unwrap_or_else(|| "unknown".to_string()),
        item_code: data0
            .and_then(|item| first_non_empty(item.get("sCode")).or_else(|| first_non_empty(item.get("code")))),
        item_msg: data0
            .and_then(|item| first_non_empty(item.get("sMsg")).or_else(|| first_non_empty(item.get("msg")))),
        ord_id: data0.and_then(|item| first_non_empty(item.get("ordId"))),
        cl_ord_id: data0.and_then(|item| first_non_empty(item.get("clOrdId"))),
    })
}

fn format_error_context(path: &str, status: Option<reqwest::StatusCode>, ctx: &OkxErrorContext) -> String {
    let mut msg = match status {
        Some(status) => format!("OKX HTTP {status} on {path}: code={}, msg={}", ctx.code, ctx.msg),
        None => format!("OKX API error on {path}: code={}, msg={}", ctx.code, ctx.msg),
    };

    if ctx.item_code.is_some() || ctx.item_msg.is_some() {
        msg.push_str("; item");
        if let Some(item_code) = &ctx.item_code {
            msg.push_str(&format!(" sCode={item_code}"));
        }
        if let Some(item_msg) = &ctx.item_msg {
            msg.push_str(&format!(", sMsg={item_msg}"));
        }
    }
    if let Some(ord_id) = &ctx.ord_id {
        msg.push_str(&format!(", ordId={ord_id}"));
    }
    if let Some(cl_ord_id) = &ctx.cl_ord_id {
        msg.push_str(&format!(", clOrdId={cl_ord_id}"));
    }
    msg
}

// ── OKX data types (raw API shapes) ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxPlaceData {
    pub ord_id: String,
    pub cl_ord_id: String,
    pub s_code: String,
    pub s_msg: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxCancelData {
    pub ord_id: String,
    pub cl_ord_id: String,
    pub s_code: String,
    pub s_msg: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxOrderData {
    pub inst_id: String,
    pub ord_id: String,
    pub cl_ord_id: String,
    pub side: String,
    pub sz: String,
    pub fill_sz: String,
    #[serde(default)]
    pub avg_px: String,
    pub state: String,
    #[serde(default)]
    pub u_time: String,
    #[serde(default)]
    pub fee: String,
    #[serde(default)]
    pub fee_ccy: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxTradeData {
    pub inst_id: String,
    pub trade_id: String,
    pub ord_id: String,
    pub cl_ord_id: String,
    pub side: String,
    pub fill_sz: String,
    pub fill_px: String,
    pub ts: String,
    #[serde(default)]
    pub fee: String,
    #[serde(default)]
    pub fee_ccy: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxBalanceDetail {
    pub ccy: String,
    #[serde(default)]
    pub cash_bal: String,
    #[serde(default)]
    pub avail_bal: String,
    #[serde(default)]
    pub frozen_bal: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxPositionData {
    pub inst_id: String,
    #[serde(default)]
    pub pos_side: String,
    #[serde(default)]
    pub pos: String,
    #[serde(default)]
    pub avail_pos: String,
}

// ── Public market data types ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxTickerData {
    pub inst_id: String,
    pub last: String,
    #[serde(default)]
    pub last_sz: String,
    #[serde(default)]
    pub bid_px: String,
    #[serde(default)]
    pub bid_sz: String,
    #[serde(default)]
    pub ask_px: String,
    #[serde(default)]
    pub ask_sz: String,
    #[serde(default)]
    pub vol24h: String,
    #[serde(default)]
    pub ts: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OkxFundingData {
    pub inst_id: String,
    #[serde(default)]
    pub funding_rate: String,
    #[serde(default)]
    pub next_funding_rate: String,
    #[serde(default)]
    pub funding_time: String,
    #[serde(default)]
    pub next_funding_time: String,
}

// ── REST client ─────────────────────────────────────────────────────────────

pub struct OkxRestClient {
    http: Client,
    config: OkxConfig,
    order_limiter: Arc<RateLimiter>,   // 30/s (60/2s)
    query_limiter: Arc<RateLimiter>,   // 30/s
    account_limiter: Arc<RateLimiter>, // 5/s (10/2s)
}

impl OkxRestClient {
    pub fn new(config: OkxConfig) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build reqwest client");
        Self {
            http,
            config,
            order_limiter: make_limiter(30),
            query_limiter: make_limiter(30),
            account_limiter: make_limiter(5),
        }
    }

    // ── Order operations ────────────────────────────────────────────────

    pub async fn place_order(
        &self,
        inst_id: &str,
        side: &str,
        ord_type: &str,
        sz: &str,
        px: Option<&str>,
        cl_ord_id: &str,
        td_mode: &str,
        pos_side: Option<&str>,
    ) -> anyhow::Result<OkxPlaceData> {
        self.order_limiter.until_ready().await;

        let mut body = serde_json::json!({
            "instId": inst_id,
            "tdMode": td_mode,
            "side": side,
            "ordType": ord_type,
            "sz": sz,
            "clOrdId": cl_ord_id,
        });
        if let Some(price) = px {
            body["px"] = serde_json::Value::String(price.to_string());
        }
        if let Some(pos_side) = pos_side.filter(|s| !s.trim().is_empty()) {
            body["posSide"] = serde_json::Value::String(pos_side.to_string());
        }

        let resp: OkxResponse<OkxPlaceData> =
            self.signed_request(Method::POST, "/api/v5/trade/order", &body.to_string()).await?;

        let item = resp.data.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!("OKX place order: empty data array, code={}, msg={}", resp.code, resp.msg)
        })?;

        if item.s_code != "0" {
            warn!(
                path = "/api/v5/trade/order",
                inst_id,
                side,
                ord_type,
                sz,
                px = px.unwrap_or(""),
                cl_ord_id,
                pos_side = pos_side.unwrap_or(""),
                s_code = item.s_code,
                s_msg = item.s_msg,
                ord_id = item.ord_id,
                "OKX place order rejected"
            );
            anyhow::bail!(
                "OKX place order rejected: sCode={}, sMsg={}, ordId={}, clOrdId={}",
                item.s_code,
                item.s_msg,
                item.ord_id,
                item.cl_ord_id
            );
        }
        Ok(item)
    }

    pub async fn cancel_order(
        &self,
        inst_id: &str,
        ord_id: &str,
    ) -> anyhow::Result<OkxCancelData> {
        self.order_limiter.until_ready().await;

        let body = serde_json::json!({
            "instId": inst_id,
            "ordId": ord_id,
        });

        let resp: OkxResponse<OkxCancelData> =
            self.signed_request(Method::POST, "/api/v5/trade/cancel-order", &body.to_string()).await?;

        let item = resp.data.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!("OKX cancel order: empty data, code={}, msg={}", resp.code, resp.msg)
        })?;

        if item.s_code != "0" {
            warn!(
                path = "/api/v5/trade/cancel-order",
                inst_id,
                ord_id,
                s_code = item.s_code,
                s_msg = item.s_msg,
                resp_ord_id = item.ord_id,
                cl_ord_id = item.cl_ord_id,
                "OKX cancel rejected"
            );
            anyhow::bail!(
                "OKX cancel rejected: sCode={}, sMsg={}, ordId={}, clOrdId={}",
                item.s_code,
                item.s_msg,
                item.ord_id,
                item.cl_ord_id
            );
        }
        Ok(item)
    }

    // ── Query operations ────────────────────────────────────────────────

    pub async fn query_order(
        &self,
        inst_id: &str,
        ord_id: Option<&str>,
        cl_ord_id: Option<&str>,
    ) -> anyhow::Result<Vec<OkxOrderData>> {
        self.query_limiter.until_ready().await;

        let mut path = format!("/api/v5/trade/order?instId={inst_id}");
        if let Some(oid) = ord_id {
            path.push_str(&format!("&ordId={oid}"));
        }
        if let Some(cid) = cl_ord_id {
            path.push_str(&format!("&clOrdId={cid}"));
        }

        let resp: OkxResponse<OkxOrderData> =
            self.signed_get(&path).await?;
        Ok(resp.data)
    }

    pub async fn query_open_orders(
        &self,
        inst_type: Option<&str>,
    ) -> anyhow::Result<Vec<OkxOrderData>> {
        self.query_limiter.until_ready().await;

        let path = if let Some(it) = inst_type {
            format!("/api/v5/trade/orders-pending?instType={it}")
        } else {
            "/api/v5/trade/orders-pending".to_string()
        };

        let resp: OkxResponse<OkxOrderData> = self.signed_get(&path).await?;
        Ok(resp.data)
    }

    pub async fn query_trades(
        &self,
        inst_type: Option<&str>,
        inst_id: Option<&str>,
    ) -> anyhow::Result<Vec<OkxTradeData>> {
        self.query_limiter.until_ready().await;

        let mut path = "/api/v5/trade/fills?".to_string();
        if let Some(it) = inst_type {
            path.push_str(&format!("instType={it}&"));
        }
        if let Some(id) = inst_id {
            path.push_str(&format!("instId={id}&"));
        }
        path.pop(); // trailing & or ?

        let resp: OkxResponse<OkxTradeData> = self.signed_get(&path).await?;
        Ok(resp.data)
    }

    pub async fn query_balance(
        &self,
        ccy: Option<&str>,
    ) -> anyhow::Result<Vec<OkxBalanceDetail>> {
        self.account_limiter.until_ready().await;

        let path = if let Some(c) = ccy {
            format!("/api/v5/account/balance?ccy={c}")
        } else {
            "/api/v5/account/balance".to_string()
        };

        // OKX balance response wraps details in a nested structure
        let resp: OkxResponse<OkxBalanceWrapper> = self.signed_get(&path).await?;
        let details: Vec<OkxBalanceDetail> = resp
            .data
            .into_iter()
            .flat_map(|w| w.details)
            .collect();
        Ok(details)
    }

    pub async fn query_positions(
        &self,
        inst_type: Option<&str>,
    ) -> anyhow::Result<Vec<OkxPositionData>> {
        self.account_limiter.until_ready().await;

        let path = if let Some(it) = inst_type {
            format!("/api/v5/account/positions?instType={it}")
        } else {
            "/api/v5/account/positions".to_string()
        };

        let resp: OkxResponse<OkxPositionData> = self.signed_get(&path).await?;
        Ok(resp.data)
    }

    // ── Public market data (unsigned) ──────────────────────────────────

    /// GET /api/v5/market/ticker (public, unsigned)
    pub async fn query_ticker(&self, inst_id: &str) -> anyhow::Result<OkxTickerData> {
        let path = format!("/api/v5/market/ticker?instId={inst_id}");
        let resp: OkxResponse<OkxTickerData> = self.public_get(&path).await?;
        resp.data.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!("OKX ticker: empty data for {inst_id}")
        })
    }

    /// GET /api/v5/market/books (public, unsigned). Returns raw Value for normalizer.
    pub async fn query_orderbook(
        &self,
        inst_id: &str,
        depth: Option<u32>,
    ) -> anyhow::Result<serde_json::Value> {
        let sz = depth.unwrap_or(5);
        let path = format!("/api/v5/market/books?instId={inst_id}&sz={sz}");
        let resp: OkxResponse<serde_json::Value> = self.public_get(&path).await?;
        resp.data.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!("OKX orderbook: empty data for {inst_id}")
        })
    }

    /// GET /api/v5/market/candles (public, unsigned). Returns raw array items for normalizer.
    /// OKX `after` = return records before this timestamp, `before` = return records after.
    pub async fn query_candles(
        &self,
        inst_id: &str,
        bar: &str,
        limit: Option<u32>,
        after_ms: Option<i64>,
        before_ms: Option<i64>,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let limit = limit.unwrap_or(100);
        let mut path = format!("/api/v5/market/candles?instId={inst_id}&bar={bar}&limit={limit}");
        if let Some(after) = after_ms {
            path.push_str(&format!("&after={after}"));
        }
        if let Some(before) = before_ms {
            path.push_str(&format!("&before={before}"));
        }
        let resp: OkxResponse<serde_json::Value> = self.public_get(&path).await?;
        Ok(resp.data)
    }

    /// GET /api/v5/public/funding-rate (public, unsigned)
    pub async fn query_funding_rate(&self, inst_id: &str) -> anyhow::Result<OkxFundingData> {
        let path = format!("/api/v5/public/funding-rate?instId={inst_id}");
        let resp: OkxResponse<OkxFundingData> = self.public_get(&path).await?;
        resp.data.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!("OKX funding rate: empty data for {inst_id}")
        })
    }

    // ── Internal helpers ────────────────────────────────────────────────

    async fn public_get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<OkxResponse<T>> {
        self.query_limiter.until_ready().await;
        let url = format!("{}{}", self.config.api_base_url, path);

        let mut req = self.http.get(&url).header("Content-Type", "application/json");
        if self.config.demo_mode {
            req = req.header("x-simulated-trading", "1");
        }

        let resp: Response = req.send().await?;
        let status = resp.status();

        if status.as_u16() == 429 {
            warn!("OKX rate limit hit (429) on {path}");
            anyhow::bail!("OKX rate limit exceeded on {path}");
        }

        let text = resp.text().await?;
        debug!(path, status = %status, "OKX public response");

        if !status.is_success() {
            if let Some(ctx) = parse_error_context(&text) {
                warn!(
                    path,
                    %status,
                    code = ctx.code,
                    msg = ctx.msg,
                    item_code = ctx.item_code.as_deref().unwrap_or(""),
                    item_msg = ctx.item_msg.as_deref().unwrap_or(""),
                    ord_id = ctx.ord_id.as_deref().unwrap_or(""),
                    cl_ord_id = ctx.cl_ord_id.as_deref().unwrap_or(""),
                    "OKX public HTTP error"
                );
                anyhow::bail!("{}", format_error_context(path, Some(status), &ctx));
            }
            anyhow::bail!("OKX HTTP {status} on {path}, body_len={}", text.len());
        }

        let parsed: OkxResponse<T> = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!("OKX response parse error on {path}: {e}, body_len={}", text.len())
        })?;

        if parsed.code != "0" {
            let ctx = parse_error_context(&text).unwrap_or(OkxErrorContext {
                code: parsed.code,
                msg: parsed.msg,
                ..Default::default()
            });
            warn!(
                path,
                code = ctx.code,
                msg = ctx.msg,
                item_code = ctx.item_code.as_deref().unwrap_or(""),
                item_msg = ctx.item_msg.as_deref().unwrap_or(""),
                ord_id = ctx.ord_id.as_deref().unwrap_or(""),
                cl_ord_id = ctx.cl_ord_id.as_deref().unwrap_or(""),
                "OKX public API error"
            );
            anyhow::bail!("{}", format_error_context(path, None, &ctx));
        }

        Ok(parsed)
    }

    async fn signed_get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<OkxResponse<T>> {
        self.signed_request(Method::GET, path, "").await
    }

    async fn signed_request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &str,
    ) -> anyhow::Result<OkxResponse<T>> {
        let timestamp = iso_timestamp();
        let method_str = method.as_str();
        let signature = auth::sign_rest(&self.config.secret_key, &timestamp, method_str, path, body);

        let url = format!("{}{}", self.config.api_base_url, path);

        let mut req = self
            .http
            .request(method.clone(), &url)
            .header("OK-ACCESS-KEY", &self.config.api_key)
            .header("OK-ACCESS-SIGN", &signature)
            .header("OK-ACCESS-TIMESTAMP", &timestamp)
            .header("OK-ACCESS-PASSPHRASE", &self.config.passphrase)
            .header("Content-Type", "application/json");

        if self.config.demo_mode {
            req = req.header("x-simulated-trading", "1");
        }

        if method_str != "GET" && !body.is_empty() {
            req = req.body(body.to_string());
        }

        let resp: Response = req.send().await?;
        let status = resp.status();

        if status.as_u16() == 429 {
            warn!("OKX rate limit hit (429) on {path}");
            anyhow::bail!("OKX rate limit exceeded on {path}");
        }

        let text = resp.text().await?;
        debug!(path, status = %status, "OKX response received");

        if !status.is_success() {
            if let Some(ctx) = parse_error_context(&text) {
                warn!(
                    path,
                    %status,
                    code = ctx.code,
                    msg = ctx.msg,
                    item_code = ctx.item_code.as_deref().unwrap_or(""),
                    item_msg = ctx.item_msg.as_deref().unwrap_or(""),
                    ord_id = ctx.ord_id.as_deref().unwrap_or(""),
                    cl_ord_id = ctx.cl_ord_id.as_deref().unwrap_or(""),
                    "OKX signed HTTP error"
                );
                anyhow::bail!("{}", format_error_context(path, Some(status), &ctx));
            }
            anyhow::bail!("OKX HTTP {status} on {path}, body_len={}", text.len());
        }

        let parsed: OkxResponse<T> = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!("OKX response parse error on {path}: {e}, body_len={}", text.len())
        })?;

        if parsed.code != "0" {
            let ctx = parse_error_context(&text).unwrap_or(OkxErrorContext {
                code: parsed.code,
                msg: parsed.msg,
                ..Default::default()
            });
            warn!(
                path,
                code = ctx.code,
                msg = ctx.msg,
                item_code = ctx.item_code.as_deref().unwrap_or(""),
                item_msg = ctx.item_msg.as_deref().unwrap_or(""),
                ord_id = ctx.ord_id.as_deref().unwrap_or(""),
                cl_ord_id = ctx.cl_ord_id.as_deref().unwrap_or(""),
                "OKX signed API error"
            );
            anyhow::bail!("{}", format_error_context(path, None, &ctx));
        }

        Ok(parsed)
    }
}

/// Nested wrapper for GET /api/v5/account/balance response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxBalanceWrapper {
    #[serde(default)]
    details: Vec<OkxBalanceDetail>,
}

/// ISO-8601 timestamp for OKX signing (e.g. "2024-01-01T00:00:00.123Z").
fn iso_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // Convert to date/time manually to avoid adding chrono dependency.
    // We need: YYYY-MM-DDTHH:MM:SS.mmmZ
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01 to Y-M-D (simplified civil_from_days)
    let (y, m, d) = civil_from_days(days as i64);

    format!(
        "{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z"
    )
}

#[cfg(test)]
mod tests {
    use super::{format_error_context, parse_error_context, OkxErrorContext};

    #[test]
    fn parse_error_context_extracts_item_level_fields() {
        let text = r#"{
          "code":"1",
          "msg":"All operations failed",
          "data":[{"clOrdId":"7442579191356194816","ordId":"","sCode":"51008","sMsg":"Order failed. Insufficient margin."}]
        }"#;
        let ctx = parse_error_context(text).expect("expected parsed context");
        assert_eq!(ctx.code, "1");
        assert_eq!(ctx.msg, "All operations failed");
        assert_eq!(ctx.item_code.as_deref(), Some("51008"));
        assert_eq!(ctx.item_msg.as_deref(), Some("Order failed. Insufficient margin."));
        assert_eq!(ctx.cl_ord_id.as_deref(), Some("7442579191356194816"));
    }

    #[test]
    fn format_error_context_includes_item_detail() {
        let ctx = OkxErrorContext {
            code: "1".into(),
            msg: "All operations failed".into(),
            item_code: Some("51008".into()),
            item_msg: Some("Order failed. Insufficient margin.".into()),
            ord_id: Some("123".into()),
            cl_ord_id: Some("456".into()),
        };
        let msg = format_error_context("/api/v5/trade/order", None, &ctx);
        assert!(msg.contains("code=1, msg=All operations failed"));
        assert!(msg.contains("sCode=51008"));
        assert!(msg.contains("sMsg=Order failed. Insufficient margin."));
        assert!(msg.contains("ordId=123"));
        assert!(msg.contains("clOrdId=456"));
    }
}

/// Convert days since epoch to (year, month, day). Adapted from Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
