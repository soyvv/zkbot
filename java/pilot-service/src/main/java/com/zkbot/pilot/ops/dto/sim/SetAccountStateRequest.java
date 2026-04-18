package com.zkbot.pilot.ops.dto.sim;

import java.util.List;

public record SetAccountStateRequest(
        long accountId,
        List<BalanceMutation> balanceMutations,
        List<PositionMutation> positionMutations,
        boolean clearExistingBalances,
        boolean clearExistingPositions,
        boolean publishUpdates,
        String reason
) {
    public record BalanceMutation(String asset, double totalQty, double availQty, double frozenQty, String mode) {}
    public record PositionMutation(String instrumentCode, int longShortType, double totalQty, double availQty, double frozenQty, String mode) {}
}
