package com.zkbot.pilot.account;

import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.grpc.OmsGrpcClient;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import zk.oms.v1.Oms;

import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;
import static org.mockito.Mockito.when;

@ExtendWith(MockitoExtension.class)
class AccountServiceTest {

    @Mock AccountRepository repository;
    @Mock OmsGrpcClient omsClient;
    @Mock DiscoveryCache discoveryCache;

    AccountService service;

    @BeforeEach
    void setUp() {
        service = new AccountService(repository, omsClient, discoveryCache);
    }

    @Test
    void get_balances_returns_empty_when_oms_missing_from_discovery() {
        when(repository.getAccountBinding(8002L)).thenReturn(Map.of("oms_id", "oms_dev_1"));
        when(omsClient.queryBalances(org.mockito.ArgumentMatchers.eq("oms_dev_1"), org.mockito.ArgumentMatchers.any(Oms.QueryBalancesRequest.class)))
                .thenThrow(new IllegalStateException("OMS not found in discovery: oms_dev_1"));

        assertThat(service.getBalances(8002L)).isEmpty();
    }

    @Test
    void get_positions_returns_empty_when_oms_missing_from_discovery() {
        when(repository.getAccountBinding(8002L)).thenReturn(Map.of("oms_id", "oms_dev_1"));
        when(omsClient.queryPosition(org.mockito.ArgumentMatchers.eq("oms_dev_1"), org.mockito.ArgumentMatchers.any(Oms.QueryPositionRequest.class)))
                .thenThrow(new IllegalStateException("OMS not found in discovery: oms_dev_1"));

        assertThat(service.getPositions(8002L)).isEmpty();
    }

    @Test
    void get_open_orders_returns_empty_when_oms_missing_from_discovery() {
        when(repository.getAccountBinding(8002L)).thenReturn(Map.of("oms_id", "oms_dev_1"));
        when(omsClient.queryOpenOrders(org.mockito.ArgumentMatchers.eq("oms_dev_1"), org.mockito.ArgumentMatchers.any(Oms.QueryOpenOrderRequest.class)))
                .thenThrow(new IllegalStateException("OMS not found in discovery: oms_dev_1"));

        assertThat(service.getOpenOrders(8002L)).isEmpty();
    }
}
