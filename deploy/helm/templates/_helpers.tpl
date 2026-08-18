{{/* Standard labels, the app.kubernetes.io set. */}}
{{- define "dynamic-config.labels" -}}
app.kubernetes.io/name: dynamic-config
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/* An image reference: digest wins over tag, latest is refused. */}}
{{- define "dynamic-config.image" -}}
{{- if eq .tag "latest" }}{{ fail "tag \"latest\" is not an identity — pin a version or a digest" }}{{- end }}
{{- if .digest }}{{ printf "%s@%s" .image .digest }}{{- else }}{{ printf "%s:%s" .image .tag }}{{- end }}
{{- end }}

{{/* The restricted-PSS container security context, shared. */}}
{{- define "dynamic-config.containerSecurity" -}}
runAsNonRoot: true
runAsUser: 65532
runAsGroup: 65532
allowPrivilegeEscalation: false
capabilities:
  drop: ["ALL"]
readOnlyRootFilesystem: true
seccompProfile:
  type: RuntimeDefault
{{- end }}

{{/*
The webhook's TLS material, one value whichever way it is issued.
Self-signed mode generates a CA and a ten-year pair at install time and
REUSES the existing Secret on upgrade (the lookup), so an upgrade does
not silently rotate what the webhook configuration trusts.
*/}}
{{- define "dynamic-config.selfSignedTls" -}}
{{- $secret := lookup "v1" "Secret" .Release.Namespace "dynamic-config-webhook-tls" -}}
{{- if $secret -}}
crt: {{ index $secret.data "tls.crt" }}
key: {{ index $secret.data "tls.key" }}
ca: {{ index $secret.data "ca.crt" }}
{{- else -}}
{{- $ca := genCA "dynamic-config-webhook-ca" 3650 -}}
{{- $dns := list (printf "dynamic-config-webhook.%s.svc" .Release.Namespace) (printf "dynamic-config-webhook.%s" .Release.Namespace) -}}
{{- $cert := genSignedCert (first $dns) nil $dns 3650 $ca -}}
crt: {{ $cert.Cert | b64enc }}
key: {{ $cert.Key | b64enc }}
ca: {{ $ca.Cert | b64enc }}
{{- end -}}
{{- end }}
