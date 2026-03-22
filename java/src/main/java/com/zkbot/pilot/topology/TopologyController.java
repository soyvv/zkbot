package com.zkbot.pilot.topology;

import com.zkbot.pilot.topology.dto.*;
import org.springframework.http.HttpStatus;
import org.springframework.web.bind.annotation.*;
import org.springframework.web.server.ResponseStatusException;

import java.util.List;
import java.util.Map;

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
    public Map<String, Object> upsertBinding(@RequestBody BindingRequest request) {
        service.upsertBinding(request.srcType(), request.srcId(),
                request.dstType(), request.dstId(),
                request.enabled(), request.metadata());
        return Map.of("status", "ok");
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
    public List<Map<String, Object>> getServiceBindings(@PathVariable String serviceKind,
                                                         @PathVariable String logicalId) {
        return service.getServiceBindings(logicalId);
    }

    @GetMapping("/services/{serviceKind}/{logicalId}/audit")
    public List<Map<String, Object>> getServiceAudit(@PathVariable String serviceKind,
                                                      @PathVariable String logicalId,
                                                      @RequestParam(defaultValue = "50") int limit) {
        return service.getServiceAudit(logicalId, limit);
    }

    @PostMapping("/services/{serviceKind}/{logicalId}/issue-bootstrap-token")
    public Map<String, Object> issueBootstrapToken(@PathVariable String serviceKind,
                                                    @PathVariable String logicalId) {
        return service.issueBootstrapToken(serviceKind, logicalId);
    }

    @PostMapping("/services/{serviceKind}/{logicalId}/reload")
    public Map<String, String> reloadService(@PathVariable String serviceKind,
                                              @PathVariable String logicalId) {
        return service.reloadService(serviceKind, logicalId);
    }

    @GetMapping("/sessions")
    public List<Map<String, Object>> listSessions(@RequestParam(required = false) String oms_id) {
        return service.listSessions(oms_id);
    }
}
