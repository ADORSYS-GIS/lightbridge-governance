-- Drop telemetry models (#30)
-- Order matters: child tables first, then parent.

DROP TABLE IF EXISTS model_pricing;
DROP TABLE IF EXISTS tool_calls;
DROP TABLE IF EXISTS model_calls;
DROP TABLE IF EXISTS executions;