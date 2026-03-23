package com.zkbot.pilot.ops.dto.sim;

public record SimPositionEntry(String instrumentCode, int longShortType, double totalQty, double availQty, double frozenQty) {}
