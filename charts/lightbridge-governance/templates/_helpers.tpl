{{- /*
Single source for the dedicated copilot-sync OTLP collector's CR name, so it
isn't retyped (and risks drifting) across otelcollector.yaml,
servicemonitor-copilot-otel.yaml, ciliumnetworkpolicy-copilot-otel.yaml and
configmap-otlp.yaml. This is a real template, so -- unlike values.yaml, which
is never templated -- it CAN read `.Values.name` and stay in sync with it.
*/}}
{{- define "lightbridge-governance.otelCollectorName" -}}
{{ .Values.name }}-copilot-otel
{{- end -}}

{{- /*
Same, for the public AI-CLI OTLP collector (otelcollector-ai-cli.yaml,
ciliumnetworkpolicy-ai-cli-otel.yaml, ingress-ai-cli-otel.yaml). A distinct
name from the copilot collector above is load-bearing, not cosmetic: both
CiliumNetworkPolicies select on `app.kubernetes.io/instance`, which the
operator derives from the CR name, and that is the only thing keeping each
policy scoped to its own collector.
*/}}
{{- define "lightbridge-governance.aiCliOtelName" -}}
{{ .Values.name }}-ai-cli-otel
{{- end -}}

{{- /*
Same again, for the public OpenCode OTLP collector (otelcollector-opencode.yaml,
ciliumnetworkpolicy-opencode-otel.yaml, ingress-opencode-otel.yaml).

⚠️ WHY A SECOND PUBLIC COLLECTOR EXISTS AT ALL, since one endpoint serving both
client fleets is the obvious thing to want: `oidcauthextension` accepts exactly
ONE `audience` string per extension instance, and the two fleets present tokens
with different `aud` claims from the SAME issuer. Verified live against
production on 2026-09-02:

    POST https://otel.ai.camer.digital/v1/traces
      no auth                 -> 401
      Bearer <opencode token> -> 401 "failed to verify token: oidc: expected
                                 audience \"governance-auth-cli\" got
                                 [\"opencode-cli\"]"

That is not a misconfiguration to fix in place -- per lightbridge-authz ADR-0011
Decision 5 the minted token's `aud` is ALWAYS exactly the requesting `client_id`
and a client cannot ask for a different one, so `opencode-cli` tokens can never
carry `governance-auth-cli`. One audience per extension, one extension per
collector, therefore one collector per audience. Same issuer, same Alloy
exporter, same three pipelines; only the host and the trusted audience differ.
*/}}
{{- define "lightbridge-governance.opencodeOtelName" -}}
{{ .Values.name }}-opencode-otel
{{- end -}}

{{- /*
================================================================================
SHARED BODY FOR EVERY PUBLIC AI-CLIENT OTLP COLLECTOR
================================================================================

Three `define`s below -- collector CR, Ingress, CiliumNetworkPolicy -- are
instantiated once per public collector (`aiCliOtel`, `opencodeOtel`). Each takes
a dict:

    root  -- the top-level context (`.`), for .Values.name / .Release.Namespace
    otel  -- the per-collector values block (.Values.aiCliOtel, .Values.opencodeOtel)
    name  -- the collector CR name (the `*OtelName` helpers above)

They are shared rather than copy-pasted because every ⚠️ comment in them records
a production incident, and a copy is a copy that silently stops being updated.
The values blocks stay separate and are NOT renamed: `aiCliOtel` is set by name
in ai-helm-values, which lands before the chart.
*/}}

{{- /*
Public OTLP ingest for developer AI clients -- Claude Code, Codex and GitHub
Copilot in VS Code on the `aiCliOtel` instance, OpenCode on the `opencodeOtel`
one, all configured by `governance-auth` (ADR-0010).

  laptop --OTLP/HTTP--> otel[-opencode].<domain> (Traefik+cert-manager)
         --> (this collector: oidc auth -> memory_limiter -> resource -> batch)
         --> Alloy --> Tempo / Loki / Mimir

⚠️ DELIBERATELY A SEPARATE COLLECTOR from the copilot-sync one in
templates/otelcollector.yaml, which was the obvious thing to reuse. Four
reasons, all checked against that CR's live config rather than assumed:
  1. It receives OTLP/gRPC on 4317 only -- no HTTP :4318, which is what these
     clients speak through a public HTTPS ingress.
  2. Its only pipeline is metrics, exported to a `prometheus` scrape endpoint.
     Claude Code also emits logs and VS Code Copilot emits traces
     (invoke_agent/chat/execute_tool); both would have nowhere to go.
  3. Its CiliumNetworkPolicy admits only copilot-sync and Alloy, so ingress
     from Traefik would be silently dropped -- failing as "the ingress
     doesn't work" with no useful error anywhere.
  4. It is replicas:1 with no PDB and ADR-0011 classes governance_copilot_*
     as dashboard-grade *because* a restart blanks those series. Public
     laptop traffic must not be able to take out copilot-sync's metrics path.

Everything exports to Alloy rather than to Tempo/Loki/Mimir directly, matching
the existing core-gateway-traces collector: Alloy is already this cluster's
fan-out point (ADR-0007), so this collector needs one exporter and no
knowledge of the backends behind it.
*/}}
{{- define "lightbridge-governance.publicOtelCollector" -}}
{{- $root := .root -}}
{{- $otel := .otel -}}
{{- $name := .name }}
apiVersion: opentelemetry.io/v1beta1
kind: OpenTelemetryCollector
metadata:
  name: {{ $name }}
  labels:
    app: {{ $root.Values.name }}
