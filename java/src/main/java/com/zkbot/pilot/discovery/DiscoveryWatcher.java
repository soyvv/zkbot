package com.zkbot.pilot.discovery;

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
import zk.discovery.v1.Discovery.ServiceRegistration;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

@Component
public class DiscoveryWatcher implements SmartLifecycle {

    private static final Logger log = LoggerFactory.getLogger(DiscoveryWatcher.class);
    private static final String REGISTRY_BUCKET = "zk-svc-registry-v1";

    private final Connection natsConnection;
    private final DiscoveryCache cache;
    private final CountDownLatch readyLatch = new CountDownLatch(1);

    private volatile boolean running;
    private Thread watchThread;

    public DiscoveryWatcher(Connection natsConnection, DiscoveryCache cache) {
        this.natsConnection = natsConnection;
        this.cache = cache;
    }

    public void waitReady() throws InterruptedException {
        readyLatch.await();
    }

    public boolean waitReady(long timeout, TimeUnit unit) throws InterruptedException {
        return readyLatch.await(timeout, unit);
    }

    @Override
    public int getPhase() {
        return 15;
    }

    @Override
    public void start() {
        running = true;
        watchThread = new Thread(this::watchLoop, "discovery-watcher");
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
                var snapshotDone = new boolean[]{false};

                KeyValueWatcher watcher = new KeyValueWatcher() {
                    @Override
                    public void watch(KeyValueEntry entry) {
                        String key = entry.getKey();
                        KeyValueOperation op = entry.getOperation();

                        if (op == KeyValueOperation.PUT) {
                            try {
                                ServiceRegistration reg = ServiceRegistration.parseFrom(entry.getValue());
                                cache.put(key, reg);
                                if (snapshotDone[0]) {
                                    log.debug("discovery: PUT key='{}' type='{}'", key, reg.getServiceType());
                                }
                            } catch (Exception e) {
                                log.warn("discovery: failed to parse registration for key='{}': {}",
                                        key, e.getMessage());
                            }
                        } else {
                            // DELETE or PURGE
                            cache.remove(key);
                            if (snapshotDone[0]) {
                                log.debug("discovery: {} key='{}'", op, key);
                            }
                        }
                    }

                    @Override
                    public void endOfData() {
                        snapshotDone[0] = true;
                        if (readyLatch.getCount() > 0) {
                            readyLatch.countDown();
                            log.info("discovery: initial snapshot loaded ({} entries)",
                                    cache.getAll().size());
                        }
                    }
                };

                subscription = kv.watchAll(watcher);

                // Block until thread is interrupted or connection drops
                while (running && natsConnection.getStatus() == Connection.Status.CONNECTED) {
                    //noinspection BusyWait
                    Thread.sleep(1000);
                }

            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                break;
            } catch (Exception e) {
                log.warn("discovery: watch error: {}, retrying in 5s", e.getMessage());
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
}
