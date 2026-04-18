package com.zkbot.pilot.ops.dto.sim;

public record ForceMatchRequest(
        long orderId,
        String fillMode,
        Double fillQty,
        Double fillPrice,
        String reason
) {}