spec:
  replicas: {{ $otel.replicas }}
  # `-contrib`, NOT the core distribution the copilot collector deliberately
  # uses: `oidcauthextension` ships only in contrib. Verified by running this
  # exact config under otel/opentelemetry-collector-contrib:0.158.0 -- the
  # oidc extension started (i.e. it fetched OIDC discovery from the issuer
  # below for real), an unauthenticated POST /v1/traces returned 401
  # "authentication didn't succeed", and a malformed bearer token returned
  # 401 "failed to parse the token". Fail-closed confirmed before this
  # template was written, not after. Re-verified identically on 0.160.0
  # (2026-09-09, values.yaml's aiCliOtel.image comment has the reason for
  # that bump) before it replaced 0.158.0 as the pin.
  image: {{ $otel.image | quote }}
  # ⚠️ PRODUCTION INCIDENT, ai-helm-values#216 (2026-08-10): enabling this
  # collector shipped it broken -- 0/2 replicas, 100% CrashLoopBackOff, every
  # single restart. The `oidc` extension makes exactly ONE OIDC-discovery
  # attempt on startup and crashes the whole process if it fails
  # (extensions/extensions.go has no retry), so any egress gap here is fatal,
  # not degraded.
  #
  # Four separate egress-rule fixes were tried here and all four failed
  # identically in production -- see ciliumnetworkpolicy-ai-cli-otel.yaml
  # for the full writeup. The actual, live-proven fix lives in that file:
  # this collector's CiliumNetworkPolicy has no `egress:` section at all.
  # Nothing here needs to route around a specific destination (no
  # `hostAliases`, no Traefik-ClusterIP indirection) because the problem was
  # never the destination -- it was any Cilium egress rule being present.
  #
  # This init container stays anyway, as defense-in-depth: it turns
  # "collector silently crash-loops forever" into a clear "OIDC discovery
  # still unreachable" log line before the main container ever gets a
  # single-shot, no-retry attempt, for whatever future cause (a real
  # transient DNS hiccup, Traefik being briefly down, etc.).
  initContainers:
    - name: wait-for-oidc-issuer
      image: {{ $otel.initContainerImage | quote }}
      command: ["sh", "-c"]
      args:
        - |
          set -eu
          for i in $(seq 1 30); do
            if curl -fsS --max-time 3 {{ printf "%s/.well-known/openid-configuration" $otel.oidc.issuerUrl | quote }} >/dev/null 2>&1; then
              echo "OIDC discovery reachable after $i attempt(s)"
              exit 0
            fi
            sleep 1
          done
          echo "OIDC discovery still unreachable after 30 attempts; giving up" >&2
          exit 1
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        runAsNonRoot: true
        # Explicit, not inferred: curlimages/curl's Dockerfile USER is the
        # symbolic `curl_user`, not a numeric UID, so the kubelet cannot
        # verify runAsNonRoot from the image alone and refuses to start the
        # container (`CreateContainerConfigError`) without this -- hit live
        # in prod after the first version of this fix merged. uid=100/
        # gid=101 confirmed by actually running `id` in curlimages/curl:8.11.1,
        # not read off documentation.
        runAsUser: 100
        runAsGroup: 101
        capabilities:
          drop: [ALL]
  ports:
    - name: otlp-http
      port: 4318
      protocol: TCP
{{- if $otel.s3.enabled }}
  # The awss3 exporter reads AWS credentials from the standard chain; these env
  # vars point it at the same ExternalSecret material copilot.s3 uses
  # (externalSecret.s3AccessKeyProperty / s3SecretKeyProperty). Required (never
  # optional) so a pod that beats ESO waits in ContainerCreating rather than
  # archiving nothing / failing auth forever -- same rule as every other
  # secretKeyRef env in this chart.
  env:
    - name: AWS_ACCESS_KEY_ID
      valueFrom:
        secretKeyRef:
          name: {{ $root.Values.name }}-env
          key: {{ $root.Values.externalSecret.s3AccessKeyProperty }}
    - name: AWS_SECRET_ACCESS_KEY
      valueFrom:
        secretKeyRef:
          name: {{ $root.Values.name }}-env
          key: {{ $root.Values.externalSecret.s3SecretKeyProperty }}
    # ⚠️ PRODUCTION INCIDENT (2026-09-09): without these, the awss3
    # exporter's S3 transfer manager defaults to
    # RequestChecksumCalculation=WhenSupported and attaches an
    # `x-amz-checksum-crc32` header (plus a matching SignedHeaders entry)
    # to every PutObject -- Hetzner's Ceph RGW backend rejected that shape
    # with 403 SignatureDoesNotMatch on 100% of uploads, silently dropping
    # the entire raw-OTLP archive leg. `when_required` is the pre-checksum
    # behavior: plain SigV4-signed requests, no extra header. Only takes
    # effect on 0.160.0+ -- open-telemetry/opentelemetry-collector-
    # contrib#50184/#50627, the transfer manager on 0.158.0 built its own
    # client config and never read these vars at all, so setting them
    # there would have been silently ignored, not a smaller fix.
    - name: AWS_REQUEST_CHECKSUM_CALCULATION
      value: "when_required"
    - name: AWS_RESPONSE_CHECKSUM_VALIDATION
      value: "when_required"
{{- end }}
{{- if or $otel.s3.enabled $otel.refusalCapture.enabled }}
  # The awss3 exporter stages each upload in a temp file before PUT; the
  # collector runs readOnlyRootFilesystem, so give it a writable /tmp.
  # The refusal-capture emptyDir (lightbridge-governance#275) is a second
  # writable mount for the same reason -- see the `logs/refusals` pipeline.
  volumes:
{{- if $otel.s3.enabled }}
    - name: tmp
      emptyDir: {}
{{- end }}
{{- if $otel.refusalCapture.enabled }}
    - name: refusal-logs
      emptyDir:
        # Safety valve, not a sizing knob: refusals are low-volume, but a log
        # burst (or a misbehaving filter) must not fill the node disk.
        sizeLimit: {{ $otel.refusalCapture.volumeSizeLimit | quote }}
{{- end }}
  volumeMounts:
{{- if $otel.s3.enabled }}
    - name: tmp
      mountPath: /tmp
{{- end }}
{{- if $otel.refusalCapture.enabled }}
    - name: refusal-logs
      mountPath: {{ $otel.refusalCapture.logDir | quote }}
{{- end }}
{{- end }}
  podSecurityContext:
    runAsNonRoot: true
    seccompProfile:
      type: RuntimeDefault
  securityContext:
    allowPrivilegeEscalation: false
    readOnlyRootFilesystem: true
    capabilities:
      drop: [ALL]
  resources:
    requests:
      cpu: {{ $otel.resources.requests.cpu | quote }}
      memory: {{ $otel.resources.requests.memory | quote }}
    limits:
      cpu: {{ $otel.resources.limits.cpu | quote }}
      memory: {{ $otel.resources.limits.memory | quote }}
  config:
    extensions:
      # Validates the caller's bearer token against the lightbridge-authz
      # API-key issuer, which already serves OIDC discovery + JWKS publicly
      # (both confirmed HTTP 200). No Authorino and no core-gateway listener
      # are involved: this collector authenticates its own callers.
      #
      # These are long-lived (120d) revocable API keys ON PURPOSE, not
      # Keycloak access tokens. All three clients read their OTEL config once
      # at process start and none has a credential-helper hook for OTLP
      # headers, so a 300s token would export for five minutes and then 401
      # silently for the rest of a session. See app/governance-auth/src/otel.rs.
{{- /*
⚠️ ONE audience per `oidc` extension instance -- this is why there are two
public collectors rather than one. A token minted for a different `client_id`
is refused here with `oidc: expected audience "<this>" got ["<theirs>"]`,
proven live against production on 2026-09-02; the transcript is in the
`opencodeOtelName` helper above.

⚠️ The rendered comment above is stale for the `aiCliOtel` instance and wrong
for `opencodeOtel`, and is kept byte-for-byte anyway: it is inside the
collector's `config`, so editing it is a change to what production renders.
Current reality, per this chart's own values.yaml: `aiCliOtel` callers present
a short-lived token authz MINTED via RFC 8693 token-exchange (Claude Code
re-invokes `governance-auth otel headers` on an interval), and `opencodeOtel`
callers present a short-lived authz device-code token that a `tokenCommand`
credential helper reads out of `@vymalo/opencode-oauth2`'s cache. Neither is a
120d API key. Correct it in a change that owns that rendering diff.
*/}}
      oidc:
        issuer_url: {{ $otel.oidc.issuerUrl | quote }}
        audience: {{ $otel.oidc.audience | quote }}

    receivers:
      otlp:
        protocols:
          http:
            endpoint: 0.0.0.0:4318
            # Without this the receiver accepts anything that reaches it, and
            # the endpoint is on the public internet.
            auth:
              authenticator: oidc
{{- /*
lightbridge-governance#284 AC1, CORRECTED before landing: this does NOT make
`oidcauthextension` log the real client address on a REFUSAL. Verified against
the exact pinned image (extension.go's Authenticate() reads only
client.FromContext(ctx).Addr, which confighttp's clientinfohandler.go sets
from req.RemoteAddr alone -- unconditionally, on this version and on
opentelemetry-collector's main branch today) and against the open, unfixed
upstream gap (open-telemetry/opentelemetry-collector#4901, filed 2022): no
version of this extension reads X-Forwarded-For, gated by include_metadata or
otherwise. The refusal log's `client_ip` stays the Traefik pod IP regardless
of this setting -- that half of AC1 is out of reach without a fork of the
extension and is NOT attempted here.

What this DOES do: populate client.Info.Metadata (separate from .Addr) so the
`resource` processor's `from_context` reads below can see request headers at
all. Without it those reads silently produce nothing, not an error.
*/}}
            include_metadata: true
{{- if $otel.refusalCapture.enabled }}
      # Reads back the collector's OWN logs (written by `service.telemetry.logs`
      # below) and keeps only the oidc-extension refusals. `file_log`, not the
      # deprecated `filelog` alias (0.158.0 warns on the latter). `start_at: end`
      # seeks past pre-existing content so a container restart never replays old
      # lines. `parse_to: attributes` (NOT body) because OTTL cannot address
      # sub-paths of `body` -- verified live in the Phase 1a spike.
      file_log/refusals:
        include: [{{ $otel.refusalCapture.logPath | quote }}]
        start_at: end
        operators:
          - type: json_parser
            parse_from: body
            parse_to: attributes
{{- end }}

    processors:
      # MUST be first: it sheds load before an OOM, and an OOM loses data
      # outright. Same rule as every other collector here (ADR-0034).
      memory_limiter:
        check_interval: 1s
        limit_mib: {{ $otel.memoryLimiterMib }}
        spike_limit_mib: {{ $otel.memorySpikeLimitMib }}
      resource:
        attributes:
          # Marks the source so an operator can tell laptop telemetry from
          # in-cluster telemetry in Tempo/Loki without guessing.
{{- /*
Now also which client FLEET it came from, since there is more than one public
collector. The token is the RFC-0003 §2 matrix row's name, lowercased to match
`microsoft-foundry`'s existing convention (`ai-cli`, `opencode`) -- it is a
values field, not a literal, so do not invent a new one per collector.
*/}}
          - action: upsert
            key: governance.source
            value: {{ $otel.sourceAttribute }}
{{- /*
lightbridge-governance#284 AC1, the half that IS reachable: attribute the real
client on telemetry that gets PAST auth (a refusal never reaches this
processor at all -- oidcauthextension runs at the receiver, before any
pipeline processor). Staged into a scratch key first because this processor
can only copy the raw header verbatim; picking the trustworthy entry out of it
needs the `transform` processor below, which runs immediately after.

⚠️ CALLER-INJECTABLE ATTRIBUTES, found in PR review (#305) and fixed before
merge -- `resourceSpans[].resource.attributes` is literal, fully
caller-controlled JSON on this public endpoint (the request BODY, nothing to
do with headers). Two `delete` actions immediately below wipe BOTH
`client.address` and this scratch key unconditionally, before anything else
runs, so a caller cannot pre-seed either one and have it survive:
  - Without the `client.address` delete: a caller submits
    `resource.attributes: [{"key":"client.address","value":{"stringValue":
    "6.6.6.6"}}]` directly, with NO X-Forwarded-For at all -- the transform
    processor's `where` guard below never fires (no real header means no
    scratch value), so the caller's own value is never overwritten. Verified
    live: reproduced, then fixed by this delete.
  - Without the `client.address.xff_raw` delete: `action: insert` (the
    original code here) only writes when the key does NOT already exist, so
    a caller who also submits `client.address.xff_raw` in the same request
    keeps their own value even alongside a completely legitimate
    X-Forwarded-For header -- `upsert` alone does not fully fix this either:
    upsert still only ACTS when `from_context` has something to write, so a
    caller-seeded value survives untouched whenever there is no real header
    at all. Verified live, both variants, before landing this delete.
*/}}
          - action: delete
            key: client.address
          - action: delete
            key: client.address.xff_raw
          - action: upsert
            key: client.address.xff_raw
            from_context: "metadata.x-forwarded-for"
{{- /*
⚠️ SPOOFABILITY, decided not defaulted (ai-helm#1081 AC4's requirement,
carried over here because this is where the header is actually consumed):
Traefik's entrypoints APPEND the peer address it observed to any
X-Forwarded-For it received -- confirmed live on hetzner-prod
(daemonset.apps/traefik's `--entryPoints.web(secure).forwardedHeaders.
trustedIPs=10.0.0.0/16` matches the Hetzner LB's PROXY-protocol CIDR, and
Traefik's own default behaviour is append, not replace, unless
`notAppendXForwardedFor` is set -- it is not, here). So a caller CAN put
anything it wants in the header it sends; what it cannot do is control what
Traefik appends after it. The transform processor below therefore reads only
the RIGHTMOST entry and discards everything to its left as untrusted --
never the first, never "the whole header". A single-hop push (the normal
case: no upstream proxy the caller controls) still works because Split on a
one-element string returns that one element.

This was verified end-to-end against the exact pinned image
(otel/opentelemetry-collector-contrib:0.158.0, via `docker run` locally, not
just rendered): a push carrying `X-Forwarded-For: 6.6.6.6, 10.42.2.109`
produced `client.address: 10.42.2.109` on the resulting ResourceSpans /
ResourceLogs -- the spoofed leading entry never survives. Re-run
identically by assert-client-address-xff.sh against 0.160.0 (2026-09-09,
same bump as values.yaml's aiCliOtel.image comment) before that became
the pin -- this OTTL is untouched by that bump, only re-verified on it.

⚠️ EMPTY/BLANK ENTRY, also found in review: a present-but-empty header (or a
trailing comma, e.g. `X-Forwarded-For: 1.2.3.4,`) is not `nil` -- the `!= nil`
guard alone let `client.address` get set to `""`, a fabricated "attribution is
present" signal that isn't one. The second `where` clause below re-derives the
same rightmost/trimmed value and requires it non-empty; verbose (the whole
Split/Len/Trim chain repeated) rather than a temp variable, because OTTL has
no intermediate-variable binding across a `where` clause and a `set()`'s value
expression -- each is evaluated independently. Verified live: a bare
`X-Forwarded-For:` and a trailing-comma `X-Forwarded-For: 1.2.3.4,` both now
produce no `client.address` attribute at all, not an empty one.
*/}}
      transform/client_address_from_xff:
        log_statements:
          - context: resource
            statements:
              - set(attributes["client.address"], Trim(Split(attributes["client.address.xff_raw"], ",")[Len(Split(attributes["client.address.xff_raw"], ",")) - 1])) where attributes["client.address.xff_raw"] != nil and Trim(Split(attributes["client.address.xff_raw"], ",")[Len(Split(attributes["client.address.xff_raw"], ",")) - 1]) != ""
              - delete_key(attributes, "client.address.xff_raw")
        trace_statements:
          - context: resource
            statements:
              - set(attributes["client.address"], Trim(Split(attributes["client.address.xff_raw"], ",")[Len(Split(attributes["client.address.xff_raw"], ",")) - 1])) where attributes["client.address.xff_raw"] != nil and Trim(Split(attributes["client.address.xff_raw"], ",")[Len(Split(attributes["client.address.xff_raw"], ",")) - 1]) != ""
              - delete_key(attributes, "client.address.xff_raw")
        metric_statements:
          - context: resource
            statements:
              - set(attributes["client.address"], Trim(Split(attributes["client.address.xff_raw"], ",")[Len(Split(attributes["client.address.xff_raw"], ",")) - 1])) where attributes["client.address.xff_raw"] != nil and Trim(Split(attributes["client.address.xff_raw"], ",")[Len(Split(attributes["client.address.xff_raw"], ",")) - 1]) != ""
              - delete_key(attributes, "client.address.xff_raw")
{{- if $otel.refusalCapture.enabled }}
      # ⚠️ lightbridge-governance#275: classify refused-ingest reasons. The
      # `file_log/refusals` receiver reads the collector's own logs; this pair
      # keeps only the oidc-extension refusals and stamps a `refusal.reason`.
      #
      # ⚠️ The filter DROPS records that MATCH the condition -- so it is negated
      # to keep refusals. It discriminates on `otelcol.component.id == "oidc"`,
      # NOT just the msg: a substring IsMatch on msg alone matches the debug
      # exporter's serialization of a refusal too, which re-exports it and loops
      # exponentially (observed live in the Phase 1a spike: file 8KB -> 3.1MB in
      # seconds). The oidc extension tags its warn lines with
      # otelcol.component.id="oidc"; every other component (incl. the debug
      # exporter, "debug") is dropped. This is also the correct production
      # discriminator -- only the oidc extension emits refusals.
      filter/refusals:
        logs:
          log_record:
            - 'not (attributes["otelcol.component.id"] == "oidc" and IsMatch(attributes["msg"], "Authentication failed"))'
      # ⚠️ The reason strings below must match the upstream `oidcauthextension`
      # warn messages byte-for-byte (extension.go:174/194). They were verified
      # against the exact pinned image in the Phase 1a spike -- live for
      # missing_credential/malformed_token, and in the exact upstream zap format
      # for the rest. `verification_failed` is the generic catch-all for a
      # signature/verification error that is not expired or wrong-audience;
      # `unknown` is the fallback for any unrecognised msg (a future upstream
      # message degrades here, never to a wrong reason).
      transform/classify_refusal:
        log_statements:
          - context: log
            statements:
              - 'set(attributes["refusal.reason"], "missing_credential") where attributes["msg"] == "Authentication failed: missing or empty header"'
              - 'set(attributes["refusal.reason"], "malformed_token") where attributes["msg"] == "Authentication failed: invalid header format" or attributes["msg"] == "Authentication failed: could not parse issuer from token"'
              - 'set(attributes["refusal.reason"], "unknown_issuer") where attributes["msg"] == "Authentication failed: could not resolve provider"'
              - 'set(attributes["refusal.reason"], "expired") where attributes["msg"] == "Authentication failed: token verification failed" and IsMatch(attributes["error"], "token is expired")'
              - 'set(attributes["refusal.reason"], "wrong_audience") where attributes["msg"] == "Authentication failed: token verification failed" and IsMatch(attributes["error"], "expected audience")'
              - 'set(attributes["refusal.reason"], "verification_failed") where attributes["msg"] == "Authentication failed: token verification failed" and attributes["refusal.reason"] == nil'
              - 'set(attributes["refusal.reason"], "unknown") where attributes["refusal.reason"] == nil'
              - 'set(attributes["refusal.issuer"], attributes["issuer"]) where attributes["issuer"] != nil'
{{- end }}
      batch:
        send_batch_size: 512
        timeout: 5s

    exporters:
      otlp/alloy:
        endpoint: {{ $otel.alloyEndpoint | quote }}
        tls:
          insecure: true
{{- if $otel.s3.enabled }}
      # Raw OTLP archive leg (lightbridge-authz #692 / #589): a third exporter,
      # parallel to -- not behind -- the Alloy leg, writing verbatim OTLP to
      # S3-compatible object storage so a field can later be promoted to a
      # column with historical backfill. Independent queue/retry from Alloy: a
      # blocked S3 sink alarms but never blocks the governed/observability path
      # (D10). The per-source prefix comes from the `governance.source` resource
      # attribute stamped by the resource processor, so the key layout is
      # <basePrefix>/<source>/<yyyy>/<mm>/<dd>/... (a cheap prefix read for a
      # one-source/window promotion backfill).
      awss3:
        s3uploader:
          region: {{ $otel.s3.region | quote }}
          s3_bucket: {{ $otel.s3.bucket | quote }}
          s3_base_prefix: {{ $otel.s3.basePrefix | quote }}
          s3_prefix: {{ $otel.sourceAttribute | quote }}
          s3_partition_format: {{ $otel.s3.partitionFormat | quote }}
          s3_partition_timezone: {{ $otel.s3.partitionTimezone | quote }}
          s3_force_path_style: {{ $otel.s3.forcePathStyle }}
          endpoint: {{ $otel.s3.endpoint | quote }}
          # awss3exporter.Config has no top-level `compression` key -- it lives
          # under s3uploader as configcompression.Type (gzip/zstd/unset). A
          # top-level `compression` here fails config validation at startup
          # ("has invalid keys: compression"), which crashes both otel
          # collector pods identically (D10/D11's third exporter leg).
          compression: {{ $otel.s3.compression | quote }}
        resource_attrs_to_s3:
          s3_prefix: "governance.source"
        marshaler: {{ $otel.s3.format | quote }}
        sending_queue:
          enabled: true
          num_consumers: {{ $otel.s3.numConsumers }}
          queue_size: {{ $otel.s3.queueSize }}
        retry_on_failure:
          enabled: true
          max_elapsed_time: 5m
{{- end }}

    service:
      extensions: [oidc]
{{- if $otel.refusalCapture.enabled }}
      # ⚠️ lightbridge-governance#275: the collector writes its OWN logs to the
      # refusal file so `file_log/refusals` can read them back. `encoding: json`
      # is required for the json_parser to work -- and it also switches the
      # collector's stderr to JSON (a deliberate format change, and better for
      # Loki). `output_paths` keeps stderr AND adds the file, so existing
      # stderr-based log collection is unaffected.
      telemetry:
        logs:
          encoding: json
          output_paths: [stderr, {{ $otel.refusalCapture.logPath | quote }}]
{{- end }}
      pipelines:
        # All three signals: Claude Code emits metrics + logs, Codex emits
        # logs + traces, VS Code Copilot emits traces + metrics + events.
        # Omitting any one of these silently drops that client's telemetry.
{{- /*
OpenCode is request-grain too (RFC-0003 §2), so the `opencodeOtel` instance
wants the same three pipelines and gets them from this same body.

`transform/client_address_from_xff` runs immediately after `resource`, not
before it (it consumes the scratch key `resource` just staged) and not last
(it deletes that scratch key before `batch` -- a batched, multi-resource
payload past this point must never carry it forward).
*/}}
        traces:
          receivers: [otlp]
          processors: [memory_limiter, resource, transform/client_address_from_xff, batch]
          exporters: [otlp/alloy{{- if $otel.s3.enabled }}, awss3{{- end }}]
        metrics:
          receivers: [otlp]
          processors: [memory_limiter, resource, transform/client_address_from_xff, batch]
          exporters: [otlp/alloy{{- if $otel.s3.enabled }}, awss3{{- end }}]
        logs:
          receivers: [otlp]
          processors: [memory_limiter, resource, transform/client_address_from_xff, batch]
          exporters: [otlp/alloy{{- if $otel.s3.enabled }}, awss3{{- end }}]
{{- if $otel.refusalCapture.enabled }}
        # ⚠️ lightbridge-governance#275: classified refusals go to Alloy/Loki.
        # No feedback loop here: the otlp/alloy exporter is a network export and
        # does NOT write back into the collector's own log file (unlike the
        # debug exporter used in the Phase 1a spike). memory_limiter first, per
        # the house rule (ADR-0034).
        logs/refusals:
          receivers: [file_log/refusals]
          processors: [memory_limiter, filter/refusals, transform/classify_refusal, batch]
          exporters: [otlp/alloy]
{{- end }}
{{- end -}}

