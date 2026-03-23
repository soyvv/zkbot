package com.zkbot.pilot.ops.dto.sim;

import com.fasterxml.jackson.annotation.JsonInclude;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record SimulatorSummary(
        String gwId,
        String venue,
        boolean live,
        String tradingAddress,
        String adminAddress,
        Boolean adminEnabled
) {}
