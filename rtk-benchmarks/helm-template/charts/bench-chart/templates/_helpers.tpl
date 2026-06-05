{{/*
Deliberately expensive helper: chains sha256sum `rounds` times so a single
chart render burns real CPU in helm's template engine. Deterministic for a
given seed, so repeated renders with identical inputs produce identical output
(which is what lets rtk's cache collapse them).
*/}}
{{- define "bench.heavyHash" -}}
{{- $acc := .seed -}}
{{- range $i := until (int .rounds) -}}
{{- $acc = sha256sum (printf "%s-%d-%s" $acc $i $acc) -}}
{{- end -}}
{{- $acc -}}
{{- end -}}

{{/*
Render a block of N hashed lines, used to inflate ConfigMap payloads.
*/}}
{{- define "bench.payload" -}}
{{- $seed := .seed -}}
{{- range $j := until (int .lines) }}
line-{{ $j }}: {{ sha256sum (printf "%s-%d" $seed $j) }}
{{- end }}
{{- end -}}
