package com.zkbot.pilot.discovery;

import org.springframework.stereotype.Component;
import zk.discovery.v1.Discovery.ServiceRegistration;

import java.util.List;
import java.util.Map;

@Component
public class DiscoveryResolver {

    private final DiscoveryCache cache;

    public DiscoveryResolver(DiscoveryCache cache) {
        this.cache = cache;
    }

    public String findOms(String omsId) {
        return extractAddress(cache.get("svc.oms." + omsId));
    }

    public String findGateway(String gwId) {
        return extractAddress(cache.get("svc.gw." + gwId));
    }

    public String findEngine(String engineId) {
        return extractAddress(cache.get("svc.engine." + engineId));
    }

    public String findMdgw(String logicalId) {
        return extractAddress(cache.get("svc.mdgw." + logicalId));
    }

    public String findRefdata(String logicalId) {
        return extractAddress(cache.get("svc.refdata." + logicalId));
    }

    public List<ServiceRegistration> findByType(String type) {
        return cache.getByType(type);
    }

    public Map<String, ServiceRegistration> getAll() {
        return cache.getAll();
    }

    private String extractAddress(ServiceRegistration reg) {
        if (reg == null || !reg.hasEndpoint()) {
            return null;
        }
        String address = reg.getEndpoint().getAddress();
        return address.isEmpty() ? null : address;
    }
}
