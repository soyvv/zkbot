package com.zkbot.pilot.discovery;

import org.springframework.stereotype.Component;
import zk.discovery.v1.Discovery.ServiceRegistration;

import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

@Component
public class DiscoveryCache {

    private final ConcurrentHashMap<String, ServiceRegistration> entries = new ConcurrentHashMap<>();

    public void put(String key, ServiceRegistration registration) {
        entries.put(key, registration);
    }

    public void remove(String key) {
        entries.remove(key);
    }

    public ServiceRegistration get(String key) {
        return entries.get(key);
    }

    public Map<String, ServiceRegistration> getAll() {
        return Map.copyOf(entries);
    }

    public List<ServiceRegistration> getByType(String serviceType) {
        return entries.values().stream()
                .filter(r -> serviceType.equals(r.getServiceType()))
                .toList();
    }
}
