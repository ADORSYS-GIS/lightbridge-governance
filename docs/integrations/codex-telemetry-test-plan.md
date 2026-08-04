# Codex Telemetry Rollout - Test Plan

This document provides step-by-step instructions for manually testing the Codex telemetry rollout implementation.

## Prerequisites

1. Access to a Codex installation
2. A per-developer ingest token from the governance registry
3. Access to the governance dashboard (Grafana)
4. Network access to `otel.ai.camer.digital`

## Test 1: Verify Statsig is Disabled

**Objective**: Confirm that Codex no longer sends metrics to OpenAI's statsig endpoint.

### Steps:

1. **Configure Codex** with the rollout configuration:
   ```toml
   [analytics]
   enabled = false
   
   [otel]
   environment = "production"
   exporter = "otlp-http"
   metrics_exporter = "none"
   trace_exporter = "otlp-http"
   
   [otel.exporter.otlp-http]
   endpoint = "https://otel.ai.camer.digital"
   protocol = "json"
   headers = { Authorization = "Bearer <your-token>" }
   ```

2. **Monitor network traffic**:
   ```bash
   # On Linux/macOS
   sudo tcpdump -i any -n host statsig.com
   
   # Or use Wireshark to filter for statsig.com
   ```

3. **Run Codex** and perform some operations:
   ```bash
   codex
   # Execute some commands, ask questions, etc.
   ```

4. **Verify**: No outbound traffic to `statsig.com` should be observed.

### Expected Result:
✅ No network requests to statsig.com
✅ Codex functions normally without statsig

---

## Test 2: Verify OTLP Export to Governance Endpoint

**Objective**: Confirm that Codex sends telemetry to `otel.ai.camer.digital`.

### Steps:

1. **Configure Codex** with the rollout configuration (same as Test 1).

2. **Monitor network traffic**:
   ```bash
   sudo tcpdump -i any -n host otel.ai.camer.digital
   ```

3. **Run Codex** and perform operations:
   ```bash
   codex
   # Execute commands, ask questions
   ```

4. **Check governance dashboard**:
   - Open Grafana
   - Navigate to the Codex telemetry dashboard
   - Look for new execution records

### Expected Result:
✅ Network requests to `otel.ai.camer.digital` are observed
✅ Execution records appear in the governance dashboard
✅ Token counts and model information are captured

---

## Test 3: Verify Identity Binding via Token

**Objective**: Confirm that telemetry is attributed to the token's identity, not the payload's `user.email`.

### Steps:

1. **Issue a token** for `dev@example.com` via the governance registry.

2. **Configure Codex** with that token in the headers.

3. **Run Codex** with a different `user.email` in the environment (if possible):
   ```bash
   # Note: Codex may not allow overriding user.email easily,
   # but the token should still be the source of truth
   codex
   ```

4. **Check governance dashboard**:
   - Find the execution record
   - Verify `internal_user_id` matches the token's Keycloak `sub`
   - Verify `user_email` from payload is stored but not used for attribution

### Expected Result:
✅ `internal_user_id` in the database matches the token's subject
✅ Payload `user.email` is stored but not used for identity resolution
✅ No mismatch alert fires (unless emails actually differ)

---

## Test 4: Verify Mismatch Alert

**Objective**: Confirm that a mismatch between token subject and payload `user.email` triggers an alert.

### Steps:

1. **Issue a token** for `alice@example.com`.

2. **Configure Codex** with that token.

3. **Simulate a payload** with a different `user.email` (this requires modifying the OTLP payload or using a test harness).

4. **Check governance logs**:
   ```bash
   # Look for mismatch warnings
   kubectl logs -l app=governance-api | grep "identity mismatch"
   ```

5. **Check metrics**:
   ```bash
   curl http://governance-api:8080/metrics | grep identity_mismatch
   ```

### Expected Result:
✅ Warning log appears with trace_id and span_id (no PII)
✅ `governance_ingest_identity_mismatch_total` metric increments
✅ Execution is still stored with token-derived identity

---

## Test 5: Verify codex exec Token Counts

**Objective**: Confirm that `codex exec` captures token counts from span attributes.

### Steps:

1. **Configure Codex** with the rollout configuration.

2. **Run codex exec**:
   ```bash
   codex exec "List all files in the current directory"
   ```

3. **Check governance dashboard**:
   - Find the execution record
   - Verify `input_tokens` and `output_tokens` are populated
   - Verify cost is calculated (not NULL)

### Expected Result:
✅ Token counts are captured from span attributes
✅ Cost is calculated in micro-USD
✅ Execution record is complete

---

## Test 6: Verify Absent user.email is Tolerated

**Objective**: Confirm that Codex works when `user.email` is absent (API-key auth).

### Steps:

1. **Configure Codex** to use API-key authentication (not ChatGPT sign-in).

2. **Run Codex**:
   ```bash
   codex
   ```

