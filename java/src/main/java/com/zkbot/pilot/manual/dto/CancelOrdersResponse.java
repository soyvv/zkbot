package com.zkbot.pilot.manual.dto;

import com.fasterxml.jackson.databind.annotation.JsonSerialize;
import com.fasterxml.jackson.databind.ser.std.ToStringSerializer;

import java.util.List;

public record CancelOrdersResponse(
        String omsId,
        List<CancelResult> results
) {
    public record CancelResult(
            @JsonSerialize(using = ToStringSerializer.class) long orderId,
            OmsActionResponse response) {}
}
