package com.zkbot.pilot.refdata;

import com.zkbot.pilot.discovery.DiscoveryResolver;
import com.zkbot.pilot.grpc.RefdataGrpcClient;
import com.zkbot.pilot.refdata.dto.InstrumentRefdataEntry;
import com.zkbot.pilot.refdata.dto.RefreshRunDto;
import io.grpc.Status;
import io.grpc.StatusRuntimeException;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;
import zk.discovery.v1.Discovery.ServiceRegistration;
import zk.refdata.v1.Refdata;

import java.sql.Timestamp;
import java.util.List;
import java.util.Map;

@Service
public class RefdataService {

    private static final Logger log = LoggerFactory.getLogger(RefdataService.class);

    private final RefdataRepository repository;
    private final RefdataGrpcClient grpcClient;
    private final DiscoveryResolver discoveryResolver;

    public RefdataService(RefdataRepository repository,
                          RefdataGrpcClient grpcClient,
                          DiscoveryResolver discoveryResolver) {
        this.repository = repository;
        this.grpcClient = grpcClient;
        this.discoveryResolver = discoveryResolver;
    }

    public InstrumentRefdataEntry getInstrument(String instrumentId) {
        var row = repository.getInstrumentById(instrumentId);
        if (row == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND, "Instrument not found: " + instrumentId);
        }
        return toEntry(row);
    }

    public List<InstrumentRefdataEntry> searchInstruments(String q, String venue, String type,
                                                          String status, boolean enabledOnly,
                                                          int limit) {
        return repository.searchInstruments(q, venue, type, status, enabledOnly, limit)
                .stream().map(RefdataService::toEntry).toList();
    }

    public RefreshRunDto triggerVenueRefresh(String venue) {
        if (venue == null || venue.isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "venue is required");
        }
        String logicalId = resolveRefdataLogicalId();
        try {
            Refdata.RefreshRunResponse resp = grpcClient.triggerVenueRefresh(logicalId, venue);
            return toDto(resp);
        } catch (StatusRuntimeException e) {
            throw mapGrpcError(e);
        }
    }

    public RefreshRunDto getRefreshRun(long runId) {
        if (runId <= 0) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "runId is required");
        }
        String logicalId = resolveRefdataLogicalId();
        try {
            Refdata.RefreshRunResponse resp = grpcClient.getRefreshRun(logicalId, runId);
            return toDto(resp);
        } catch (StatusRuntimeException e) {
            throw mapGrpcError(e);
        }
    }

    private String resolveRefdataLogicalId() {
        // Refdata-svc registers as svc.refdata.<logical_id>; pick the first one
        // visible in discovery. Multi-refdata environments are out of scope.
        List<ServiceRegistration> regs = discoveryResolver.findByType("refdata");
        if (regs == null || regs.isEmpty()) {
            log.warn("no refdata service registered in discovery");
            throw new ResponseStatusException(HttpStatus.SERVICE_UNAVAILABLE,
                    "no refdata service registered");
        }
        return regs.get(0).getServiceId();
    }

    private static RefreshRunDto toDto(Refdata.RefreshRunResponse r) {
        return new RefreshRunDto(
                r.getRunId(),
                r.getVenue(),
                r.getStatus(),
                r.getAdded(),
                r.getUpdated(),
                r.getDisabled(),
                r.getErrorDetail(),
                r.getStartedAtMs(),
                r.getCompletedAtMs()
        );
    }

    private static ResponseStatusException mapGrpcError(StatusRuntimeException e) {
        Status.Code code = e.getStatus().getCode();
        String detail = e.getStatus().getDescription();
        return switch (code) {
            case NOT_FOUND -> new ResponseStatusException(HttpStatus.NOT_FOUND, detail, e);
            case FAILED_PRECONDITION -> new ResponseStatusException(HttpStatus.CONFLICT, detail, e);
            case INVALID_ARGUMENT -> new ResponseStatusException(HttpStatus.BAD_REQUEST, detail, e);
            case UNAVAILABLE -> new ResponseStatusException(HttpStatus.SERVICE_UNAVAILABLE, detail, e);
            default -> new ResponseStatusException(HttpStatus.BAD_GATEWAY,
                    "refdata gRPC error: " + code + " " + (detail == null ? "" : detail), e);
        };
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
