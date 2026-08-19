{{/* Base name: chart name unless overridden. */}}
{{- define "dynamic-config.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end }}

{{/* Fullname: fixed, cluster-recognisable, overridable. */}}
{{- define "dynamic-config.fullname" -}}
{{- default (include "dynamic-config.name" .) .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- end }}

{{- define "dynamic-config.webhook.fullname" -}}
{{- printf "%s-webhook" (include "dynamic-config.fullname" .) -}}
{{- end }}

{{- define "dynamic-config.operator.fullname" -}}
{{- printf "%s-operator" (include "dynamic-config.fullname" .) -}}
{{- end }}

{{- define "dynamic-config.webhook.serviceAccountName" -}}
{{- if .Values.webhook.serviceAccount.create -}}
{{- default (include "dynamic-config.webhook.fullname" .) .Values.webhook.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.webhook.serviceAccount.name -}}
{{- end -}}
{{- end }}

{{- define "dynamic-config.operator.serviceAccountName" -}}
{{- if .Values.operator.serviceAccount.create -}}
{{- default (include "dynamic-config.operator.fullname" .) .Values.operator.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.operator.serviceAccount.name -}}
{{- end -}}
{{- end }}

{{/* Standard labels, the app.kubernetes.io set, plus the operator's own. */}}
{{- define "dynamic-config.labels" -}}
app.kubernetes.io/name: {{ include "dynamic-config.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end }}

{{/* Selector labels never move on upgrade: no version, no chart. */}}
{{- define "dynamic-config.webhook.selectorLabels" -}}
app.kubernetes.io/name: {{ include "dynamic-config.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: webhook
{{- end }}

{{- define "dynamic-config.operator.selectorLabels" -}}
app.kubernetes.io/name: {{ include "dynamic-config.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: operator
{{- end }}

{{/* commonAnnotations, rendered where a resource has no other notes. */}}
{{- define "dynamic-config.annotations" -}}
{{- with .Values.commonAnnotations }}
annotations:
{{ toYaml . | indent 2 }}
{{- end }}
{{- end }}

{{/* An image reference: digest wins over tag, latest is refused. */}}
{{/* Takes (dict "component" <.Values.x> "root" $): an empty tag means
     "the chart's appVersion, v-prefixed" — the tag release.yml pushes —
     so a release bumps ONE number and the three images follow. */}}
{{- define "dynamic-config.image" -}}
{{- $c := .component -}}
{{- $tag := $c.tag | default (printf "v%s" .root.Chart.AppVersion) -}}
{{- if eq $tag "latest" }}{{ fail "tag \"latest\" is not an identity — pin a version or a digest" }}{{- end }}
{{- if $c.digest }}{{ printf "%s@%s" $c.image $c.digest }}{{- else }}{{ printf "%s:%s" $c.image $tag }}{{- end }}
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
Self-signed mode generates a CA and a pair at install time and REUSES
the existing Secret on upgrade (the lookup), so an upgrade does not
silently rotate what the webhook configuration trusts.
*/}}
{{- define "dynamic-config.selfSignedTls" -}}
{{- $name := printf "%s-tls" (include "dynamic-config.webhook.fullname" .) -}}
{{- $secret := lookup "v1" "Secret" .Release.Namespace $name -}}
{{- if $secret -}}
crt: {{ index $secret.data "tls.crt" }}
key: {{ index $secret.data "tls.key" }}
ca: {{ index $secret.data "ca.crt" }}
{{- else -}}
{{- $days := int .Values.webhook.selfSignedDays -}}
{{- $ca := genCA (printf "%s-ca" (include "dynamic-config.webhook.fullname" .)) $days -}}
{{- $dns := list (printf "%s.%s.svc" (include "dynamic-config.webhook.fullname" .) .Release.Namespace) (printf "%s.%s" (include "dynamic-config.webhook.fullname" .) .Release.Namespace) -}}
{{- $cert := genSignedCert (first $dns) nil $dns $days $ca -}}
crt: {{ $cert.Cert | b64enc }}
key: {{ $cert.Key | b64enc }}
ca: {{ $ca.Cert | b64enc }}
{{- end -}}
{{- end }}
