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

{{/*
The installation ConfigMap's name, in one place.

It is mounted by the webhook and created beside it, and those were two
expressions that agreed by coincidence until one of them did not: the
ConfigMap was named after the *release* and the mount after the *webhook*,
so the moment anything put a map in `perStore` the volume named a
ConfigMap nobody had created. A pod stuck in CreateContainerConfigError
takes the webhook down, and with `failurePolicy: Ignore` the API server
then admits every pod uninjected — which is the failure a gate is supposed
to prevent, arrived at by way of the gate itself.
*/}}
{{- define "dynamic-config.installation.fullname" -}}
{{- printf "%s-installation" (include "dynamic-config.webhook.fullname" .) -}}
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

{{/*
The installation document, or nothing.

Every setting an installation makes reaches the webhook as a string,
because that is what an environment variable is — and several of those
strings are little grammars. A values file has YAML already, so these
may be written as maps instead, and the map travels to the pod as a
document rather than being flattened into a grammar here.

Emits nothing when every one of them is a string (or unset), which is
what the callers test for: no map, no ConfigMap, no mount, no variable.
*/}}
{{- define "dynamic-config.installationDocument" -}}
{{- $document := dict -}}
{{- if and .Values.agent.defaults.perStore (not (kindIs "string" .Values.agent.defaults.perStore)) -}}
{{- $stores := dict -}}
{{- range $store, $knobs := .Values.agent.defaults.perStore -}}
{{/* Every entry, whichever way it was written: a store left out here
     would travel as a variable instead, and a variable replaces this
     whole document rather than merging with it. */}}
{{- $_ := set $stores $store $knobs -}}
{{- end -}}
{{- if $stores -}}
{{- $_ := set $document "storeDefaults" $stores -}}
{{- end -}}
{{- end -}}
{{- range $key := list "agentEnvAllow" "sourceAllow" "sourceDeny" -}}
{{- $value := index $.Values.webhook $key -}}
{{- if and $value (not (kindIs "string" $value)) -}}
{{- $_ := set $document $key $value -}}
{{- end -}}
{{- end -}}
{{- if $document -}}
{{- toYaml $document -}}
{{- end -}}
{{- end -}}