{{- /*
Public TLS entrypoint for a public AI-client OTLP collector.

Traefik rather than the Envoy core-gateway, deliberately: otel.<domain> and
otel-opencode.<domain> both already resolve to the Traefik load balancer (the
wildcard *.ai.camer.digital record), exactly as auth.ai.camer.digital does, so
this needs no DNS change and no new Gateway listener. Authentication does NOT
come from Authorino here -- the collector authenticates its own callers via the
oidc extension (see the collector define above), so nothing in the
Envoy/Authorino path is required for these hosts.

TLS is issued by cert-manager's ingress-shim: the `cluster-issuer` annotation
plus the `tls:` block below is enough for it to create and renew the
Certificate. There is no hand-written Certificate resource on purpose -- one
would duplicate what the annotation already does and give two controllers an
opinion about the same Secret.

The backend Service is named `<collector CR name>-collector`, which is the
OpenTelemetry Operator's own naming convention (the same one
templates/servicemonitor-copilot-otel.yaml relies on for the copilot
collector).
*/}}
{{- define "lightbridge-governance.publicOtelIngress" -}}
{{- $root := .root -}}
{{- $otel := .otel -}}
{{- $name := .name }}
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: {{ $name }}
  labels:
    app: {{ $root.Values.name }}
  annotations:
    cert-manager.io/cluster-issuer: {{ $otel.ingress.clusterIssuer | quote }}
    {{- with $otel.ingress.annotations }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
spec:
  ingressClassName: {{ $otel.ingress.className | quote }}
  rules:
    - host: {{ $otel.ingress.host | quote }}
      http:
        paths:
          # OTLP/HTTP puts each signal on its own path (/v1/traces,
          # /v1/metrics, /v1/logs), so this is a prefix match on `/` rather
          # than three exact rules -- the SDKs append the suffix themselves
          # from the base endpoint governance-auth writes.
          - path: /
            pathType: Prefix
            backend:
              service:
                name: {{ printf "%s-collector" $name }}
                port:
                  number: 4318
  tls:
    - secretName: {{ printf "%s-tls" $otel.ingress.host }}
      hosts:
        - {{ $otel.ingress.host | quote }}
{{- end -}}

{{- /*
Ingress-only policy for a public AI-client OTLP collector. No `egress:`
section, on purpose -- read the incident writeup below before adding one back.

⚠️ Any CiliumNetworkPolicy with an `ingress:` block makes the selected pods
deny-by-default for ingress, INCLUDING kubelet's liveness/readiness probes,
which arrive with host/remote-node identity rather than a pod identity.
Without `fromEntities: [host, remote-node, health]` this collector never goes
Ready and reads exactly like a crash loop rather than a network policy
problem -- the trap charts/redact-gateway's CNP documents and that was
verified live there.

The endpointSelector pins `app.kubernetes.io/instance` to THIS collector.
The copilot collector's policy does the same for its own, and the two public
collectors' policies do it for each other -- so no policy ever applies to a
collector it was not written for, despite all of them sharing the operator's
managed-by/component labels. If that instance key were dropped, each policy
would apply to every collector and Traefik's traffic would be denied by the
copilot policy.

⚠️⚠️⚠️ WHY THERE IS NO `egress:` SECTION HERE, EXHAUSTIVELY, because the
obvious-looking alternative is a guaranteed outage
(lightbridge-governance#85/#87/#88/#89 incident, 2026-08-10 -- FOUR merged,
rolled-out PRs, each looking correctly evidenced, each failing identically
in production):

  1. `toFQDNs: matchName: auth.ai.camer.digital` -- looked correctly compiled
     (`cilium bpf policy get` showed the right rule, right identity, DNS
     resolving fine through the proxy) and still 100% timed out, on two
     different nodes, zero drop verdicts ever logged.
  2. `toCIDR: 46.225.40.134/32` (the issuer's LB IP) -- identical result.
     Also 100% timeout, also zero bytes/packets ever recorded against a rule
     Cilium's own tooling reported as freshly compiled and correctly
     enforced.
  3. `toEndpoints` matching Traefik's own pods, reached via its in-cluster
     ClusterIP instead of the external LB IP (thinking the problem was
     specifically "external" egress) -- ALSO 100% timeout, despite an
     `toEndpoints` rule to Alloy on this SAME policy working instantly for
     the SAME endpoint at the SAME time.
  4. `toEntities: [world]` -- the broadest possible "allow external traffic"
     primitive, no per-IP/per-endpoint identity resolution at all. Also
     100% timeout.

All four were tested live against `hetzner-prod`, not just rendered and
assumed. The actual isolating test, live, twice independently: deleting
this CiliumNetworkPolicy object entirely, and separately, applying a
version with an `ingress:` block but NO `egress:` key at all -- both
configurations let the exact same request through instantly (HTTP 200,
~24ms) that timed out 100% of the time under every egress rule shape above,
including ones covering the same Alloy/DNS traffic that demonstrably works.

Conclusion: it is not which Cilium egress primitive is used, and not which
destination. It's that an `egress:` section being present AT ALL on this
CiliumNetworkPolicy breaks external (non-cluster-CIDR) TCP/443 traffic on
this cluster's current Cilium version/config, for reasons not explained by
any control-plane state this investigation could reach (every failed rule
was correctly compiled, correctly identity-matched, and never logged a
drop verdict). That's a cluster/Cilium-level question, not one this chart's
resources can fix -- attempted, exhaustively, and reverted each time.

So: egress for this collector is intentionally UNRESTRICTED (matches Alloy
push and OIDC discovery/JWKS both being legitimate needs anyway; ingress
stays tightly scoped to Traefik + kubelet probes, which is where the actual
external attack surface is). If you're reading this because you want to
narrow egress again: don't, until whatever's actually broken here is fixed
at the Cilium/cluster level -- re-adding an `egress:` section, in ANY
shape, has broken this collector 100% of the time, four different ways, so
far.
*/}}
{{- define "lightbridge-governance.publicOtelNetworkPolicy" -}}
{{- $root := .root -}}
{{- $otel := .otel -}}
{{- $name := .name }}
apiVersion: cilium.io/v2
kind: CiliumNetworkPolicy
metadata:
  name: {{ $name }}
  labels:
    app: {{ $root.Values.name }}
spec:
  description: >-
    The {{ $otel.displayName }} OTLP collector accepts OTLP/HTTP from Traefik on 4318 and
    kubelet probes. Egress is intentionally unrestricted -- see this file's
    header comment for why a scoped egress rule, in every shape tried, broke
    external port-443 traffic on this cluster.
  endpointSelector:
    matchLabels:
      app.kubernetes.io/managed-by: opentelemetry-operator
      app.kubernetes.io/component: opentelemetry-collector
      app.kubernetes.io/instance: {{ printf "%s.%s" $root.Release.Namespace $name }}
  ingress:
    # Traefik's proxied OTLP/HTTP. Cross-namespace, so every key is
    # `k8s:`-prefixed, matching this chart's existing Alloy rules.
    - fromEndpoints:
        - matchLabels:
            k8s:io.kubernetes.pod.namespace: {{ $otel.ingress.traefikNamespace }}
      toPorts:
        - ports:
            - port: "4318"
              protocol: TCP
    # Kubelet probes -- see the trap called out above this policy.
    - fromEntities: [host, remote-node, health]
{{- end -}}
