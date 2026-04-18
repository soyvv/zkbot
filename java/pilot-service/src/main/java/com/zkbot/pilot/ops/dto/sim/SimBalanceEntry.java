package com.zkbot.pilot.ops.dto.sim;

public record SimBalanceEntry(String asset, double totalQty, double availQty, double frozenQty) {}
