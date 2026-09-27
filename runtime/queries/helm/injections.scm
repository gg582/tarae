; tarae: does not inherit gotmpl — gotmpl injects the text between templates as HTML (for Go's html/template),
; and if inherited that earlier rule beats YAML. Helm charts are YAML.

((comment) @injection.content
 (#set! injection.language "comment"))

((text) @injection.content
 (#set! injection.language "yaml")
 (#set! injection.combined))
