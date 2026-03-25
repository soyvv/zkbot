package com.zkbot.pilot.bootstrap;

import io.nats.client.Connection;
import io.nats.client.KeyValue;
import io.nats.client.api.KeyValueEntry;
import io.nats.client.api.KeyValueOperation;
import io.nats.client.api.KeyValueWatcher;
import io.nats.client.impl.NatsKeyValueWatchSubscription;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.context.SmartLifecycle;
import org.springframework.stereotype.Component;
import com.zkbot.pilot.discovery.DiscoveryCache;

import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

@Component
public class KvReconciler implements SmartLifecycle {

    private static final Logger log = LoggerFactory.getLogger(KvReconciler.class);
    private static final String REGISTRY_BUCKET = "zk-svc-registry-v1";
    private static final long SWEEP_INTERVAL_MS = 15_000;

    private final Connection natsConnection;
    private final BootstrapRepository repository;
    private final DiscoveryCache discoveryCache;
    private final Set<String> live = ConcurrentHashMap.newKeySet();
    private final CountDownLatch readyLatch = new CountDownLatch(1);

    private volatile boolean running;
    private Thread watchThread;

    public KvReconciler(Connection natsConnection, BootstrapRepository repository, DiscoveryCache discoveryCache) {
        this.natsConnection = natsConnection;
        this.repository = repository;
        this.discoveryCache = discoveryCache;
    }

    public boolean isKvLive(String kvKey) {
        return live.contains(kvKey);
    }

    public void waitReady() throws InterruptedException {
        readyLatch.await();
    }

    public boolean waitReady(long timeout, TimeUnit unit) throws InterruptedException {
        return readyLatch.await(timeout, unit);
    }

    @Override
    public int getPhase() {
        return 10;
    }

    @Override
    public void start() {
        running = true;
        watchThread = new Thread(this::watchLoop, "kv-reconciler");
        watchThread.setDaemon(true);
        watchThread.start();
    }

    @Override
    public void stop() {
        running = false;
        if (watchThread != null) {
            watchThread.interrupt();
        }
    }

    @Override
    public boolean isRunning() {
        return running;
    }

    private void watchLoop() {
        while (running) {
            NatsKeyValueWatchSubscription subscription = null;
            try {
                KeyValue kv = natsConnection.keyValue(REGISTRY_BUCKET);

                // Build snapshot in a thread-safe staging set, then swap on endOfData.
                Set<String> nextLive = ConcurrentHashMap.newKeySet();
                var snapshotDone = new boolean[]{false};

                KeyValueWatcher watcher = new KeyValueWatcher() {
                    @Override
                    public void watch(KeyValueEntry entry) {
                        String key = entry.getKey();
                        KeyValueOperation op = entry.getOperation();

                        if (op == KeyValueOperation.PUT) {
                            if (snapshotDone[0]) {
                                live.add(key);
                            } else {
                                nextLive.add(key);
                            }
                        } else {
                            // DELETE or PURGE
                            if (snapshotDone[0]) {
                                live.remove(key);
                                onKvLost(key);
                            } else {
                                nextLive.remove(key);
                            }
                        }
                    }

                    @Override
                    public void endOfData() {
                        Set<String> oldLive = Set.copyOf(live);
                        live.clear();
                        live.addAll(nextLive);
                        snapshotDone[0] = true;

                        if (readyLatch.getCount() > 0) {
                            readyLatch.countDown();
                            log.info("reconciler: initial snapshot loaded ({} live keys)",
                                    live.size());
                        }

                        // Fence keys that disappeared while disconnected
                        for (String lostKey : oldLive) {
                            if (!nextLive.contains(lostKey)) {
                                onKvLost(lostKey);
                            }
                        }
                    }
                };

                subscription = kv.watchAll(watcher);

                // Periodic sweep: detect keys that expired via max_age TTL
                // (TTL expiry does not emit watch events in NATS JetStream)
                long lastSweep = System.currentTimeMillis();
                while (running && natsConnection.getStatus() == Connection.Status.CONNECTED) {
                    //noinspection BusyWait
                    Thread.sleep(1000);

                    long now = System.currentTimeMillis();
                    if (now - lastSweep >= SWEEP_INTERVAL_MS && snapshotDone[0]) {
                        lastSweep = now;
                        sweepExpiredKeys(kv);
                    }
                }

            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                break;
            } catch (Exception e) {
                log.warn("reconciler: watch error: {}, retrying in 5s", e.getMessage());
                try {
                    //noinspection BusyWait
                    Thread.sleep(5000);
                } catch (InterruptedException ie) {
                    Thread.currentThread().interrupt();
                    break;
                }
            } finally {
                if (subscription != null) {
                    try {
                        subscription.unsubscribe();
                    } catch (Exception ignored) {
                    }
                }
            }
        }
    }

    private void sweepExpiredKeys(KeyValue kv) {
        List<String> snapshot = List.copyOf(live);
        for (String key : snapshot) {
            try {
                KeyValueEntry entry = kv.get(key);
                if (entry == null) {
                    live.remove(key);
                    log.info("reconciler: sweep detected expired key '{}'", key);
                    onKvLost(key);
                }
            } catch (Exception e) {
                log.warn("reconciler: sweep error checking '{}': {}", key, e.getMessage());
            }
        }
    }

    private void onKvLost(String kvKey) {
        try {
            discoveryCache.remove(kvKey);
            Map<String, Object> row = repository.findActiveSessionByKvKey(kvKey);
            if (row == null) {
                return;
            }
            String ownerSessionId = (String) row.get("owner_session_id");
            String logicalId = (String) row.get("logical_id");
            String instanceType = (String) row.get("instance_type");

            repository.fenceSession(ownerSessionId);

            if ("ENGINE".equalsIgnoreCase(instanceType)) {
                String env = repository.getEnvForLogical(logicalId);
                if (env != null) {
                    repository.releaseInstanceId(env, logicalId);
                }
            }

            log.info("reconciler: fenced session for kv_key='{}' owner='{}'",
                    kvKey, ownerSessionId);
        } catch (Exception e) {
            log.warn("reconciler: error fencing '{}': {}", kvKey, e.getMessage());
        }
    }
}
