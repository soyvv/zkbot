use std::collections::HashMap;

/// Compact handle into the intern table. Plain u32, not a newtype.
pub type InternedStrId = u32;

/// Append-only string intern table.
/// Intern once at config load or boundary decode. Use integer IDs in hot state.
pub struct InternTable {
    ids_by_str: HashMap<Box<str>, u32>,
    strs_by_id: Vec<Box<str>>,
}

impl InternTable {
    pub fn new() -> Self {
        Self {
            ids_by_str: HashMap::new(),
            strs_by_id: Vec::new(),
        }
    }

    /// Intern a string, returning its ID. Returns existing ID if already interned.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids_by_str.get(s) {
            return id;
        }
        let id = self.strs_by_id.len() as u32;
        let boxed: Box<str> = s.into();
        self.strs_by_id.push(boxed.clone());
        self.ids_by_str.insert(boxed, id);
        id
    }

    /// Resolve an ID back to its string. Panics if ID is invalid.
    pub fn resolve(&self, id: u32) -> &str {
        &self.strs_by_id[id as usize]
    }

    /// Try to resolve, returning None for invalid IDs.
    pub fn try_resolve(&self, id: u32) -> Option<&str> {
        self.strs_by_id.get(id as usize).map(|s| &**s)
    }

    /// Look up the ID for a string without interning it. Returns None if not interned.
    pub fn lookup(&self, s: &str) -> Option<u32> {
        self.ids_by_str.get(s).copied()
    }

    /// Number of interned strings.
    pub fn len(&self) -> usize {
        self.strs_by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strs_by_id.is_empty()
    }
}

impl Default for InternTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_resolve_roundtrip() {
        let mut table = InternTable::new();
        let id = table.intern("BTC-USDT");
        assert_eq!(table.resolve(id), "BTC-USDT");

        let id2 = table.intern("ETH-USDT");
        assert_eq!(table.resolve(id2), "ETH-USDT");
        assert_ne!(id, id2);
    }

    #[test]
    fn dedup_same_string_returns_same_id() {
        let mut table = InternTable::new();
        let id1 = table.intern("BTC-USDT");
        let id2 = table.intern("BTC-USDT");
        assert_eq!(id1, id2);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        let mut table = InternTable::new();
        assert_eq!(table.lookup("BTC-USDT"), None);

        table.intern("ETH-USDT");
        assert_eq!(table.lookup("BTC-USDT"), None);
        assert_eq!(table.lookup("ETH-USDT"), Some(0));
    }
}
