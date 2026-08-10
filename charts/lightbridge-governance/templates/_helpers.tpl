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
