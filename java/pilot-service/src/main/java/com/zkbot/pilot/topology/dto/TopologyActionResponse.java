package com.zkbot.pilot.topology.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record TopologyActionResponse(String status, String subject, String note) {

    public TopologyActionResponse(String status) {
        this(status, null, null);
    }
}
