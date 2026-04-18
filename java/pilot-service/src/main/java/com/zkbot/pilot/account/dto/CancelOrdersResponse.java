package com.zkbot.pilot.account.dto;

import java.util.List;

public record CancelOrdersResponse(long accountId, List<CancelOrderResult> results) {}
