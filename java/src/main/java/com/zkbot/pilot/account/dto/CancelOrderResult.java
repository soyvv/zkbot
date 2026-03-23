package com.zkbot.pilot.account.dto;

public record CancelOrderResult(long orderId, String status, String message) {}
