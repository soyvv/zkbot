package com.zkbot.pilot.refdata;

import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

@Repository
public class RefdataRepository {

    private static final int MAX_SEARCH_KEYWORDS = 16;

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
                                                        String status, boolean enabledOnly,
                                                        int limit) {
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
        if (status != null && !status.isBlank()) {
            // lifecycle_status values: 'active' | 'disabled' | 'deprecated'.
            // Stored lowercase; compare case-insensitively for caller convenience.
            sb.append(" AND LOWER(lifecycle_status) = LOWER(?)");
            params.add(status);
        }
        if (q != null && !q.isBlank()) {
            String[] keywords = q.trim().split("\\s+");
            // Bound the number of keyword clauses — a pathological caller could
            // pass thousands of words and balloon the SQL plan.
            int kwLimit = Math.min(keywords.length, MAX_SEARCH_KEYWORDS);
            for (int k = 0; k < kwLimit; k++) {
                String kw = keywords[k];
                sb.append(" AND (instrument_id ILIKE ? OR instrument_exch ILIKE ? OR venue ILIKE ? OR base_asset ILIKE ? OR quote_asset ILIKE ?)");
                String pattern = "%" + kw + "%";
                for (int i = 0; i < 5; i++) {
                    params.add(pattern);
                }
            }
        }

        sb.append(" ORDER BY venue, instrument_id LIMIT ?");
        // Temporary ceiling: full venue universes (e.g. IBKR ~11k) must be reachable.
        // TODO: migrate to AG Grid server-side row model when datasets exceed this
        // (raise a follow-up ticket once a second venue grows past ~20k rows).
        params.add(Math.min(limit, 20000));

        return jdbc.queryForList(sb.toString(), params.toArray());
    }
}
