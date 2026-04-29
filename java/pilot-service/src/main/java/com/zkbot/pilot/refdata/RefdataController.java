package com.zkbot.pilot.refdata;

import com.zkbot.pilot.refdata.dto.InstrumentRefdataEntry;
import com.zkbot.pilot.refdata.dto.RefreshRunDto;
import org.springframework.web.bind.annotation.*;

import java.util.List;

@RestController
@RequestMapping("/v1/refdata")
public class RefdataController {

    private final RefdataService service;

    public RefdataController(RefdataService service) {
        this.service = service;
    }

    @GetMapping("/instruments")
    public List<InstrumentRefdataEntry> searchInstruments(
            @RequestParam(required = false) String q,
            @RequestParam(required = false) String venue,
            @RequestParam(required = false) String type,
            @RequestParam(required = false) String status,
            @RequestParam(name = "enabled_only", defaultValue = "true") boolean enabledOnly,
            @RequestParam(defaultValue = "5000") int limit) {
        return service.searchInstruments(q, venue, type, status, enabledOnly, limit);
    }

    @GetMapping("/instruments/{instrumentId}")
    public InstrumentRefdataEntry getInstrument(@PathVariable String instrumentId) {
        return service.getInstrument(instrumentId);
    }

    /**
     * Trigger a manual refresh for a single venue. Returns immediately with the
     * newly-allocated run row (status="running"); clients poll
     * {@link #getRefreshRun(long)} for progress and final counts.
     *
     * <p>Returns 409 (Conflict) if a refresh is already in progress for the venue.
     * Returns 404 if the venue has no resolvable loader.
     */
    @PostMapping("/venues/{venue}/refresh")
    public RefreshRunDto triggerVenueRefresh(@PathVariable String venue) {
        return service.triggerVenueRefresh(venue);
    }

    @GetMapping("/runs/{runId}")
    public RefreshRunDto getRefreshRun(@PathVariable long runId) {
        return service.getRefreshRun(runId);
    }
}
