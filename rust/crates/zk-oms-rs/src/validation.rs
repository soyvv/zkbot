use zk_proto_rs::zk::oms::v1::{OrderCancelRequest, OrderRequest};

pub fn validate_order_request(req: &OrderRequest) -> Result<(), &'static str> {
    if req.order_id <= 0 {
        return Err("place_order: order_id must be > 0");
    }
    if req.account_id <= 0 {
        return Err("place_order: account_id must be > 0");
    }
    if req.instrument_code.trim().is_empty() {
        return Err("place_order: instrument_code is required");
    }
    if !req.qty.is_finite() || req.qty <= 0.0 {
        return Err("place_order: qty must be finite and > 0");
    }
    if !req.price.is_finite() {
        return Err("place_order: price must be finite");
    }
    Ok(())
}

pub fn validate_cancel_request(req: &OrderCancelRequest) -> Result<(), &'static str> {
    if req.order_id <= 0 {
        return Err("cancel_order: order_id must be > 0");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_cancel_request, validate_order_request};
    use zk_proto_rs::zk::oms::v1::{OrderCancelRequest, OrderRequest};

    fn sample_order_request() -> OrderRequest {
        OrderRequest {
            order_id: 42,
            account_id: 9001,
            instrument_code: "BTC-USDT".into(),
            qty: 1.0,
            price: 100.0,
            ..Default::default()
        }
    }

    #[test]
    fn order_validation_rejects_zero_order_id() {
        let req = OrderRequest {
            order_id: 0,
            ..sample_order_request()
        };
        assert_eq!(
            validate_order_request(&req),
            Err("place_order: order_id must be > 0")
        );
    }

    #[test]
    fn order_validation_rejects_empty_instrument() {
        let req = OrderRequest {
            instrument_code: " ".into(),
            ..sample_order_request()
        };
        assert_eq!(
            validate_order_request(&req),
            Err("place_order: instrument_code is required")
        );
    }

    #[test]
    fn cancel_validation_rejects_zero_order_id() {
        let req = OrderCancelRequest {
            order_id: 0,
            ..Default::default()
        };
        assert_eq!(
            validate_cancel_request(&req),
            Err("cancel_order: order_id must be > 0")
        );
    }
}
