package com.zkbot.pilot.meta;

import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.discovery.DiscoveryCache;
import org.springframework.stereotype.Service;
import zk.discovery.v1.Discovery.ServiceRegistration;

import java.time.Instant;
import java.util.*;

@Service
public class MetaService {

    private final MetaRepository repository;
    private final DiscoveryCache discoveryCache;
    private final PilotProperties props;
    private final VenueManifestLoader venueManifestLoader;

    public MetaService(MetaRepository repository, DiscoveryCache discoveryCache,
                       PilotProperties props, VenueManifestLoader venueManifestLoader) {
        this.repository = repository;
        this.discoveryCache = discoveryCache;
        this.props = props;
        this.venueManifestLoader = venueManifestLoader;
    }

    public Map<String, Object> getMeta(Set<String> domains) {
        var result = new LinkedHashMap<String, Object>();
        result.put("as_of", Instant.now().toString());
        result.put("env", props.env());

        boolean all = domains == null || domains.isEmpty();

        // enums — always included (cheap, static)
        result.put("enums", buildEnums());

        // refs — filtered by domain
        var refs = new LinkedHashMap<String, Object>();

        if (all || domains.contains("topology") || domains.contains("manual") || domains.contains("accounts")) {
            refs.put("oms", buildOmsRefs());
            refs.put("venues", buildVenueRefs());
            refs.put("gateways", buildGatewayRefs());
        }
        if (all || domains.contains("topology")) {
            refs.put("mdgws", buildMdgwRefs());
        }
        if (all || domains.contains("accounts") || domains.contains("manual")) {
            refs.put("accounts", buildAccountRefs());
        }
        if (all || domains.contains("bot")) {
            refs.put("bots", buildBotRefs());
        }

        result.put("refs", refs);
        return result;
    }

    // --- enums: Pilot-owned constants ---

    private Map<String, Object> buildEnums() {
        var enums = new LinkedHashMap<String, Object>();

        enums.put("order_sides", List.of(
                optionOf("BUY", "Buy"),
                optionOf("SELL", "Sell")));

        enums.put("order_types", List.of(
                optionOf("LIMIT", "Limit"),
                optionOf("MARKET", "Market")));

        enums.put("time_in_force", List.of(
                optionOf("GTC", "Good Till Cancel"),
                optionOf("IOC", "Immediate or Cancel"),
                optionOf("FOK", "Fill or Kill"),
                optionOf("GTD", "Good Till Date")));

        enums.put("open_close_type", List.of(
                optionOf("OPEN", "Open"),
                optionOf("CLOSE", "Close")));

        enums.put("instrument_type", List.of(
                optionOf("SPOT", "Spot"),
                optionOf("PERP", "Perpetual"),
                optionOf("FUTURE", "Future"),
                optionOf("OPTION", "Option")));

        enums.put("account_statuses", List.of(
                optionOf("active", "Active"),
                optionOf("disabled", "Disabled"),
                optionOf("suspended", "Suspended")));

        enums.put("bot_statuses", List.of(
                optionOf("INITIALIZING", "Initializing"),
                optionOf("RUNNING", "Running"),
                optionOf("PAUSED", "Paused"),
                optionOf("STOPPED", "Stopped"),
                optionOf("ERROR", "Error")));

        enums.put("service_families", List.of(
                optionOf("OMS", "OMS"),
                optionOf("GW", "Gateway"),
                optionOf("MDGW", "Market Data Gateway"),
                optionOf("ENGINE", "Strategy Engine"),
                optionOf("REFDATA", "Reference Data")));

        enums.put("risk_states", List.of(
                optionOf("ok", "OK"),
                optionOf("warning", "Warning"),
                optionOf("breach", "Breach"),
                optionOf("panic", "Panic")));

        enums.put("alert_severities", List.of(
                optionOf("LOW", "Low"),
                optionOf("MEDIUM", "Medium"),
                optionOf("HIGH", "High"),
                optionOf("CRITICAL", "Critical")));

        return enums;
    }

    // --- refs: aggregated from DB + live KV state ---

    private List<Map<String, Object>> buildOmsRefs() {
        var liveState = discoveryCache.getAll();
        var omsRows = repository.listOmsInstances();
        return omsRows.stream().map(row -> {
            String omsId = (String) row.get("oms_id");
            boolean live = isLive(omsId, "oms", liveState);
            boolean enabled = Boolean.TRUE.equals(row.get("enabled"));
            var ref = new LinkedHashMap<String, Object>();
            ref.put("value", omsId);
            ref.put("label", omsId);
            ref.put("live", live);
            ref.put("enabled", enabled);
            return (Map<String, Object>) ref;
        }).toList();
    }

