package com.zkbot.pilot.ops.dto.sim;

import com.fasterxml.jackson.annotation.JsonInclude;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record SimActionResponse(
        boolean accepted,
        String message,
        String errorId,
        Integer clearedCount,
        Integer clearedOrders,
        Integer clearedErrorRules,
        Long matchedOrderId,
        Integer generatedTradeCount,
        Double remainingOpenQty,
        Integer appliedBalanceCount,
        Integer appliedPositionCount,
        Integer affectedOrderCount,
        Integer generatedReportCount,
        Boolean matchingPaused,
        String previousPolicy,
        String currentPolicy
) {
    public static SimActionResponse simple(boolean accepted, String message) {
        return new SimActionResponse(accepted, message, null, null, null, null,
                null, null, null, null, null, null, null, null, null, null);
    }
}
