package com.zkbot.pilot;

import com.zkbot.pilot.config.PilotProperties;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
import java.util.Map;

@RestController
public class HealthController {

    private final PilotProperties props;

    public HealthController(PilotProperties props) {
        this.props = props;
    }

    @GetMapping("/health")
    public Map<String, String> health() {
        return Map.of(
            "status", "ok",
            "pilot_id", props.pilotId(),
            "env", props.env()
        );
    }
}
