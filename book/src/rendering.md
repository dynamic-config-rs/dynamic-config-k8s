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

### The filters this agent adds

minijinja's built-ins, plus six that a configuration template needs and
minijinja either does not ship or ships behind a feature this build does
not carry:

| filter | for |
|---|---|
| `b64encode` | a Kubernetes Secret's `data` is base64, so a template writing one has to encode |
| `b64decode` | the other direction; a value that is not base64, or not UTF-8 once decoded, is a render error rather than mojibake |
| `json` | `tojson` is behind a disabled feature, and a template that cannot emit JSON is missing the format half its consumers read |
| `yaml` | the same, for the other half |
| `quote` | a password with a `#` in it ends a line in half the formats here |
| `required` | strict undefined already refuses a *missing* key; this refuses one that is present and empty, which is the shape a missing secret usually arrives in. `{{ db.password \| required("no password in the vault path") }}` names the field instead of writing a blank one |

What is not here, and will not be: a filter that reaches the network, the
filesystem or the environment. The pipeline is fetch → resolve → validate
→ template, and a template that could fetch would make the same input
render differently on two pods.

## Checking the document before it is published

```yaml
    dynamic-config.rs/schema-configmap: "billing-schema"   # or "billing-schema/other-key.json"
```

The resolved document is validated against a JSON Schema before anything
is written. A document that fails is refused, the last good file keeps
serving, and the failure is a log line and a counter — the same shape as
any other render failure.

The bindings already validate: a Rust, Python or Node application gets a
typed refusal from the engine. This is the door for everyone else — the
Java service reading a `.properties` file, the daemon reading YAML — for
whom a `port: "abc"` would otherwise be discovered at startup, one
restart after the bad document was published.

The schema is re-read every render, like a template, so tightening it does
not need a rollout.

## Several files, one fetch

```yaml
    dynamic-config.rs/path: "/config/app.yaml"
    dynamic-config.rs/also.db: "/config/db.env"
    dynamic-config.rs/also-section.db: "database"
```

More files cut from the **same fetched document**, published all or none:
one fetch, one generation, and a failure in the third does not leave the
first two on disk. Each file is still its own atomic rename — a reader can
catch the gap between two of them, which is a rename apart rather than a
fetch apart.

Several *stores* cannot share a generation. Two stores have no common
instant, and no protocol either of them speaks can say "these two reads
are the same version", so a second store is a
[named render](annotations.md#several-documents-one-pod) with a generation
of its own.

## What the application is running

```yaml
    dynamic-config.rs/meta: "true"
```

Writes a sibling file — `/config/app.yaml` gets `/config/.app.yaml.meta` —
holding the SHA-256 of the rendered bytes, the store's own revision, and
when the render landed. Same atomic rename, same mode.

It answers a question an application cannot otherwise ask about itself:
*which configuration am I running?* Two pods holding the same file is a
claim nobody can check from inside either of them; two pods printing the
same digest is one anybody can. It describes the render and never contains
it — no values reach it, ever.
