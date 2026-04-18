package com.zkbot.pilot.ops.dto.sim;

import com.fasterxml.jackson.annotation.JsonInclude;
import java.util.Map;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record InjectErrorRequest(
        String errorId,
        String scope,
        Map<String, Object> matchCriteria,
        String effect,
        Integer grpcStatusCode,
        String errorMessage,
        Long delayMs,
        Integer duplicateCount,
        String triggerPolicy,
        Integer triggerTimes,
        Integer priority,
        Boolean enabled
) {}
