package com.zkbot.pilot.refdata;

import com.zkbot.pilot.refdata.dto.InstrumentRefdataEntry;
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
            @RequestParam(name = "enabled_only", defaultValue = "true") boolean enabledOnly,
            @RequestParam(defaultValue = "50") int limit) {
        return service.searchInstruments(q, venue, type, enabledOnly, limit);
    }

    @GetMapping("/instruments/{instrumentId}")
    public InstrumentRefdataEntry getInstrument(@PathVariable String instrumentId) {
        return service.getInstrument(instrumentId);
    }
}
