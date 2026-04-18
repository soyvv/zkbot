package com.zkbot.pilot.manual.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

import java.util.List;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record OrderPreviewResponse(
        boolean valid,
        String error,
        Long accountId,
        String instrumentId,
        String side,
        String orderType,
        String price,
        String qty,
        String timeInForce,
        List<String> warnings
) {}
