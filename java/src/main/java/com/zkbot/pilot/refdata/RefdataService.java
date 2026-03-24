package com.zkbot.pilot.refdata;

import com.zkbot.pilot.refdata.dto.InstrumentRefdataEntry;
import org.springframework.http.HttpStatus;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;

import java.sql.Timestamp;
import java.util.List;
import java.util.Map;

@Service
public class RefdataService {

    private final RefdataRepository repository;

    public RefdataService(RefdataRepository repository) {
        this.repository = repository;
    }

    public InstrumentRefdataEntry getInstrument(String instrumentId) {
        var row = repository.getInstrumentById(instrumentId);
        if (row == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, "Instrument not found: " + instrumentId);
        }
        return toEntry(row);
    }

    public List<InstrumentRefdataEntry> searchInstruments(String q, String venue, String type,
                                                          boolean enabledOnly, int limit) {
        return repository.searchInstruments(q, venue, type, enabledOnly, limit)
                .stream().map(RefdataService::toEntry).toList();
    }

    private static InstrumentRefdataEntry toEntry(Map<String, Object> row) {
        return new InstrumentRefdataEntry(
                (String) row.get("instrument_id"),
                (String) row.get("instrument_exch"),
                (String) row.get("venue"),
                (String) row.get("instrument_type"),
                (String) row.get("base_asset"),
                (String) row.get("quote_asset"),
                (String) row.get("settlement_asset"),
                toDouble(row.get("price_tick_size")),
                toDouble(row.get("qty_lot_size")),
                toInt(row.get("price_precision")),
                toInt(row.get("qty_precision")),
                toDouble(row.get("min_order_qty")),
                toDouble(row.get("max_order_qty")),
                Boolean.TRUE.equals(row.get("disabled")) ? "disabled" : "active",
                row.get("updated_at") instanceof Timestamp ts ? ts.getTime() : null
        );
    }

    private static Double toDouble(Object v) {
        return v instanceof Number n ? n.doubleValue() : null;
    }

    private static Integer toInt(Object v) {
        return v instanceof Number n ? n.intValue() : null;
    }
}
