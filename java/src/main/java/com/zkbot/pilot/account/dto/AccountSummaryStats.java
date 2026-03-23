package com.zkbot.pilot.account.dto;

public record AccountSummaryStats(
    int positionCount,
    int openOrderCount,
    String riskState
) {}
