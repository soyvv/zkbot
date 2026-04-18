package com.zkbot.pilot.bootstrap;

import com.zkbot.pilot.discovery.DiscoveryCache;
import io.nats.client.Connection;
import org.junit.jupiter.api.Test;

import java.lang.reflect.Method;
import java.util.Map;

import static org.mockito.Mockito.*;

class KvReconcilerTest {

    @Test
    void onKvLost_removesDiscoveryCacheEntry_beforeFencingSession() throws Exception {
        Connection natsConnection = mock(Connection.class);
        BootstrapRepository repository = mock(BootstrapRepository.class);
        DiscoveryCache discoveryCache = mock(DiscoveryCache.class);
        KvReconciler reconciler = new KvReconciler(natsConnection, repository, discoveryCache);

        when(repository.findActiveSessionByKvKey("svc.gw.gw_okx_demo1")).thenReturn(
                Map.of(
                        "owner_session_id", "sess-1",
                        "logical_id", "gw_okx_demo1",
                        "instance_type", "GW"
                )
        );

        Method method = KvReconciler.class.getDeclaredMethod("onKvLost", String.class);
        method.setAccessible(true);
        method.invoke(reconciler, "svc.gw.gw_okx_demo1");

        verify(discoveryCache).remove("svc.gw.gw_okx_demo1");
        verify(repository).fenceSession("sess-1");
    }

    @Test
    void onKvLost_stillRemovesDiscoveryCacheEntry_whenNoActiveSessionRowExists() throws Exception {
        Connection natsConnection = mock(Connection.class);
        BootstrapRepository repository = mock(BootstrapRepository.class);
        DiscoveryCache discoveryCache = mock(DiscoveryCache.class);
        KvReconciler reconciler = new KvReconciler(natsConnection, repository, discoveryCache);

        when(repository.findActiveSessionByKvKey("svc.gw.gw_okx_demo1")).thenReturn(null);

        Method method = KvReconciler.class.getDeclaredMethod("onKvLost", String.class);
        method.setAccessible(true);
        method.invoke(reconciler, "svc.gw.gw_okx_demo1");

        verify(discoveryCache).remove("svc.gw.gw_okx_demo1");
        verify(repository, never()).fenceSession(any());
    }
}
