package com.zkbot.pilot.topology.dto;

import java.time.Instant;
import java.util.List;

public record TopologyView(Scope scope, List<ServiceNode> nodes, List<Edge> edges, List<Object> groups) {

    public record Scope(String omsId, Instant asOf) {}
}
