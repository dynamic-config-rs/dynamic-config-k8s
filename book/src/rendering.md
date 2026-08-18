# Rendering

The agent does one resolution, through the same engine every binding
uses, and writes the *resolved* document — so the file on disk is what
an in-process consumer would have computed, not a second dialect.

The output format follows `--out`'s extension: `.json`, `.toml`,
`.yaml`, `.ini`, `.properties`.

**The flat formats are legal here and refused by the engine's `save` —
both on purpose.** `save`'s contract is a typed round trip, which a
string-widening format cannot keep. A rendered file for a consumer is a
different contract, and the agent owns it, stated:

- Nested tables become dotted keys (properties) or sections (INI).
- A string that would widen on the way back in — `"1.10"`, `"true"` —
  is double-quoted in INI, so the round trip through the engine's own
  parser answers the same document. There is a test that holds exactly
  this.
- **Arrays are refused, by path.** Neither format has them; inventing
  an encoding would be a dialect of one. Render to json/toml/yaml when
  the document has lists.

Writes are write-then-rename, so a watching application sees whole
files — the same courtesy an atomic-save editor pays, and the reason
the engine's own watcher tolerates a 25ms grace.

On a fetch failure the sidecar keeps the last rendered file and says so
in its log — keep-last-good, the organisation's standing behaviour. An
init run with nothing yet rendered fails instead, which fails the pod,
which is what an init container is for.

## Templates

Without one, the agent renders the resolved document **verbatim** —
same keys, same shapes, only the format changes with the extension.
That is the right default and it stays the default.

A template takes over when verbatim cannot serve: an application that
wants `DATABASE_URL=postgres://…` assembled from three keys, a
framework with its own nesting, a file with a header. The template owns
the output bytes, which also frees the extension — `.env` and `.conf`
become legal exactly there.

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: billing-template
data:
  template: |
    DATABASE_URL=postgres://{{ db.user }}@{{ db.host }}:{{ db.port }}/billing
    BETA={{ flags.beta }}
```

```yaml
    dynamic-config.rs/path: "/config/app.env"
    dynamic-config.rs/template-configmap: "billing-template"
    # or, for one-liners:
    dynamic-config.rs/template: "db={{ db.host }}:{{ db.port }}"
```

The syntax is [minijinja](https://docs.rs/minijinja)'s — Jinja2:
`{{ value }}`, `{% for %}`, `{% if %}`, filters. The template's context
is the **resolved document**, the same value every binding reads, so a
template cannot see anything the application could not.

The semantics that matter in production:

- **Undefined is strict.** `{{ db.hots }}` is a render *error*, not an
  empty string — a typo that silently renders nothing would ship a
  broken file with a clean exit code. At startup the error is fatal and
  lands in the pod's events; during a watch, the running pod keeps its
  last good file, like any fetch failure.
- **Booleans render as `true`/`false`**, not Python's `True` — a
  template writes config files, and every format this agent speaks
  spells them lowercase.
- **The trailing newline survives.** Env files want one; what the
  template author wrote is what lands.
- **The ConfigMap is re-read at every render**, so editing the template
  takes effect on the next tick — no rollout. It is also why the
  template belongs in a ConfigMap: it is code, and it gets reviewed and
  versioned like code.
- `template` and `template-configmap` together are refused at
  admission: one template, one place.
