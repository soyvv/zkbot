package com.zkbot.pilot.meta;

import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.web.bind.annotation.*;
import org.springframework.web.server.ResponseStatusException;

import java.util.Map;
import java.util.Set;

@RestController
@RequestMapping("/v1/meta")
public class MetaController {

    private final MetaService service;
    private final VenueManifestLoader venueManifestLoader;

    public MetaController(MetaService service, VenueManifestLoader venueManifestLoader) {
        this.service = service;
        this.venueManifestLoader = venueManifestLoader;
    }

    /**
     * Return UI-facing metadata: enums (static constants) + refs (aggregated from DB + KV).
     * Optional ?domains=manual,accounts,topology,bot to filter refs.
     */
    @GetMapping
    public Map<String, Object> getMeta(@RequestParam(required = false) Set<String> domains) {
        return service.getMeta(domains);
    }

    /**
     * Return JSON Schema for a venue capability's config.
     * Used by UI to render dynamic config forms for GW/MDGW onboarding.
     */
    @GetMapping(value = "/venues/{venueId}/schema/{capability}", produces = MediaType.APPLICATION_JSON_VALUE)
    public String getVenueConfigSchema(@PathVariable String venueId,
                                        @PathVariable String capability) {
        String schema = venueManifestLoader.getConfigSchema(venueId, capability);
        if (schema == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "No config schema for venue=" + venueId + " capability=" + capability);
        }
        return schema;
    }
}
