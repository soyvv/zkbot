package com.zkbot.pilot.account.dto;

import com.fasterxml.jackson.databind.annotation.JsonSerialize;
import com.fasterxml.jackson.databind.ser.std.ToStringSerializer;

public record OpenOrderEntry(
        @JsonSerialize(using = ToStringSerializer.class) long orderId,
        String exchOrderRef,
        String instrument,
        String side,
        String status,
        double price,
        double qty,
        double filledQty,
        double filledAvgPrice,
        @JsonSerialize(using = ToStringSerializer.class) long createdAt,
        @JsonSerialize(using = ToStringSerializer.class) long updatedAt
) {}
