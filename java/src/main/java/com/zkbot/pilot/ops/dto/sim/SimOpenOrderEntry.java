package com.zkbot.pilot.ops.dto.sim;

import com.fasterxml.jackson.databind.annotation.JsonSerialize;
import com.fasterxml.jackson.databind.ser.std.ToStringSerializer;

public record SimOpenOrderEntry(
        @JsonSerialize(using = ToStringSerializer.class) long orderId,
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