3. **Check governance dashboard**:
   - Find the execution record
   - Verify `user_email` is NULL
   - Verify `internal_user_id` is still populated from the token

### Expected Result:
✅ Execution is stored successfully
✅ `user_email` is NULL
✅ `internal_user_id` is populated from token
✅ No errors or rejections

---

## Test 7: Verify Content Capture is Off

**Objective**: Confirm that prompt content is not captured.

### Steps:

1. **Configure Codex** with the rollout configuration.

2. **Run Codex** with sensitive content:
   ```bash
   codex
   # Ask: "What is my API key? It's sk-1234567890abcdef"
   ```

3. **Check governance database**:
   ```sql
   SELECT * FROM executions WHERE trace_id = '<your-trace-id>';
   ```

4. **Verify** that no prompt content is stored.

### Expected Result:
✅ No prompt content in the database
✅ Only metadata (token counts, model, duration) is stored
✅ `log_user_prompt` is false in the configuration

---

## Test 8: Verify Idempotency

**Objective**: Confirm that reprocessing the same telemetry doesn't create duplicates.

### Steps:

1. **Run Codex** and note the trace_id.

2. **Check execution count**:
   ```sql
   SELECT COUNT(*) FROM executions WHERE trace_id = '<your-trace-id>';
   ```

3. **Simulate reprocessing** (send the same OTLP payload again).

4. **Check execution count again**:
   ```sql
   SELECT COUNT(*) FROM executions WHERE trace_id = '<your-trace-id>';
   ```

### Expected Result:
✅ Execution count remains 1 (not 2)
✅ No duplicate records
✅ Costs are preserved from first write

---

## Test 9: Verify Cost Calculation

**Objective**: Confirm that costs are calculated correctly in micro-USD.

### Steps:

1. **Run Codex** with known token counts (e.g., 1000 input, 500 output).

2. **Check model pricing**:
   ```sql
   SELECT * FROM model_pricing WHERE model = 'gpt-4';
   ```

3. **Calculate expected cost**:
   - Example: $30/M input tokens, $60/M output tokens
   - Expected: (1000 * 30 / 1_000_000) + (500 * 60 / 1_000_000) = 0.03 + 0.03 = $0.06
   - In micro-USD: 60,000

4. **Check execution cost**:
   ```sql
   SELECT estimated_cost_micro_usd FROM executions WHERE trace_id = '<your-trace-id>';
   ```

### Expected Result:
✅ Cost matches expected calculation
✅ Cost is in micro-USD (integer)
✅ Cost is not NULL

---

## Test 10: Verify Multiple Tool Calls

**Objective**: Confirm that multiple tool calls in one execution have unique span_ids.

### Steps:

1. **Run Codex** with multiple tool calls:
   ```bash
   codex
   # Ask it to read multiple files, run multiple commands
   ```

2. **Check tool_calls table**:
   ```sql
   SELECT span_id, tool_name FROM tool_calls 
   WHERE execution_id = '<your-execution-id>';
   ```

3. **Verify** all span_ids are unique.

### Expected Result:
✅ Each tool call has a unique span_id
✅ No unique constraint violations
✅ All tool calls are stored

---

## Success Criteria

All tests pass if:
- ✅ Statsig is disabled (no traffic to OpenAI)
- ✅ OTLP export works (traffic to governance endpoint)
- ✅ Identity binding uses token, not payload
- ✅ Mismatch alerts fire correctly
- ✅ codex exec captures token counts
- ✅ Absent user.email is tolerated
- ✅ Content capture is off
- ✅ Idempotency works (no duplicates)
- ✅ Cost calculation is correct
- ✅ Multiple tool calls have unique IDs

## Troubleshooting

### No traffic to governance endpoint
- Check token is valid and not expired
- Check network connectivity to `otel.ai.camer.digital`
- Check Codex logs for errors

### Mismatch alert not firing
- Verify token subject differs from payload email
- Check governance API logs for warnings
- Verify metric is incrementing

### Token counts are NULL
- Check Codex version (older versions may not emit token counts)
- Verify span attributes contain `codex.turn.input_tokens`
- Check normalizer logs for parsing errors

### Cost is NULL
- Verify model pricing exists in `model_pricing` table
- Check token counts are not NULL
- Verify cost calculation logic

## Rollback Plan

If issues are discovered:

1. **Disable telemetry** by removing the `[otel]` section from config
2. **Re-enable statsig** by setting `[analytics] enabled = true`
3. **Investigate** logs and metrics for root cause
4. **Fix** and re-test before re-enabling

## Sign-off

- [ ] All 10 tests pass
- [ ] No errors in governance API logs
- [ ] Dashboard shows correct data
- [ ] Performance is acceptable
- [ ] Security review complete (no PII leakage)

**Tester**: _______________  
**Date**: _______________  
**Signature**: _______________