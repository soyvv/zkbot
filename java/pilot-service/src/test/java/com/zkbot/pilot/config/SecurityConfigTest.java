package com.zkbot.pilot.config;

import com.zkbot.pilot.HealthController;
import com.zkbot.pilot.account.AccountController;
import com.zkbot.pilot.account.AccountService;
import com.zkbot.pilot.bot.BotController;
import com.zkbot.pilot.bot.BotService;
import com.zkbot.pilot.bot.LegacyCompatController;
import com.zkbot.pilot.manual.ManualController;
import com.zkbot.pilot.manual.ManualService;
import com.zkbot.pilot.ops.OpsController;
import com.zkbot.pilot.ops.OpsService;
import com.zkbot.pilot.topology.TopologyController;
import com.zkbot.pilot.topology.TopologyService;
import org.junit.jupiter.api.Nested;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.autoconfigure.web.servlet.WebMvcTest;
import org.springframework.context.annotation.Import;
import org.springframework.test.context.ActiveProfiles;
import org.springframework.test.context.bean.override.mockito.MockitoBean;
import org.springframework.test.web.servlet.MockMvc;
import org.springframework.test.web.servlet.ResultMatcher;

import static org.springframework.security.test.web.servlet.request.SecurityMockMvcRequestPostProcessors.httpBasic;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.post;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.put;
import static org.springframework.test.web.servlet.result.MockMvcResultMatchers.status;

/**
 * Security role matrix test.
 * Only asserts auth decisions (401/403), not business logic.
 * Uses isUnauthorized() / isForbidden() for denied access.
 * For permitted access, asserts NOT 401 and NOT 403 (the response may be 200 or 500
 * depending on mock return values — we only care that auth passed).
 */
@WebMvcTest({
        HealthController.class,
        TopologyController.class,
        ManualController.class,
        AccountController.class,
        BotController.class,
        OpsController.class,
        LegacyCompatController.class
})
@Import({SecurityConfig.class, DevSecurityConfig.class})
@ActiveProfiles("dev")
class SecurityConfigTest {

    @Autowired
    private MockMvc mvc;

    @MockitoBean TopologyService topologyService;
    @MockitoBean ManualService manualService;
    @MockitoBean AccountService accountService;
    @MockitoBean BotService botService;
    @MockitoBean OpsService opsService;
    @MockitoBean PilotProperties pilotProperties;

    /** Auth passed — status is neither 401 nor 403 (may be 200, 500, etc.) */
    private static ResultMatcher authPassed() {
        return result -> {
            int s = result.getResponse().getStatus();
            if (s == 401 || s == 403) {
                throw new AssertionError("Expected auth to pass but got " + s);
            }
        };
    }

    // ── Health (public) ───────────────────────────────────────────────────

    @Test
    void health_accessible_without_auth() throws Exception {
        mvc.perform(get("/health"))
                .andExpect(authPassed());
    }

    // ── Unauthenticated → 401 ─────────────────────────────────────────────

    @Test
    void topology_returns_401_without_auth() throws Exception {
        mvc.perform(get("/v1/topology"))
                .andExpect(status().isUnauthorized());
    }

    @Test
    void manual_returns_401_without_auth() throws Exception {
        mvc.perform(post("/v1/manual/orders").contentType("application/json").content("{}"))
                .andExpect(status().isUnauthorized());
    }

    @Test
    void accounts_returns_401_without_auth() throws Exception {
        mvc.perform(get("/v1/accounts"))
                .andExpect(status().isUnauthorized());
    }

    @Test
    void bot_returns_401_without_auth() throws Exception {
        mvc.perform(get("/v1/bot/strategies"))
                .andExpect(status().isUnauthorized());
    }

    @Test
    void ops_returns_401_without_auth() throws Exception {
        mvc.perform(get("/v1/ops/audit/registrations"))
                .andExpect(status().isUnauthorized());
    }

    // ── TRADER role ───────────────────────────────────────────────────────

    @Nested
    class TraderRole {

