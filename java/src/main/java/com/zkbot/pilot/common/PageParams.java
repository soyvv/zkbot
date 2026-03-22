package com.zkbot.pilot.common;

public record PageParams(int limit, String cursor, String sortBy, String sortOrder) {
    public PageParams {
        if (limit <= 0) limit = 50;
        if (limit > 500) limit = 500;
        if (sortOrder == null) sortOrder = "desc";
    }

    public static PageParams defaults() {
        return new PageParams(50, null, null, "desc");
    }
}
