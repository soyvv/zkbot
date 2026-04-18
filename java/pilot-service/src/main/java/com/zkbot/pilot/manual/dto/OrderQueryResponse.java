package com.zkbot.pilot.manual.dto;

import java.util.List;

public record OrderQueryResponse(String omsId, List<OrderDetailEntry> orders) {}
