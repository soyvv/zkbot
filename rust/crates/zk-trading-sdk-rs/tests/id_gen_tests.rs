use std::collections::HashSet;
use std::sync::Arc;

use zk_trading_sdk_rs::id_gen::SnowflakeIdGen;

#[test]
fn test_snowflake_instance_id_bounds_rejects_1024() {
    let result = SnowflakeIdGen::new(1024);
    assert!(result.is_err(), "instance_id 1024 must be rejected");
}

#[test]
fn test_snowflake_instance_id_max_accepts_1023() {
    let result = SnowflakeIdGen::new(1023);
    assert!(result.is_ok(), "instance_id 1023 must be accepted");
}

#[test]
fn test_snowflake_id_monotonic_within_thread() {
    let gen = SnowflakeIdGen::new(1).expect("valid instance_id");
    let mut prev = gen.next_id();
    for _ in 0..100 {
        let id = gen.next_id();
        assert!(id > prev, "IDs must be monotonically increasing: {prev} >= {id}");
        prev = id;
    }
}

#[test]
fn test_snowflake_id_uniqueness_parallel() {
    let gen = Arc::new(SnowflakeIdGen::new(5).expect("valid instance_id"));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let g = Arc::clone(&gen);
            std::thread::spawn(move || {
                (0..2500).map(|_| g.next_id()).collect::<Vec<i64>>()
            })
        })
        .collect();

    let all_ids: Vec<i64> = threads
        .into_iter()
        .flat_map(|h| h.join().expect("thread panicked"))
        .collect();

    assert_eq!(all_ids.len(), 10_000, "expected 10_000 IDs");

    let unique: HashSet<i64> = all_ids.iter().copied().collect();
    assert_eq!(unique.len(), 10_000, "all 10_000 IDs must be unique");
}

#[test]
fn test_snowflake_id_encodes_instance_id() {
    let instance_id: u16 = 42;
    let gen = SnowflakeIdGen::new(instance_id).expect("valid instance_id");
    let id = gen.next_id();
    // bits 12-21 (10 bits) encode instance_id
    let extracted = ((id >> 12) & 0x3FF) as u16;
    assert_eq!(extracted, instance_id, "instance_id bits must match");
}
