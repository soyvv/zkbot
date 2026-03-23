package com.zkbot.pilot.account.dto;

public record AccountBindingResponse(
    String omsId,
    String gwId,
    boolean omsLive,
    String omsAddress,
    boolean gwLive,
    String gwAddress
) {}
