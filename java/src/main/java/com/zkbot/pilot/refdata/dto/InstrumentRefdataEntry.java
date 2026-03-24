package com.zkbot.pilot.refdata.dto;

import com.fasterxml.jackson.annotation.JsonInclude;

@JsonInclude(JsonInclude.Include.NON_NULL)
public record InstrumentRefdataEntry(
        String instrumentId,
        String instrumentExch,
        String venue,
        String instrumentType,
        String baseAsset,
        String quoteAsset,
        String settlementAsset,
        Double priceTick,
        Double qtyLot,
        Integer pricePrecision,
        Integer qtyPrecision,
        Double minOrderQty,
        Double maxOrderQty,
        String lifecycleStatus,
        Long updatedAt
) {}
