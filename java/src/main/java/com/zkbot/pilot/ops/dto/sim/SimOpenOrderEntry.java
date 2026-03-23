package com.zkbot.pilot.ops.dto.sim;

public record SimOpenOrderEntry(
        long orderId,
        String exchOrderRef,
        long accountId,
        String instrument,
        int side,
        double remainingQty,
        double filledQty,
        double limitPrice,
        long createdTs,
        long updatedTs
) {}
