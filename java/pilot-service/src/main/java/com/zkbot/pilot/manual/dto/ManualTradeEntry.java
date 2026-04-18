package com.zkbot.pilot.manual.dto;

import com.fasterxml.jackson.databind.annotation.JsonSerialize;
import com.fasterxml.jackson.databind.ser.std.ToStringSerializer;

public record ManualTradeEntry(
        @JsonSerialize(using = ToStringSerializer.class) long orderId,
        String instrument,
        String side,
        double filledQty,
        double filledPrice,
        @JsonSerialize(using = ToStringSerializer.class) long filledTs,
        @JsonSerialize(using = ToStringSerializer.class) long accountId
) {}
