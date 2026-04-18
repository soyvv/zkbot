package com.zkbot.pilot.ops.dto;

import java.time.Instant;

public record ReconRunEntry(
        long reconRunId,
        String reconType,
        Long accountId,
        String omsId,
        Instant periodStart,
        Instant periodEnd,
        int omsCount,
        int exchCount,
        int matchedCount,
        int omsOnlyCount,
        int exchOnlyCount,
        int amendedCount,
        String status,
        Instant runAt
) {}
