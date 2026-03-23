package com.zkbot.pilot.manual.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record OmsActionResponse(
        String omsId,
        String status,
        long timestamp,
        String message,
        String errorType
) {}
