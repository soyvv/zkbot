package com.zkbot.pilot.topology;

import com.zkbot.pilot.ops.dto.RegistrationAuditEntry;
import com.zkbot.pilot.topology.dto.*;
import org.springframework.http.HttpStatus;
import org.springframework.web.bind.annotation.*;
import org.springframework.web.server.ResponseStatusException;

import java.util.List;

@RestController
@RequestMapping("/v1/topology")
public class TopologyController {

    private final TopologyService service;

    public TopologyController(TopologyService service) {
        this.service = service;
    }

    @GetMapping
    public TopologyView getTopology(@RequestParam(required = false) String oms_id) {
        return service.getTopology(oms_id);
    }

    @PutMapping("/bindings")
    public TopologyActionResponse upsertBinding(@RequestBody BindingRequest request) {
        service.upsertBinding(request.srcType(), request.srcId(),
                request.dstType(), request.dstId(),
                request.enabled(), request.metadata());
        return new TopologyActionResponse("ok");
    }

    @GetMapping("/views")
    public List<String> listViews() {
        return service.listViews();
    }

    @GetMapping("/views/{viewName}")
    public TopologyView getView(@PathVariable String viewName,
                                @RequestParam(required = false) String oms_id) {
        return service.getView(viewName, oms_id);
    }

    @GetMapping("/services")
    public List<ServiceNode> listServices(@RequestParam(required = false) String oms_id,
                                          @RequestParam(required = false) String service_kind) {
        return service.listServices(oms_id, service_kind);
    }

    @PostMapping("/services/{serviceKind}")
    public CreateServiceResponse createService(@PathVariable String serviceKind,
                                                @RequestBody CreateServiceRequest request) {
        return service.createService(serviceKind, request);
    }

    @GetMapping("/services/{serviceKind}")
    public List<ServiceNode> listServicesByKind(@PathVariable String serviceKind,
                                                @RequestParam(required = false) String oms_id) {
        return service.listServices(oms_id, serviceKind);
    }

    @GetMapping("/services/{serviceKind}/{logicalId}")
    public ServiceDetail getServiceDetail(@PathVariable String serviceKind,
                                          @PathVariable String logicalId) {
        ServiceDetail detail = service.getServiceDetail(serviceKind, logicalId);
        if (detail == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Service not found: " + serviceKind + "/" + logicalId);
        }
        return detail;
    }

    @GetMapping("/services/{serviceKind}/{logicalId}/bindings")
    public List<BindingEntry> getServiceBindings(@PathVariable String serviceKind,
                                                  @PathVariable String logicalId) {
        return service.getServiceBindings(logicalId);
    }

    @GetMapping("/services/{serviceKind}/{logicalId}/audit")
    public List<RegistrationAuditEntry> getServiceAudit(@PathVariable String serviceKind,
                                                        @PathVariable String logicalId,
                                                        @RequestParam(defaultValue = "50") int limit) {
        return service.getServiceAudit(logicalId, limit);
    }

    @PostMapping("/services/{serviceKind}/{logicalId}/issue-bootstrap-token")
    public BootstrapTokenResponse issueBootstrapToken(@PathVariable String serviceKind,
                                                       @PathVariable String logicalId) {
        return service.issueBootstrapToken(serviceKind, logicalId);
    }

    @PostMapping("/services/{serviceKind}/{logicalId}/reload")
    public TopologyActionResponse reloadService(@PathVariable String serviceKind,
                                              @PathVariable String logicalId) {
        return service.reloadService(serviceKind, logicalId);
    }

    @GetMapping("/sessions")
    public List<SessionResponse> listSessions(@RequestParam(required = false) String oms_id) {
        return service.listSessions(oms_id);
    }

    // --- Refdata venue instances ---

    @GetMapping("/refdata-venues")
    public List<RefdataVenueInstanceEntry> listRefdataVenues() {
        return service.listRefdataVenueInstances();
    }

    @GetMapping("/refdata-venues/{logicalId}")
    public RefdataVenueInstanceEntry getRefdataVenue(@PathVariable String logicalId) {
        return service.getRefdataVenueInstance(logicalId);
    }

    @PostMapping("/refdata-venues")
    public RefdataVenueInstanceEntry createRefdataVenue(@RequestBody CreateRefdataVenueRequest request) {
        return service.createRefdataVenueInstance(request);
    }
}
