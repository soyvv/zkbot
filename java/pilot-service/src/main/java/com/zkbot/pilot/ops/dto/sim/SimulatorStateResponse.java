package com.zkbot.pilot.ops.dto.sim;

import com.fasterxml.jackson.annotation.JsonInclude;
import java.util.List;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record SimulatorStateResponse(
        String gwId,
        List<SimOpenOrderEntry> openOrders,
        List<SimBalanceEntry> balances,
        List<SimPositionEntry> positions,
        List<InjectedErrorEntry> activeErrorRules,
        String currentMatchPolicy,
        boolean matchingPaused,
        Long lastTickTs,
        Long lastEventTs
) {}
