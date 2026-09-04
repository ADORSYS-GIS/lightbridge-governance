# Codex Telemetry Rollout - Test Plan

## Critical Tests

### 1. Verify Statsig is Disabled
```bash
# Monitor network traffic
sudo tcpdump -i any -n host statsig.com

# Run codex and verify no traffic to statsig.com
codex
```
**Expected**: No outbound traffic to statsig.com

### 2. Verify OTLP Export
```bash
# Monitor traffic to governance endpoint
sudo tcpdump -i any -n host otel.ai.camer.digital

# Run codex
codex
```
**Expected**: Network requests to `otel.ai.camer.digital`

### 3. Verify Identity Binding
1. Issue token for `dev@example.com`
2. Configure Codex with that token
3. Check governance dashboard
**Expected**: `internal_user_id` matches token's Keycloak `sub`

### 4. Verify codex exec Token Counts
```bash
codex exec "List files in current directory"
```
**Expected**: Token counts captured from span attributes, cost calculated

### 5. Verify Absent user.email Tolerance
1. Configure Codex with API-key auth (not ChatGPT sign-in)
2. Run codex
**Expected**: Execution stored with `user_email = NULL`, `internal_user_id` populated

### 6. Verify Content Capture Off
```sql
SELECT * FROM executions WHERE trace_id = '<your-trace-id>';
```
**Expected**: No prompt content in database

### 7. Verify Idempotency
```sql
-- Run codex, check count
SELECT COUNT(*) FROM executions WHERE trace_id = '<trace-id>';
-- Reprocess same telemetry
SELECT COUNT(*) FROM executions WHERE trace_id = '<trace-id>';
```
**Expected**: Count remains 1 (no duplicates)

### 8. Verify Cost Calculation
```sql
SELECT estimated_cost_micro_usd FROM executions WHERE trace_id = '<trace-id>';
```
**Expected**: Cost matches calculation (e.g., 1000 input + 500 output at $30/$60 per M = 60000 micro-USD)

### 9. Verify Multiple Tool Calls
```sql
SELECT span_id, tool_name FROM tool_calls WHERE execution_id = '<exec-id>';
```
**Expected**: Each tool call has unique span_id

### 10. Verify Mismatch Alert
1. Issue token for `alice@example.com`
2. Simulate payload with different `user.email`
3. Check logs
**Expected**: Warning logged

## Success Criteria
- ✅ All 10 tests pass
- ✅ No errors in governance API logs
- ✅ Dashboard shows correct data
- ✅ No PII leakage

## Troubleshooting
- **No traffic to endpoint**: Check token validity, network connectivity
- **Mismatch alert not firing**: Verify token subject differs from payload email
- **Token counts NULL**: Check Codex version, verify span attributes
- **Cost NULL**: Verify model pricing exists, token counts not NULL