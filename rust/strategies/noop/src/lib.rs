use zk_strategy_sdk_rs::strategy::Strategy;

/// Minimal no-op strategy used as the default placeholder for engine service boot.
#[derive(Debug, Default, Clone)]
pub struct NoopStrategy;

impl NoopStrategy {
    pub fn new() -> Self {
        Self
    }
}

impl Strategy for NoopStrategy {}
