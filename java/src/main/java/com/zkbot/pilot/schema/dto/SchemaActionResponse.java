package com.zkbot.pilot.schema.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record SchemaActionResponse(String status, Integer version) {

    public SchemaActionResponse(String status) {
        this(status, null);
    }
}
