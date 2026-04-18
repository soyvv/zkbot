package com.zkbot.pilot.manual.dto;

import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.databind.annotation.JsonSerialize;
import com.fasterxml.jackson.databind.ser.std.ToStringSerializer;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record OrderDetailEntry(
        @JsonSerialize(using = ToStringSerializer.class) long orderId,
        String instrument,
        String side,
        String status,
        double price,
        double qty,
        double filledQty,
        double filledAvgPrice,
        @JsonSerialize(using = ToStringSerializer.class) long accountId,
        @JsonSerialize(using = ToStringSerializer.class) long createdAt,
        @JsonSerialize(using = ToStringSerializer.class) long updatedAt,
        String errorMsg
) {}
