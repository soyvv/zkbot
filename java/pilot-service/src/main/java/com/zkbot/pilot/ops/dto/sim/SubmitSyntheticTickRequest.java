package com.zkbot.pilot.ops.dto.sim;

import com.fasterxml.jackson.annotation.JsonInclude;
import java.util.List;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record SubmitSyntheticTickRequest(
        String tickType,
        String instrumentCode,
        String exchange,
        Long originalTimestamp,
        Double volume,
        Double latestTradePrice,
        Double latestTradeQty,
        String latestTradeSide,
        List<PriceLevelEntry> buyPriceLevels,
        List<PriceLevelEntry> sellPriceLevels,
        String sourceTag,
        String reason
) {
    public record PriceLevelEntry(double price, double qty) {}
}
