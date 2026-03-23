package com.zkbot.pilot.grpc;

import com.zkbot.pilot.discovery.DiscoveryResolver;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.stereotype.Component;
import zk.oms.v1.OMSServiceGrpc;
import zk.oms.v1.OMSServiceGrpc.OMSServiceBlockingStub;
import zk.oms.v1.Oms;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

@Component
public class OmsGrpcClient implements DisposableBean {

    private static final Logger log = LoggerFactory.getLogger(OmsGrpcClient.class);

    private final DiscoveryResolver discoveryResolver;
    private final ConcurrentHashMap<String, ManagedChannel> channelCache = new ConcurrentHashMap<>();
    private final ConcurrentHashMap<String, String> addressCache = new ConcurrentHashMap<>();

    public OmsGrpcClient(DiscoveryResolver discoveryResolver) {
        this.discoveryResolver = discoveryResolver;
    }

    public OMSServiceBlockingStub getStub(String omsId) {
        String currentAddress = discoveryResolver.findOms(omsId);
        if (currentAddress == null) {
            throw new IllegalStateException("OMS not found in discovery: " + omsId);
        }

        String cachedAddress = addressCache.get(omsId);
        if (cachedAddress != null && !cachedAddress.equals(currentAddress)) {
            log.info("OMS {} address changed from {} to {}, rebuilding channel", omsId, cachedAddress, currentAddress);
            ManagedChannel old = channelCache.remove(omsId);
            addressCache.remove(omsId);
            if (old != null) {
                old.shutdown();
            }
        }

        ManagedChannel channel = channelCache.computeIfAbsent(omsId, id -> {
            log.info("Creating gRPC channel to OMS {} at {}", id, currentAddress);
            addressCache.put(id, currentAddress);
            return ManagedChannelBuilder.forTarget(currentAddress)
                    .usePlaintext()
                    .build();
        });
        return OMSServiceGrpc.newBlockingStub(channel);
    }

    public Oms.OMSResponse placeOrder(String omsId, Oms.PlaceOrderRequest request) {
        return getStub(omsId).placeOrder(request);
    }

    public Oms.OMSResponse batchPlaceOrders(String omsId, Oms.BatchPlaceOrdersRequest request) {
        return getStub(omsId).batchPlaceOrders(request);
    }

    public Oms.OMSResponse cancelOrder(String omsId, Oms.CancelOrderRequest request) {
        return getStub(omsId).cancelOrder(request);
    }

    public Oms.OMSResponse batchCancelOrders(String omsId, Oms.BatchCancelOrdersRequest request) {
        return getStub(omsId).batchCancelOrders(request);
    }

    public Oms.OMSResponse panic(String omsId, Oms.PanicRequest request) {
        return getStub(omsId).panic(request);
    }

    public Oms.OMSResponse dontPanic(String omsId, Oms.DontPanicRequest request) {
        return getStub(omsId).dontPanic(request);
    }

    public Oms.OrderDetailResponse queryOpenOrders(String omsId, Oms.QueryOpenOrderRequest request) {
        return getStub(omsId).queryOpenOrders(request);
    }

    public Oms.OrderDetailResponse queryOrderDetails(String omsId, Oms.QueryOrderDetailRequest request) {
        return getStub(omsId).queryOrderDetails(request);
    }

    public Oms.TradeDetailResponse queryTradeDetails(String omsId, Oms.QueryTradeDetailRequest request) {
        return getStub(omsId).queryTradeDetails(request);
    }

    public Oms.PositionResponse queryPosition(String omsId, Oms.QueryPositionRequest request) {
        return getStub(omsId).queryPosition(request);
    }

    public Oms.QueryBalancesResponse queryBalances(String omsId, Oms.QueryBalancesRequest request) {
        return getStub(omsId).queryBalances(request);
    }

    @Override
    public void destroy() {
        log.info("Shutting down {} OMS gRPC channels", channelCache.size());
        channelCache.forEach((id, channel) -> {
            try {
                channel.shutdown().awaitTermination(5, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                log.warn("Interrupted while shutting down channel for OMS {}", id);
                channel.shutdownNow();
                Thread.currentThread().interrupt();
            }
        });
        channelCache.clear();
    }
}
