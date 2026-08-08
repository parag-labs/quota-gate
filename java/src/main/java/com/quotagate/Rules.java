package com.quotagate;

import java.util.ArrayList;
import java.util.List;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

/** Rule definitions: what to limit, over which window, at which scope. */
public final class Rules {

    private Rules() {
    }

    /** Where a limit is enforced. A request is keyed differently per scope. */
    public enum Scope {
        GLOBAL,
        TENANT,
        USER
    }

    /** One provider-style limit. Any subset of the max* dimensions may be set. */
    public static final class LimitRule {
        public final String model;
        public final double windowSeconds;
        public final Double maxTokens;
        public final Double maxRequests;
        public final Double maxCost;
        public final Integer maxConcurrent;
        public final Scope scope;
        public final int bucketsPerWindow;
        public final boolean precise;
        public final String name;

        private LimitRule(Builder b) {
            this.model = b.model;
            this.windowSeconds = b.windowSeconds;
            this.maxTokens = b.maxTokens;
            this.maxRequests = b.maxRequests;
            this.maxCost = b.maxCost;
            this.maxConcurrent = b.maxConcurrent;
            this.scope = b.scope;
            this.bucketsPerWindow = b.bucketsPerWindow;
            this.precise = b.precise;
            this.name = b.name;
        }

        /** 0 selects the exact per-event log; otherwise window/bucketsPerWindow. */
        public double bucketSeconds() {
            return precise ? 0.0 : windowSeconds / Math.max(1, bucketsPerWindow);
        }

        public String label() {
            return name != null ? name
                    : model + ":" + scope.name().toLowerCase() + ":" + (long) windowSeconds + "s";
        }

        public boolean hasUsageLimit() {
            return maxTokens != null || maxRequests != null || maxCost != null;
        }

        public static Builder builder(String model, double windowSeconds) {
            return new Builder(model, windowSeconds);
        }

        /** Fluent builder so a rule can set just the dimensions it cares about. */
        public static final class Builder {
            private final String model;
            private final double windowSeconds;
            private Double maxTokens;
            private Double maxRequests;
            private Double maxCost;
            private Integer maxConcurrent;
            private Scope scope = Scope.GLOBAL;
            private int bucketsPerWindow = 60;
            private boolean precise;
            private String name;

            private Builder(String model, double windowSeconds) {
                this.model = model;
                this.windowSeconds = windowSeconds;
            }

            public Builder maxTokens(double v) {
                this.maxTokens = v;
                return this;
            }

            public Builder maxRequests(double v) {
                this.maxRequests = v;
                return this;
            }

            public Builder maxCost(double v) {
                this.maxCost = v;
                return this;
            }

            public Builder maxConcurrent(int v) {
                this.maxConcurrent = v;
                return this;
            }

            public Builder scope(Scope v) {
                this.scope = v;
                return this;
            }

            public Builder bucketsPerWindow(int v) {
                this.bucketsPerWindow = v;
                return this;
            }

            public Builder precise(boolean v) {
                this.precise = v;
                return this;
            }

            public Builder name(String v) {
                this.name = v;
                return this;
            }

            public LimitRule build() {
                return new LimitRule(this);
            }
        }
    }

    private static final ObjectMapper MAPPER = new ObjectMapper();

    public static List<LimitRule> fromJson(String text) {
        try {
            JsonNode root = MAPPER.readTree(text);
            List<LimitRule> rules = new ArrayList<>();
            for (JsonNode row : root.get("rules")) {
                LimitRule.Builder b = LimitRule.builder(
                        row.get("model").asText(), row.get("window_seconds").asDouble());
                if (row.has("max_tokens")) {
                    b.maxTokens(row.get("max_tokens").asDouble());
                }
                if (row.has("max_requests")) {
                    b.maxRequests(row.get("max_requests").asDouble());
                }
                if (row.has("max_cost")) {
                    b.maxCost(row.get("max_cost").asDouble());
                }
                if (row.has("max_concurrent")) {
                    b.maxConcurrent(row.get("max_concurrent").asInt());
                }
                if (row.has("scope")) {
                    b.scope(Scope.valueOf(row.get("scope").asText().toUpperCase()));
                }
                if (row.has("buckets_per_window")) {
                    b.bucketsPerWindow(row.get("buckets_per_window").asInt());
                }
                if (row.has("precise")) {
                    b.precise(row.get("precise").asBoolean());
                }
                if (row.has("name")) {
                    b.name(row.get("name").asText());
                }
                rules.add(b.build());
            }
            return rules;
        } catch (Exception e) {
            throw new IllegalArgumentException("invalid rules json", e);
        }
    }
}
