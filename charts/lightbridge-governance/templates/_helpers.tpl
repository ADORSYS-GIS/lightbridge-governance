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
