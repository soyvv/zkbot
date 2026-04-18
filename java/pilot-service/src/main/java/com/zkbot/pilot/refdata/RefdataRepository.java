package com.zkbot.pilot.refdata;

import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

@Repository
public class RefdataRepository {

    private final JdbcTemplate jdbc;

    public RefdataRepository(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    public Map<String, Object> getInstrumentById(String instrumentId) {
        List<Map<String, Object>> rows = jdbc.queryForList(
                "SELECT * FROM cfg.instrument_refdata WHERE instrument_id = ?", instrumentId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public List<Map<String, Object>> searchInstruments(String q, String venue, String type,
                                                        boolean enabledOnly, int limit) {
        var sb = new StringBuilder("SELECT * FROM cfg.instrument_refdata WHERE 1=1");
        var params = new ArrayList<>();

        if (enabledOnly) {
            sb.append(" AND NOT disabled");
        }
        if (venue != null && !venue.isBlank()) {
            sb.append(" AND UPPER(venue) = UPPER(?)");
            params.add(venue);
        }
        if (type != null && !type.isBlank()) {
            sb.append(" AND UPPER(instrument_type) = UPPER(?)");
            params.add(type);
        }
        if (q != null && !q.isBlank()) {
            String[] keywords = q.trim().split("\\s+");
            for (String kw : keywords) {
                sb.append(" AND (instrument_id ILIKE ? OR instrument_exch ILIKE ? OR venue ILIKE ? OR base_asset ILIKE ? OR quote_asset ILIKE ?)");
                String pattern = "%" + kw + "%";
                for (int i = 0; i < 5; i++) {
                    params.add(pattern);
                }
            }
        }

        sb.append(" ORDER BY venue, instrument_id LIMIT ?");
        params.add(Math.min(limit, 500));

        return jdbc.queryForList(sb.toString(), params.toArray());
    }
}
