package com.zkbot.pilot;

import com.zkbot.pilot.config.PilotProperties;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.boot.context.properties.EnableConfigurationProperties;

@SpringBootApplication
@EnableConfigurationProperties(PilotProperties.class)
public class PilotApplication {

    public static void main(String[] args) {
        SpringApplication.run(PilotApplication.class, args);
    }
}
