# Onboard a Foundry integration

**When:** somebody needs to point a Microsoft Foundry hosted agent at us.

## 1. Create the application and integration

```bash
governance-ctl application create --name "support-agent" --owner "@someone"
governance-ctl integration create --application support-agent --provider microsoft-foundry
```

The token is printed **once**. Only its argon2id hash is stored, so it cannot be recovered
-- if it is lost, revoke and reissue (see the revocation runbook). Hand it over out of band;
do not paste it into a ticket.

Default `content_capture` is `metadata_only`. Raising it to `redacted` or `full` is a
deliberate act with retention consequences -- see §5.

## 2. Give them the configuration

```yaml
environment_variables:
  - name: OTEL_EXPORTER_OTLP_ENDPOINT
    value: https://otel.ai.camer.digital
  - name: OTEL_EXPORTER_OTLP_PROTOCOL
    value: http/protobuf
  - name: OTEL_EXPORTER_OTLP_HEADERS
    value: Authorization=Bearer <token>
```

⚠️ **Tell them changing these later requires publishing a new agent version.** That is a
Foundry constraint, not ours, and it is why the endpoint and token are long-lived with
server-side revocation rather than short-lived with rotation.

## 3. Confirm telemetry is arriving

```bash
governance-ctl integration status --id <integration-id>
```

`last_telemetry_at` is written on ingest. Until it is non-null, nothing has arrived --
**and a dashboard with no violations looks exactly the same as one with no telemetry**, so
check this field rather than the board.

If it stays null after they have run the agent:

```bash
# Did the request reach the gateway and get authenticated?
kubectl --context admin@homeos -n envoy-gateway-system logs deploy/<envoy> --tail=100 \
  | grep otel.ai.camer.digital
# Is the collector accepting spans?
kubectl -n governance exec deploy/foundry-gateway-collector -- \
  wget -qO- localhost:8888/metrics | grep receiver_accepted
```

A 401 here means the token, the AuthConfig or the `sectionNames` listener list -- in
practice, most often the listener list, because a listener missing from `sectionNames` is
not covered by Authorino at all.

## 4. Verify redaction before they send anything real

Have them run one execution with a deliberately seeded fake secret, then **read the stored
span**:

```sql
-- via the governance datasource, or psql against the -ro replica
SELECT trace_id, status FROM foundry_execution ORDER BY started_at DESC LIMIT 5;
```

and inspect the corresponding trace in Tempo. Trust the stored artefact, not the
processor's own counter -- the counter says what it thinks it did.

## 5. If they want content capture

`redacted` or `full` requires: an explicit request from the application owner, a note of
who approved it, and awareness that **full content retention is governed by Loki's
per-stream retention, which is not configured yet** (ADR-0004). Until it is, `full` means
90 days, not 7. Do not promise 7.
