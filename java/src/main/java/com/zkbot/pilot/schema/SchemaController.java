package com.zkbot.pilot.schema;

import com.zkbot.pilot.schema.dto.SchemaResourceEntry;
import com.zkbot.pilot.schema.dto.SchemaActionResponse;
import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.web.bind.annotation.*;
import org.springframework.web.server.ResponseStatusException;

import java.util.List;


@RestController
@RequestMapping("/v1/schema")
public class SchemaController {

    private final SchemaService service;
    private final SchemaRegistrySyncer syncer;

    public SchemaController(SchemaService service, SchemaRegistrySyncer syncer) {
        this.service = service;
        this.syncer = syncer;
    }

    @GetMapping
    public List<SchemaResourceEntry> list(@RequestParam(required = false, name = "type") String resourceType) {
        return service.listResources(resourceType);
    }

    @GetMapping("/service-kinds/{serviceKind}")
    public SchemaResourceEntry getServiceKindManifest(@PathVariable String serviceKind) {
        var manifest = service.getActiveServiceKindManifest(serviceKind);
        if (manifest == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "No active manifest for service kind: " + serviceKind);
        }
        return manifest;
    }

    @GetMapping("/venues/{venueId}/capabilities/{capability}")
    public SchemaResourceEntry getVenueCapabilityManifest(@PathVariable String venueId,
                                                           @PathVariable String capability) {
        var manifest = service.getActiveVenueCapabilityManifest(venueId, capability);
        if (manifest == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "No active manifest for venue=" + venueId + " capability=" + capability);
        }
        return manifest;
    }

    @GetMapping(value = "/service-kinds/{serviceKind}/config", produces = MediaType.APPLICATION_JSON_VALUE)
    public String getServiceKindConfigSchema(@PathVariable String serviceKind) {
        String schema = service.getConfigSchema("service_kind", serviceKind.toLowerCase());
        if (schema == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "No config schema for service kind: " + serviceKind);
        }
        return schema;
    }

    @GetMapping(value = "/venues/{venueId}/capabilities/{capability}/config",
            produces = MediaType.APPLICATION_JSON_VALUE)
    public String getVenueCapabilityConfigSchema(@PathVariable String venueId,
                                                   @PathVariable String capability) {
        String schema = service.getConfigSchema("venue_capability", venueId + "/" + capability);
        if (schema == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "No config schema for venue=" + venueId + " capability=" + capability);
        }
        return schema;
    }

    @PutMapping("/service-kinds/{serviceKind}/versions/{version}/activate")
    public SchemaActionResponse activateServiceKindVersion(@PathVariable String serviceKind,
                                                            @PathVariable int version) {
        service.activateVersion("service_kind", serviceKind.toLowerCase(), version);
        return new SchemaActionResponse("activated", version);
    }

    @PutMapping("/service-kinds/{serviceKind}/versions/{version}/deprecate")
    public SchemaActionResponse deprecateServiceKindVersion(@PathVariable String serviceKind,
                                                             @PathVariable int version) {
        service.deprecateVersion("service_kind", serviceKind.toLowerCase(), version);
        return new SchemaActionResponse("deprecated", version);
    }

    @PostMapping("/sync")
    public SchemaActionResponse triggerSync() {
        service.invalidateAllCaches();
        syncer.sync();
        return new SchemaActionResponse("sync_triggered");
    }
}
