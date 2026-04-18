package com.zkbot.pilot.common;

import org.springframework.stereotype.Component;

/**
 * Snowflake order-ID generator matching the Rust implementation in zk-trading-sdk-rs.
 * <p>
 * Format: timestamp_ms (41 bits) | instance_id (10 bits) | sequence (12 bits)
 */
@Component
public class SnowflakeIdGen {

    private static final int MAX_INSTANCE_ID = 1023;  // 10 bits
    private static final int MAX_SEQUENCE = 4095;      // 12 bits
    private static final int INSTANCE_ID_SHIFT = 12;
    private static final int TIMESTAMP_SHIFT = 22;     // 10 + 12

    private final int instanceId;
    private long lastMs = -1;
    private int sequence = 0;

    public SnowflakeIdGen() {
        this(0);
    }

    public SnowflakeIdGen(int instanceId) {
        if (instanceId < 0 || instanceId > MAX_INSTANCE_ID) {
            throw new IllegalArgumentException("instanceId must be 0.." + MAX_INSTANCE_ID + ", got " + instanceId);
        }
        this.instanceId = instanceId;
    }

    public synchronized long nextId() {
        long nowMs = System.currentTimeMillis();
        if (nowMs > lastMs) {
            lastMs = nowMs;
            sequence = 0;
        } else {
            sequence++;
            if (sequence > MAX_SEQUENCE) {
                // Sequence exhausted this ms; spin until clock advances
                while (System.currentTimeMillis() <= lastMs) {
                    Thread.onSpinWait();
                }
                lastMs = System.currentTimeMillis();
                sequence = 0;
            }
        }
        return (lastMs << TIMESTAMP_SHIFT)
                | ((long) instanceId << INSTANCE_ID_SHIFT)
                | sequence;
    }
}