    private List<Map<String, Object>> buildVenueRefs() {
        // Merge DB venues with manifest venues for a complete list
        var dbVenues = new LinkedHashSet<>(repository.listDistinctVenues());
        var manifestVenues = venueManifestLoader.getAll();

        var result = new ArrayList<Map<String, Object>>();
        var seen = new HashSet<String>();

        // Manifests first (richer data)
        for (var m : manifestVenues) {
            var ref = new LinkedHashMap<String, Object>();
            ref.put("value", m.venue());
            ref.put("label", m.venue().toUpperCase());
            ref.put("capabilities", new ArrayList<>(m.capabilities().keySet()));
            ref.put("supports_tradfi_sessions", m.supportsTradfiSessions());
            result.add(ref);
            seen.add(m.venue());
        }

        // DB-only venues (no manifest available)
        for (String v : dbVenues) {
            if (!seen.contains(v.toLowerCase())) {
                result.add(optionOf(v, v));
            }
        }

        return result;
    }

    private List<Map<String, Object>> buildGatewayRefs() {
        var liveState = discoveryCache.getAll();
        var gwRows = repository.listGatewayInstances();
        return gwRows.stream().map(row -> {
            String gwId = (String) row.get("gw_id");
            boolean live = isLive(gwId, "gw", liveState);
            boolean enabled = Boolean.TRUE.equals(row.get("enabled"));
            var ref = new LinkedHashMap<String, Object>();
            ref.put("value", gwId);
            ref.put("label", gwId);
            ref.put("venue", row.get("venue"));
            ref.put("live", live);
            ref.put("enabled", enabled);
            return (Map<String, Object>) ref;
        }).toList();
    }

    private List<Map<String, Object>> buildMdgwRefs() {
        var liveState = discoveryCache.getAll();
        var instances = repository.listLogicalInstances(props.env());
        return instances.stream()
                .filter(i -> "MDGW".equalsIgnoreCase((String) i.get("instance_type")))
                .map(i -> {
                    String logicalId = (String) i.get("logical_id");
                    boolean live = isLive(logicalId, "mdgw", liveState);
                    boolean enabled = Boolean.TRUE.equals(i.get("enabled"));
                    var ref = new LinkedHashMap<String, Object>();
                    ref.put("value", logicalId);
                    ref.put("label", logicalId);
                    ref.put("live", live);
                    ref.put("enabled", enabled);
                    return (Map<String, Object>) ref;
                }).toList();
    }

    private List<Map<String, Object>> buildAccountRefs() {
        var accountRows = repository.listAccounts();
        return accountRows.stream().map(row -> {
            var ref = new LinkedHashMap<String, Object>();
            ref.put("value", String.valueOf(row.get("account_id")));
            String label = row.get("account_id") + " / " +
                    Objects.toString(row.get("exch_account_id"), "");
            ref.put("label", label.endsWith(" / ") ? String.valueOf(row.get("account_id")) : label);
            ref.put("venue", row.get("venue"));
            ref.put("oms_id", row.get("oms_id"));
            ref.put("gw_id", row.get("gw_id"));
            ref.put("status", row.get("status"));
            return (Map<String, Object>) ref;
        }).toList();
    }

    private List<Map<String, Object>> buildBotRefs() {
        var strategyRows = repository.listStrategies();
        return strategyRows.stream().map(row -> {
            var ref = new LinkedHashMap<String, Object>();
            ref.put("value", row.get("strategy_id"));
            ref.put("label", row.get("strategy_id"));
            ref.put("runtime_type", row.get("runtime_type"));
            ref.put("enabled", row.get("enabled"));
            ref.put("execution_status", row.get("execution_status"));
            ref.put("oms_id", row.get("target_oms_id"));
            return (Map<String, Object>) ref;
        }).toList();
    }

    // --- helpers ---

    private boolean isLive(String logicalId, String serviceType,
                            Map<String, ServiceRegistration> liveState) {
        String kvKey = "svc." + serviceType + "." + logicalId;
        if (liveState.containsKey(kvKey)) return true;
        // Fallback: check by service_id field in registration
        for (var entry : liveState.values()) {
            if (logicalId.equals(entry.getServiceId())) return true;
        }
        return false;
    }

    private static Map<String, Object> optionOf(String value, String label) {
        var m = new LinkedHashMap<String, Object>();
        m.put("value", value);
        m.put("label", label);
        return m;
    }
}
