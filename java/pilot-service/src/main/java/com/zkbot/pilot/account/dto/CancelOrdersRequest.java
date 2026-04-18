package com.zkbot.pilot.account.dto;

import java.util.List;

public record CancelOrdersRequest(List<Long> orderIds) {}
