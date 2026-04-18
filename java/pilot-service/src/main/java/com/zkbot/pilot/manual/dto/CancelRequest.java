package com.zkbot.pilot.manual.dto;

import java.util.List;

public record CancelRequest(
        long accountId,
        List<String> orderIds
) {}
