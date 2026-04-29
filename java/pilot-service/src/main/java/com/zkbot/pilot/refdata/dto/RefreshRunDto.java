package com.zkbot.pilot.refdata.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

/**
 * REST projection of zk.refdata.v1.RefreshRunResponse for the Refdata UI page.
 */
@JsonInclude(JsonInclude.Include.NON_NULL)
public record RefreshRunDto(
        long runId,
        String venue,
        String status,            // running | completed | failed
        int added,
        int updated,
        int disabled,
        String errorDetail,
        long startedAtMs,
        long completedAtMs
) {}