        @Test
        void can_access_manual_trading() throws Exception {
            mvc.perform(post("/v1/manual/orders")
                            .with(httpBasic("trader", "trader"))
                            .contentType("application/json").content("{}"))
                    .andExpect(authPassed());
        }

        @Test
        void can_read_accounts() throws Exception {
            mvc.perform(get("/v1/accounts")
                            .with(httpBasic("trader", "trader")))
                    .andExpect(authPassed());
        }

        @Test
        void cannot_write_accounts() throws Exception {
            mvc.perform(post("/v1/accounts")
                            .with(httpBasic("trader", "trader"))
                            .contentType("application/json").content("{}"))
                    .andExpect(status().isForbidden());
        }

        @Test
        void cannot_access_topology() throws Exception {
            mvc.perform(get("/v1/topology")
                            .with(httpBasic("trader", "trader")))
                    .andExpect(status().isForbidden());
        }

        @Test
        void cannot_access_ops() throws Exception {
            mvc.perform(get("/v1/ops/audit/registrations")
                            .with(httpBasic("trader", "trader")))
                    .andExpect(status().isForbidden());
        }

        @Test
        void can_start_bot_execution() throws Exception {
            mvc.perform(post("/v1/bot/executions/start")
                            .with(httpBasic("trader", "trader"))
                            .contentType("application/json").content("{}"))
                    .andExpect(authPassed());
        }

        @Test
        void can_stop_bot_execution() throws Exception {
            mvc.perform(post("/v1/bot/executions/test-exec/stop")
                            .with(httpBasic("trader", "trader"))
                            .contentType("application/json").content("{}"))
                    .andExpect(authPassed());
        }

        @Test
        void can_read_bot_data() throws Exception {
            mvc.perform(get("/v1/bot/strategies")
                            .with(httpBasic("trader", "trader")))
                    .andExpect(authPassed());
        }

        @Test
        void cannot_create_strategy() throws Exception {
            mvc.perform(post("/v1/bot/strategies")
                            .with(httpBasic("trader", "trader"))
                            .contentType("application/json").content("{}"))
                    .andExpect(status().isForbidden());
        }

        @Test
        void cannot_put_topology_bindings() throws Exception {
            mvc.perform(put("/v1/topology/bindings")
                            .with(httpBasic("trader", "trader"))
                            .contentType("application/json").content("{}"))
                    .andExpect(status().isForbidden());
        }
    }

    // ── OPS role ──────────────────────────────────────────────────────────

    @Nested
    class OpsRole {

        @Test
        void can_access_topology() throws Exception {
            mvc.perform(get("/v1/topology")
                            .with(httpBasic("ops", "ops")))
                    .andExpect(authPassed());
        }

        @Test
        void can_access_ops() throws Exception {
            mvc.perform(get("/v1/ops/audit/registrations")
                            .with(httpBasic("ops", "ops")))
                    .andExpect(authPassed());
        }

        @Test
        void can_write_accounts() throws Exception {
            mvc.perform(post("/v1/accounts")
                            .with(httpBasic("ops", "ops"))
                            .contentType("application/json").content("{}"))
                    .andExpect(authPassed());
        }

        @Test
        void can_create_strategy() throws Exception {
            mvc.perform(post("/v1/bot/strategies")
                            .with(httpBasic("ops", "ops"))
                            .contentType("application/json").content("{}"))
                    .andExpect(authPassed());
        }
    }

    // ── ADMIN role ────────────────────────────────────────────────────────

    @Nested
    class AdminRole {

        @Test
        void can_access_topology() throws Exception {
            mvc.perform(get("/v1/topology").with(httpBasic("admin", "admin")))
                    .andExpect(authPassed());
        }

        @Test
        void can_access_ops() throws Exception {
            mvc.perform(get("/v1/ops/audit/registrations").with(httpBasic("admin", "admin")))
                    .andExpect(authPassed());
        }

        @Test
        void can_access_accounts() throws Exception {
            mvc.perform(get("/v1/accounts").with(httpBasic("admin", "admin")))
                    .andExpect(authPassed());
        }
    }
}
