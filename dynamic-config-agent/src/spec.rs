//! What the agent was asked to do — flags in, one validated `Spec` out.
//!
//! Hand-rolled rather than clap: the webhook writes these flags into pod
//! specs, so the surface is part of the *annotation* contract and must
//! stay boring. Everything secret travels in environment variables, never
//! in flags — `kubectl describe pod` prints arguments to anyone with read
//! access, and environment variables only to those who can read the pod
//! spec's secrets too.

use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
usage: dynamic-config-agent
           --source <consul|vault|config-server|firestore|git|redis|etcd|nats|s3>
           --endpoint <address> --key <path> --out <file>
           [--watch <seconds>] [--section <name>] [--file-mode <octal>]
           [--token <bearer>]
           [--auth <method>] [--auth-mount <mount>] [--auth-role <role>]
           [--auth-username <user>] [--auth-token-path <file>]
           [--namespace <vault-namespace>] [--ref <git-ref>]
           [--ssh-key <file>] [--api-url <url>]
           [--template <file> | --template-inline <text>]
           [--ca <file>] [--tls-cert <file> --tls-key <file>]

  --source      which store speaks at the other end
  --endpoint    the store's address: a url for consul, vault,
                config-server, git, etcd (comma-separated for several)
                and nats; <project>[/<database>] for firestore; the
                BUCKET for s3; a redis:// or rediss:// url for redis (use
                DYNAMIC_CONFIG_AGENT_ENDPOINT when the url carries a
                password)
  --key         the document's key/path in the store; for vault,
                <mount>/<path>; for config-server,
                <application>/<profile>; for git, the file's path in
                the repository; for nats, <bucket>/<key>; for s3, the
                object key
  --out         where the rendered document lands; the extension picks
                the output format (.json .toml .yaml .ini .properties) —
                unless a template is given, which then owns the bytes
                and frees the extension
  --watch       watch the store; absent = one shot (init mode). The
                number is the poll interval for a store that must be
                asked, and the resync interval for one that pushes
  --section     the section key the document nests under (default: the
                whole document)
  --file-mode   the rendered file's permissions, octal (e.g. 0640);
                default: the umask's answer, typically 0644
  --metrics-addr  serve Prometheus text (renders, failures, last render
                timestamp) on this address, e.g. 0.0.0.0:9090

  --auth        how to authenticate; the methods each store takes:
                  consul     token | kubernetes | jwt
                  etcd       (no --auth) --tls-cert/--tls-key client
                             certificates, and/or --auth-username +
                             DYNAMIC_CONFIG_AGENT_PASSWORD
                  nats       (no --auth) a .creds file via
                             --auth-token-path, or a token
                  s3         (no --auth) the ambient AWS chain (IRSA)
                  vault      token | kubernetes | approle | jwt |
                             userpass | ldap | cert
                  firestore  metadata-server | access-token | emulator
                  git        anonymous | token | ssh | ssh-key
                config-server takes a bearer token — or kubernetes:
                  the pod's projected SA token, reviewed by the server;
                redis reads credentials from its url
  --auth-mount  vault: the auth method's mount path when it is not the
                default; consul: the auth method's NAME (required for
                kubernetes and jwt)
  --auth-role   vault kubernetes: the role to assume (required);
                vault approle: the role id; vault jwt/cert: optional
  --auth-username  vault userpass/ldap: the user; git: the basic-auth
                user when the host is not token-shaped
  --auth-token-path  where the service-account token is mounted, when
                it is not the conventional path
  --namespace   the Vault namespace (Vault Enterprise)
  --ref         git: what to read — <branch>, branch:<name>,
                tag:<name> or commit:<sha> (default: branch main)
  --ssh-key     git ssh-key auth: the private key file
  --api-url     firestore: the API endpoint when it is not Google's —
                the emulator, a private endpoint
  --template    a minijinja template file; the resolved document is its
                context ({{ db.host }}), its output is the file, re-read
                at every render so a mounted ConfigMap edit takes
  --template-inline
                the same, given directly — for one-liners
  --ca          a private certificate authority, PEM
  --tls-cert / --tls-key
                a client certificate and its key, PEM, together

  --token       bearer/store token; prefer DYNAMIC_CONFIG_AGENT_TOKEN,
                which does not appear in `kubectl describe pod`

environment:
  DYNAMIC_CONFIG_AGENT_TOKEN     the bearer/access token
  DYNAMIC_CONFIG_AGENT_PASSWORD  the second secret, where a method has
                one: the approle secret id, the userpass/ldap password
                (never a flag, on purpose)
  DYNAMIC_CONFIG_AGENT_ENDPOINT  the endpoint, when it must not appear
                in the pod spec (a redis url with a password in it)
  --notify-http <url>
                POST here after every render — `http://127.0.0.1:8080/reload`.
                Localhost only. One attempt, a short deadline, never fatal:
                the file is already correct when this is sent
  --on-drift    warn (default) | repair | fail, for a rendered file
                something else in the pod has written to
  --history N   keep the last N replaced generations beside each render,
                under a hidden directory, so an incident can ask what the
                file was before. Off by default; these live on the
                rendered volume, which is the pod's memory
  --require-ack the readiness probe stays 503 until the application POSTs
                the fingerprint it is running to /applied on the metrics
                port. Needs the application's cooperation, so it is off
  --canary <file>
                a mounted file holding a percentage. This pod publishes a
                new document only if its own bucket falls under it, so a
                change reaches part of the fleet first. Widening the number
                needs no restart
  --timeout <seconds>
                the deadline for one fetch attempt, where the store has a
                door for it. Ten seconds everywhere by default, which is
                right for a store on this network and wrong for a Git
                remote across a WAN
  --events      write Kubernetes Events on this pod when a render fails or
                the document goes missing, so `kubectl describe pod` shows
                them. Needs a service account with `create` on events; off
                unless the chart granted it and the pod asked
  --revoke-grace <seconds>
                how long to spend handing a lease back on shutdown; five
                by default, and the webhook refuses a value past the pod's
                terminationGracePeriodSeconds
  --tls-server-name <name>
                the name the store's certificate must carry, when the
                endpoint is an address it does not name — an IP, usually
  --tls-skip-verify
                connect without authenticating the store AT ALL. Anything
                on the path can then read the configuration and rewrite it.
                Try --ca first: trusting one more certificate keeps the
                server authenticated
  --no-tls-reload
                do NOT rebuild the store's client when --ca, --tls-cert,
                --tls-key or --ssh-key changes on disk. On by default: the
                kubelet rewrites a mounted Secret in place, and a client
                built once against the old material stops working at a
                moment unrelated to the rotation
";

/// What to do when the very first fetch fails.
///
/// The rendered volume is an `emptyDir`, which survives a *container*
/// restart and dies with the pod — so a sidecar that has already written
/// the file once and is coming back finds it there. That file is the
/// cheapest correct cache there is, and reading it back is what turns a
/// store outage from "this pod cannot start" into "this pod starts on what
/// it had".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartupPolicy {
    /// Serve the file already on disk when the first fetch fails.
    ///
    /// The default. Loud about it — the document is stale and nothing here
    /// knows how stale — but a pod that starts on last week's configuration
    /// is usually better than a pod that does not start at all, and the
    /// staleness gauge is how an operator finds out either way.
    #[default]
    AllowCached,
    /// Fail instead. For a pod that must never start on a credential that
    /// may have been rotated out from under it.
    RequireFresh,
    /// Start regardless, with whatever is there — including nothing.
    ///
    /// The foot-gun, named so that choosing it is deliberate: the
    /// application gets an empty file rather than a configuration.
    BestEffort,
}

impl StartupPolicy {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "allow-cached" => Ok(Self::AllowCached),
            "require-fresh" => Ok(Self::RequireFresh),
            "best-effort" => Ok(Self::BestEffort),
            other => Err(format!(
                "--startup-policy {other:?}: allow-cached, require-fresh or best-effort"
            )),
        }
    }
}

/// What to do when the document stops being there.
///
/// A store answering "that path holds nothing" is a different fact from a
/// store not answering, and the stores tell them apart now — but only the
/// caller can decide what it means. For a feature flag, keeping the last
/// value is obviously right. For a credential that was deliberately
/// revoked, keeping it is the opposite of right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnDelete {
    /// Keep serving the last good document, and say so.
    ///
    /// The default, because it is the one that cannot lose an application
    /// its configuration — and because a delete is very often a mistake
    /// somebody is about to undo.
    #[default]
    Retain,
    /// Truncate the file.
    ///
    /// For a credential whose disappearance is meant to take effect: an
    /// application that reads an empty file and fails is better than one
    /// that goes on using a secret somebody revoked.
    Remove,
    /// End the agent.
    ///
    /// The pod restarts, and — under `require-fresh` — does not come back
    /// until the document does.
    Fail,
}

impl OnDelete {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "retain" => Ok(Self::Retain),
            "remove" => Ok(Self::Remove),
            "fail" => Ok(Self::Fail),
            other => Err(format!("--on-delete {other:?}: retain, remove or fail")),
        }
    }
}

/// The most generations `--history` will keep.
///
/// The rendered volume is `medium: Memory` by default, so every kept
/// generation is charged to the pod's memory limit — which is 64Mi by
/// default. Ten is enough to answer "what was it before?" through a bad
/// afternoon and small enough that nobody has to do the arithmetic.
pub const MOST_HISTORY: usize = 10;

/// What to do when the rendered file stops being what was rendered.
///
/// The agent owns the file; the volume is shared. Something else in the pod
/// can write to it — a debug session, an init container with an opinion, an
/// application that rewrites its own configuration — and until the next
/// change arrives from the store, nothing notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnDrift {
    /// Say so, and leave it.
    ///
    /// The default, because the agent cannot know whether the write was a
    /// mistake or the point. What it can do is stop the difference being
    /// invisible.
    #[default]
    Warn,
    /// Write the rendered document back over it.
    ///
    /// For a file whose contents are the store's and nobody else's.
    Repair,
    /// End the agent.
    ///
    /// The pod restarts, and the file is rendered again from the store —
    /// which is repair by a longer route, and also a restart somebody will
    /// see.
    Fail,
}

impl OnDrift {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "warn" => Ok(Self::Warn),
            "repair" => Ok(Self::Repair),
            "fail" => Ok(Self::Fail),
            other => Err(format!("--on-drift {other:?}: warn, repair or fail")),
        }
    }
}

/// One more file cut from the same fetched document.
///
/// **Not a second source.** Two stores have no common instant, so "these
/// files are one generation" is not something either of them can promise —
/// cross-store transactionality is not a feature that was skipped, it is
/// one that does not exist. What is achievable, and what this is, is
/// several *renderings* of one document: a different section, a different
/// format, a different template, written together or not at all.
#[derive(Debug, Clone)]
pub struct Rendering {
    /// Where this one goes. Its extension picks the format, exactly as
    /// `--out` does.
    pub out: PathBuf,
    /// The section of the document to cut, or the whole of it.
    pub section: Option<String>,
    /// Permissions, defaulting to the main render's.
    pub file_mode: Option<u32>,
}

impl Rendering {
    /// `out=/config/db.env,section=db,mode=0600`.
    ///
    /// A single argument rather than three positional ones, because three
    /// flags that have to arrive in step are three flags somebody gets out
    /// of step.
    fn parse(text: &str, default_mode: Option<u32>) -> Result<Self, String> {
        let mut out = None;
        let mut section = None;
        let mut file_mode = default_mode;

        for field in text.split(',') {
            let (name, value) = field.split_once('=').ok_or_else(|| {
                format!("--also {text:?}: each field is `name=value`, comma-separated")
            })?;

            match name.trim() {
                "out" => out = Some(PathBuf::from(value)),
                "section" => section = Some(value.to_owned()),
                "mode" => {
                    let octal = value.strip_prefix("0o").unwrap_or(value);

                    file_mode = Some(
                        u32::from_str_radix(octal, 8)
                            .map_err(|_| format!("--also mode={value:?}: octal, like 0640"))?,
                    );
                }
                other => {
                    return Err(format!(
                        "--also {other:?}: the fields are out, section and mode"
                    ))
                }
            }
        }

        let out = out.ok_or_else(|| format!("--also {text:?}: `out` is required"))?;

        // The same rule the main render's `--out` follows, checked here so
        // a bad extension is a startup failure rather than a render one.
        crate::render::OutputFormat::of(&out)
            .ok_or_else(|| format!("--also out={}: unknown format", out.display()))?;

        Ok(Self {
            out,
            section,
            file_mode,
        })
    }
}

/// A credential this agent uses to *reach* a store.
///
/// Wiped when it goes. Deliberately not the resolved document — that has
/// to be plaintext to be written to a file, and pretending otherwise would
/// be a security claim this cannot keep. What it does keep is narrower and
/// true: the token, the password and the keys that open a connection do not
/// stay in this process's memory after the connection is made.
pub type Credential = zeroize::Zeroizing<String>;

pub struct Spec {
    pub source: String,
    pub endpoint: String,
    pub key: String,
    pub out: PathBuf,
    pub watch: Option<Duration>,
    /// `--file-mode`: the rendered file's permissions, octal. `None`
    /// leaves the process umask's answer (0644, typically).
    pub file_mode: Option<u32>,
    /// `--metrics-addr`: a Prometheus text endpoint, opt-in.
    pub metrics_addr: Option<String>,
    /// `--startup-policy`: what a failed first fetch means.
    pub startup_policy: StartupPolicy,
    /// `--max-document-bytes`: how large a fetched document may be.
    ///
    /// The engine's ceiling unless this says otherwise. It guards against
    /// the accident — a key pointed at the wrong object — against a
    /// container that has 64Mi and holds the document several times over
    /// between fetching, parsing and rendering it.
    pub max_document_bytes: Option<usize>,
    /// `--on-delete`: what a document that is no longer there means.
    pub on_delete: OnDelete,
    /// `--also`: further files cut from the same fetched document.
    ///
    /// Published **all or none**: if any of them fails to resolve, render
    /// or validate, none is written and every last good file stays. That
    /// is the property — an application reading two of these never sees
    /// one from before a change and one from after it because the second
    /// failed.
    pub also: Vec<Rendering>,
    /// `--schema`: a JSON Schema the resolved document must satisfy.
    ///
    /// A mounted file. Checked before the template and before the write, so
    /// a document that does not satisfy it never reaches the application
    /// and the last good file goes on serving.
    pub schema: Option<PathBuf>,
    /// `--meta`: write a sibling `.<name>.meta` beside the document.
    ///
    /// What was rendered, never the rendering: a digest, the store's
    /// revision, and a clock.
    pub meta: bool,
    /// `--max-staleness`: how old a document may be before this agent
    /// stops reporting ready. `None` is no ceiling.
    pub max_staleness: Option<Duration>,
    /// `--dynamic`: read a dynamic-secret engine rather than KV.
    ///
    /// Vault only, for now. It changes what a read *is*: every fetch mints
    /// a credential with a lease, which somebody then has to renew and hand
    /// back.
    pub dynamic: bool,
    /// `--no-revoke-on-shutdown`: keep the lease when the pod stops.
    ///
    /// Revoking is the default — a credential outliving the pod that holds
    /// it is a window nobody needs — and this is the opt-out for a lease
    /// something else is still using.
    pub revoke_on_shutdown: bool,
    /// `--revoke-grace`: how long to spend handing the lease back.
    ///
    /// Kubernetes is already counting down to SIGKILL when this runs, so
    /// the number that matters is not this one alone but this one against
    /// `terminationGracePeriodSeconds` — which is why the webhook refuses a
    /// value past it rather than letting the two disagree.
    pub revoke_grace: Duration,
    /// `--notify-http`: a localhost endpoint to POST to after a render.
    pub notify_http: Option<crate::notify::Endpoint>,
    /// `--on-drift`: what to do when something else writes to the rendered
    /// file.
    pub on_drift: OnDrift,
    /// Whether to rebuild the store's client when its trust material
    /// changes on disk.
    ///
    /// On, and off only through `--no-tls-reload`. The kubelet updates a
    /// mounted ConfigMap or Secret in place, and every client here reads
    /// that material once — so without this a rotated CA is a pod restart,
    /// and a rotation nobody restarted for is a store that stops answering
    /// at a moment unrelated to the rotation.
    pub tls_reload: bool,
    /// `--history N`: how many replaced generations to keep beside each
    /// render. Zero, and off, unless a pod asks.
    pub history: usize,
    /// `--require-ack`: `/readyz` stays 503 until the application says it
    /// is running what was published.
    pub require_ack: bool,
    /// `--events`: write Kubernetes Events on this pod for failures.
    ///
    /// Off, and twice opt-in — the chart grants the RBAC and the pod asks —
    /// because it puts an API credential beside every application that
    /// turns it on, which the sidecar has never carried.
    pub events: bool,
    /// `--canary <file>`: a mounted file holding the cohort percentage.
    ///
    /// A pod outside the cohort keeps serving what it has until the number
    /// grows past its bucket — which happens by editing a ConfigMap, with
    /// no pod restart, because a restart would discard the state the canary
    /// exists to watch.
    pub canary: Option<PathBuf>,
    /// `--timeout <seconds>`: the deadline for one fetch attempt.
    ///
    /// `None` leaves the store's own default, which is ten seconds
    /// everywhere. What this is for is the store that is slow on purpose —
    /// a Git remote across a WAN, a bucket in another region — where the
    /// default is a fetch that keeps timing out and no way to say so.
    pub timeout: Option<Duration>,
    /// Read through [`Spec::token`]; see [`Credential`].
    pub token: Option<Credential>,
    pub section: Option<String>,
    pub auth: Option<String>,
    pub auth_mount: Option<String>,
    pub auth_role: Option<String>,
    pub auth_username: Option<String>,
    pub auth_token_path: Option<String>,
    pub namespace: Option<String>,
    pub reference: Option<String>,
    pub ssh_key: Option<PathBuf>,
    pub api_url: Option<String>,
    pub template: Option<PathBuf>,
    pub template_inline: Option<String>,
    pub ca: Option<PathBuf>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// `--tls-server-name`: the name the certificate must carry, when that
    /// is not the one in `--endpoint`.
    pub tls_server_name: Option<String>,
    /// `--tls-skip-verify`: connect without authenticating the server.
    ///
    /// Held as a plain flag rather than folded into the TLS material,
    /// because the log line that announces it on every fetch reads this and
    /// nothing else.
    pub tls_skip_verify: bool,
    /// `DYNAMIC_CONFIG_AGENT_PASSWORD`, and no flag: a password in a pod
    /// spec's `args` is a password in `kubectl describe pod`.
    ///
    /// Read through [`Spec::password`]; see [`Credential`].
    pub password: Option<Credential>,
}

// Hand-written, never derived: a derive prints every field, and two of
// them are credentials. `{:?}` reaching a log is an ordinary accident — a
// `dbg!`, a `tracing::debug!(?spec)` — and an accident must not disclose a
// secret. The store crates follow the same rule for the same reason.
impl std::fmt::Debug for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spec")
            .field("source", &self.source)
            .field("endpoint", &self.endpoint)
            .field("key", &self.key)
            .field("out", &self.out)
            .field("watch", &self.watch)
            .field("auth", &self.auth)
            .field("token", &self.token.as_ref().map(|_| "***"))
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

impl Spec {
    /// The token, as the stores want it.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.token.as_ref().map(|token| token.as_str())
    }

    /// The password, as the stores want it.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        self.password.as_ref().map(|password| password.as_str())
    }
}

impl Spec {
    pub fn from_args(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut source = None;
        let mut endpoint = std::env::var("DYNAMIC_CONFIG_AGENT_ENDPOINT").ok();
        let mut key = None;
        let mut out = None;
        let mut watch = None;
        let mut token = std::env::var("DYNAMIC_CONFIG_AGENT_TOKEN")
            .ok()
            .map(Credential::new);
        let mut section = None;
        let mut file_mode = None;
        let mut metrics_addr = None;
        let mut startup_policy = StartupPolicy::default();
        let mut max_document_bytes = None;
        let mut on_delete = OnDelete::default();
        let mut also: Vec<String> = Vec::new();
        let mut schema = None;
        let mut meta = false;
        let mut max_staleness = None;
        let mut dynamic = false;
        let mut revoke_on_shutdown = true;
        let mut revoke_grace = crate::sidecar::REVOKE_DEADLINE;
        let mut notify_http = None;
        let mut on_drift = OnDrift::default();
        let mut tls_reload = true;
        let mut history = 0;
        let mut require_ack = false;
        let mut events = false;
        let mut canary = None;
        let mut timeout = None;
        let mut auth = None;
        let mut auth_mount = None;
        let mut auth_role = None;
        let mut auth_username = None;
        let mut auth_token_path = None;
        let mut namespace = None;
        let mut reference = None;
        let mut ssh_key = None;
        let mut api_url = None;
        let mut template = None;
        let mut template_inline = None;
        let mut ca = None;
        let mut tls_cert = None;
        let mut tls_key = None;
        let mut tls_server_name = None;
        let mut tls_skip_verify = false;

        let mut arguments = arguments.peekable();

        while let Some(flag) = arguments.next() {
            let mut value = |name: &str| {
                arguments
                    .next()
                    .ok_or_else(|| format!("{name} needs a value"))
            };

            match flag.as_str() {
                "--source" => source = Some(value("--source")?),
                "--endpoint" => endpoint = Some(value("--endpoint")?),
                "--key" => key = Some(value("--key")?),
                "--out" => out = Some(PathBuf::from(value("--out")?)),
                "--watch" => {
                    let seconds: u64 = value("--watch")?
                        .parse()
                        .map_err(|_| "--watch takes whole seconds".to_string())?;
                    watch = Some(Duration::from_secs(seconds.max(1)));
                }
                "--token" => token = Some(Credential::new(value("--token")?)),
                "--section" => section = Some(value("--section")?),
                "--metrics-addr" => metrics_addr = Some(value("--metrics-addr")?),
                "--startup-policy" => {
                    startup_policy = StartupPolicy::parse(&value("--startup-policy")?)?;
                }
                "--max-document-bytes" => {
                    max_document_bytes = Some(
                        value("--max-document-bytes")?
                            .parse()
                            .map_err(|_| "--max-document-bytes takes whole bytes".to_string())?,
                    );
                }
                "--on-delete" => on_delete = OnDelete::parse(&value("--on-delete")?)?,
                "--on-drift" => on_drift = OnDrift::parse(&value("--on-drift")?)?,
                "--no-tls-reload" => tls_reload = false,
                "--require-ack" => require_ack = true,
                "--events" => events = true,
                "--canary" => canary = Some(PathBuf::from(value("--canary")?)),
                "--timeout" => {
                    let seconds: u64 = value("--timeout")?
                        .parse()
                        .map_err(|_| "--timeout takes whole seconds".to_string())?;

                    if seconds == 0 {
                        return Err("--timeout 0 is no deadline at all; leave the flag off \
                                    to keep the store's own"
                            .to_owned());
                    }

                    timeout = Some(Duration::from_secs(seconds));
                }
                "--history" => {
                    history = value("--history")?
                        .parse()
                        .map_err(|_| "--history takes a count".to_string())?;

                    if history > MOST_HISTORY {
                        return Err(format!(
                            "--history {history}: at most {MOST_HISTORY}. These live on the \
                             rendered volume, which is the pod's memory by default"
                        ));
                    }
                }
                "--notify-http" => {
                    notify_http = Some(
                        crate::notify::Endpoint::parse(&value("--notify-http")?)
                            .map_err(|error| format!("--notify-http {error}"))?,
                    );
                }
                "--also" => also.push(value("--also")?),
                "--schema" => schema = Some(PathBuf::from(value("--schema")?)),
                "--meta" => meta = true,
                "--max-staleness" => {
                    let seconds: u64 = value("--max-staleness")?
                        .parse()
                        .map_err(|_| "--max-staleness takes whole seconds".to_string())?;

                    if seconds == 0 {
                        return Err(
                            "--max-staleness 0 is no ceiling; leave the flag off".to_owned()
                        );
                    }

                    max_staleness = Some(Duration::from_secs(seconds));
                }
                "--dynamic" => dynamic = true,
                "--no-revoke-on-shutdown" => revoke_on_shutdown = false,
                "--revoke-grace" => {
                    let seconds: u64 = value("--revoke-grace")?
                        .parse()
                        .map_err(|_| "--revoke-grace takes whole seconds".to_string())?;

                    if seconds == 0 {
                        return Err("--revoke-grace 0 revokes nothing; use \
                                    --no-revoke-on-shutdown to mean that"
                            .to_owned());
                    }

                    revoke_grace = Duration::from_secs(seconds);
                }
                "--file-mode" => {
                    let text = value("--file-mode")?;
                    let octal = text.strip_prefix("0o").unwrap_or(&text);
                    let mode = u32::from_str_radix(octal, 8)
                        .map_err(|_| format!("--file-mode {text:?}: octal, like 0640"))?;

                    if mode > 0o777 {
                        return Err(format!("--file-mode {text:?}: at most 0777"));
                    }

                    if mode & 0o400 == 0 {
                        return Err(format!(
                            "--file-mode {text:?}: the owner must at least read it"
                        ));
                    }

                    file_mode = Some(mode);
                }
                "--auth" => auth = Some(value("--auth")?),
                "--auth-mount" => auth_mount = Some(value("--auth-mount")?),
                "--auth-role" => auth_role = Some(value("--auth-role")?),
                "--auth-username" => auth_username = Some(value("--auth-username")?),
                "--auth-token-path" => auth_token_path = Some(value("--auth-token-path")?),
                "--namespace" => namespace = Some(value("--namespace")?),
                "--ref" => reference = Some(value("--ref")?),
                "--ssh-key" => ssh_key = Some(PathBuf::from(value("--ssh-key")?)),
                "--api-url" => api_url = Some(value("--api-url")?),
                "--template" => template = Some(PathBuf::from(value("--template")?)),
                "--template-inline" => template_inline = Some(value("--template-inline")?),
                "--ca" => ca = Some(PathBuf::from(value("--ca")?)),
                "--tls-server-name" => tls_server_name = Some(value("--tls-server-name")?),
                "--tls-skip-verify" => tls_skip_verify = true,
                "--tls-cert" => tls_cert = Some(PathBuf::from(value("--tls-cert")?)),
                "--tls-key" => tls_key = Some(PathBuf::from(value("--tls-key")?)),
                "--one-shot" => watch = None,
                other => return Err(format!("unknown flag {other:?}")),
            }
        }

        let spec = Self {
            source: source.ok_or("--source is required")?,
            endpoint: endpoint
                .ok_or("--endpoint is required (or DYNAMIC_CONFIG_AGENT_ENDPOINT)")?,
            key: key.ok_or("--key is required")?,
            out: out.ok_or("--out is required")?,
            watch,
            token,
            section,
            file_mode,
            metrics_addr,
            startup_policy,
            max_document_bytes,
            on_delete,
            // Parsed after `file_mode` is settled, so an `--also` without a
            // `mode` inherits the main render's rather than the umask's.
            also: also
                .iter()
                .map(|text| Rendering::parse(text, file_mode))
                .collect::<Result<Vec<_>, _>>()?,
            schema,
            meta,
            max_staleness,
            dynamic,
            revoke_on_shutdown,
            revoke_grace,
            notify_http,
            on_drift,
            tls_reload,
            history,
            require_ack,
            events,
            canary,
            timeout,
            auth,
            auth_mount,
            auth_role,
            auth_username,
            auth_token_path,
            namespace,
            reference,
            ssh_key,
            api_url,
            template,
            template_inline,
            ca,
            tls_cert,
            tls_key,
            tls_server_name,
            tls_skip_verify,
            password: std::env::var("DYNAMIC_CONFIG_AGENT_PASSWORD")
                .ok()
                .map(Credential::new),
        };

        spec.validated()
    }

    /// Everything that can be refused before a byte leaves the pod.
    ///
    /// The webhook writes these flags sight unseen, so the agent is where
    /// a wrong combination gets its sentence — at startup, in the pod's
    /// events, not as a store error twenty minutes later.
    fn validated(self) -> Result<Self, String> {
        match self.source.as_str() {
            "consul" | "vault" | "config-server" | "firestore" | "git" | "redis" | "etcd"
            | "nats" | "s3" => {}
            other => {
                return Err(format!(
                    "--source {other:?}: one of consul, vault, config-server, \
                     firestore, git, redis, etcd, nats, s3"
                ))
            }
        }

        if self.tls_cert.is_some() != self.tls_key.is_some() {
            return Err("--tls-cert and --tls-key come together: a certificate \
                 without its key (or the reverse) can prove nothing"
                .to_owned());
        }

        if self.reference.is_some() && self.source != "git" {
            return Err(format!("--ref is git's flag; --source is {}", self.source));
        }

        if self.ssh_key.is_some() && self.source != "git" {
            return Err(format!(
                "--ssh-key is git's flag; --source is {}",
                self.source
            ));
        }

        if self.api_url.is_some() && !matches!(self.source.as_str(), "firestore" | "s3") {
            return Err(format!(
                "--api-url is firestore's and s3's flag; --source is {}",
                self.source
            ));
        }

        if self.namespace.is_some() && self.source != "vault" {
            return Err(format!(
                "--namespace is vault's flag; --source is {}",
                self.source
            ));
        }

        self.validated_auth()?;

        if self.template.is_some() && self.template_inline.is_some() {
            return Err(
                "--template and --template-inline are both set: one template, one place".to_owned(),
            );
        }

        // A template owns the output bytes, so the extension stops
        // meaning anything — `.env` and `.conf` become legal exactly
        // there.
        let templated = self.template.is_some() || self.template_inline.is_some();

        if !templated && crate::render::OutputFormat::of(&self.out).is_none() {
            return Err(format!(
 "--out {:?}: the extension must be one of .json .toml .yaml .ini .properties                  (or give a --template, which owns the bytes)",
 self.out
 ));
        }

        Ok(self)
    }

    fn validated_auth(&self) -> Result<(), String> {
        let auth = self.auth.as_deref();

        let token_needed = |method: &str| {
            self.token.as_deref().map(|_| ()).ok_or(format!(
                "--auth {method} needs a token: set DYNAMIC_CONFIG_AGENT_TOKEN \
                 or pass --token"
            ))
        };
        let password_needed = |method: &str, what: &str| {
            self.password.as_deref().map(|_| ()).ok_or(format!(
                "--auth {method} needs {what}: set DYNAMIC_CONFIG_AGENT_PASSWORD \
                 (there is no flag, on purpose — flags are visible in \
                 `kubectl describe pod`)"
            ))
        };

        match (self.source.as_str(), auth) {
            ("consul", None) => {} // a token if one is set, anonymous otherwise
            ("consul", Some("token")) => token_needed("token")?,
            ("consul", Some(method @ ("kubernetes" | "jwt"))) => {
                if self.auth_mount.is_none() {
                    return Err(format!(
                        "--auth {method} on consul needs --auth-mount: the auth \
                         method's name, as configured in consul"
                    ));
                }
                if method == "jwt" {
                    token_needed("jwt")?;
                }
            }
            ("consul", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on consul: token, kubernetes or jwt"
                ))
            }

            ("vault", None | Some("token")) => token_needed("token")?,
            ("vault", Some("kubernetes")) => {
                if self.auth_role.is_none() {
                    return Err("--auth kubernetes on vault needs --auth-role: \
                         the vault role to assume"
                        .to_owned());
                }
            }
            ("vault", Some("approle")) => {
                if self.auth_role.is_none() {
                    return Err(
                        "--auth approle needs --auth-role: the role id (the public half)"
                            .to_owned(),
                    );
                }
                password_needed("approle", "the secret id")?;
            }
            ("vault", Some("jwt")) => token_needed("jwt")?,
            ("vault", Some(method @ ("userpass" | "ldap"))) => {
                if self.auth_username.is_none() {
                    return Err(format!("--auth {method} needs --auth-username"));
                }
                password_needed(method, "the password")?;
            }
            ("vault", Some("cert")) => {
                if self.tls_cert.is_none() {
                    return Err("--auth cert needs --tls-cert and --tls-key: the \
                         certificate IS the credential"
                        .to_owned());
                }
            }
            ("vault", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on vault: token, kubernetes, approle, jwt, \
                     userpass, ldap or cert"
                ))
            }

            ("config-server", None) => {} // a token if one is set
            ("config-server", Some("kubernetes")) => {
                // The pod's own projected service-account token as the
                // bearer — the server's [kubernetes] TokenReview auth.
                // Nothing to validate: the default token path exists in
                // every pod, and --auth-token-path overrides it.
            }
            ("config-server", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on config-server: \"kubernetes\" (the pod's \
 projected token, reviewed by the server) or drop --auth and \
 set DYNAMIC_CONFIG_AGENT_TOKEN"
                ))
            }

            ("firestore", None | Some("metadata-server" | "emulator")) => {}
            ("firestore", Some("access-token")) => token_needed("access-token")?,
            ("firestore", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on firestore: metadata-server, access-token \
                     or emulator"
                ))
            }

            ("git", None | Some("anonymous" | "ssh")) => {}
            ("git", Some("token")) => token_needed("token")?,
            ("git", Some("ssh-key")) => {
                if self.ssh_key.is_none() {
                    return Err("--auth ssh-key needs --ssh-key: the private key file".to_owned());
                }
            }
            ("git", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on git: anonymous, token, ssh or ssh-key"
                ))
            }

            // etcd speaks exactly two methods, both first-class: TLS
            // client certificates, and username/password. No --auth
            // selector — the flags present ARE the method, and both
            // present together is etcd's own "cert for the channel,
            // password for the user" combination.
            ("etcd", None) => {
                if self.auth_username.is_some() {
                    password_needed("user", "the password half")?;
                }
            }
            ("etcd", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on etcd: drop --auth — client certificates \
                     ride --tls-cert/--tls-key and a user rides \
                     --auth-username + DYNAMIC_CONFIG_AGENT_PASSWORD"
                ))
            }

            // NATS: a .creds file (the account idiom) via
            // --auth-token-path, or a bare token; anonymous otherwise.
            ("nats", None) => {
                if !self.key.contains('/') {
                    return Err("nats' --key is <bucket>/<key>".to_owned());
                }
            }
            ("nats", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on nats: drop --auth — a .creds file rides \
                     --auth-token-path, a token rides DYNAMIC_CONFIG_AGENT_TOKEN"
                ))
            }

            // S3: the ambient chain — on EKS that is IRSA, the workload's
            // own identity. There is nothing to configure here, which is
            // the point.
            ("s3", None) => {}
            ("s3", Some(other)) => {
                return Err(format!(
                    "--auth {other:?} on s3: drop --auth — credentials come from \
                     the ambient AWS chain (IRSA on EKS)"
                ))
            }

            ("redis", None) => {}
            ("redis", Some(_)) => {
                return Err("redis reads credentials from its url; put them there \
                     (via DYNAMIC_CONFIG_AGENT_ENDPOINT, so the password stays \
                     out of the pod spec) and drop --auth"
                    .to_owned())
            }

            _ => unreachable!("source validated above"),
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> impl Iterator<Item = String> + '_ {
        text.split_whitespace().map(str::to_owned)
    }

    #[test]
    fn a_full_line_parses() {
        let spec = Spec::from_args(args(
            "--source consul --endpoint http://consul:8500 --key app/config.json \
             --out /config/rendered.toml --watch 15",
        ))
        .expect("parses");

        assert_eq!(spec.source, "consul");
        assert_eq!(spec.watch, Some(Duration::from_secs(15)));
    }

    #[test]
    fn a_missing_flag_names_itself() {
        let error =
            Spec::from_args(args("--source consul --endpoint x --key y")).expect_err("refused");

        assert!(error.contains("--out"), "{error}");
    }

    #[test]
    fn an_unknown_source_lists_the_known_ones() {
        let error = Spec::from_args(args(
            "--source zookeeper --endpoint x --key y --out /z.json",
        ))
        .expect_err("refused");

        assert!(error.contains("consul"), "{error}");
        assert!(error.contains("redis"), "{error}");
    }

    #[test]
    fn an_unknown_extension_is_refused_up_front() {
        let error = Spec::from_args(args("--source consul --endpoint x --key y --out /z.conf"))
            .expect_err("refused");

        assert!(error.contains(".properties"), "{error}");
    }

    #[test]
    fn the_async_stores_parse() {
        // The 0.1.0 refusal-by-name retired when the async path landed.
        for line in [
            "--source etcd --endpoint http://etcd:2379 --key app/config.json --out /z.json",
            "--source nats --endpoint nats://nats:4222 --key config/db.json --out /z.json",
            "--source s3 --endpoint myapp-config --key prod/db.json --out /z.json",
        ] {
            Spec::from_args(args(line)).expect("parses");
        }
    }

    #[test]
    fn etcd_a_user_without_a_password_is_refused() {
        let error = Spec::from_args(args(
            "--source etcd --endpoint http://etcd:2379 --key app/config.json \
             --out /z.json --auth-username myapp",
        ))
        .expect_err("refused");

        assert!(error.contains("DYNAMIC_CONFIG_AGENT_PASSWORD"), "{error}");
    }

    #[test]
    fn nats_wants_a_bucket_and_a_key() {
        let error = Spec::from_args(args(
            "--source nats --endpoint nats://nats:4222 --key flat --out /z.json",
        ))
        .expect_err("refused");

        assert!(error.contains("<bucket>/<key>"), "{error}");
    }

    #[test]
    fn vault_kubernetes_wants_a_role() {
        let error = Spec::from_args(args(
            "--source vault --endpoint http://vault:8200 --key secret/app \
             --out /z.json --auth kubernetes",
        ))
        .expect_err("refused");

        assert!(error.contains("--auth-role"), "{error}");
    }

    #[test]
    fn consul_kubernetes_wants_the_method_name() {
        let error = Spec::from_args(args(
            "--source consul --endpoint http://consul:8500 --key app/c.json \
             --out /z.json --auth kubernetes",
        ))
        .expect_err("refused");

        assert!(error.contains("--auth-mount"), "{error}");
    }

    #[test]
    fn a_certificate_without_its_key_is_refused() {
        let error = Spec::from_args(args(
            "--source consul --endpoint x --key y.json --out /z.json --tls-cert /c.pem",
        ))
        .expect_err("refused");

        assert!(error.contains("--tls-key"), "{error}");
    }

    #[test]
    fn a_flag_on_the_wrong_store_names_its_owner() {
        let error = Spec::from_args(args(
            "--source consul --endpoint x --key y.json --out /z.json --ref main",
        ))
        .expect_err("refused");

        assert!(error.contains("git"), "{error}");
    }

    #[test]
    fn an_auth_method_the_store_does_not_take_lists_what_it_does() {
        let error = Spec::from_args(args(
            "--source firestore --endpoint proj --key config/db --out /z.json \
             --auth approle",
        ))
        .expect_err("refused");

        assert!(error.contains("metadata-server"), "{error}");
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    fn spec_with_token(token: &str) -> Spec {
        Spec::from_args(
            [
                "--source",
                "consul",
                "--endpoint",
                "http://127.0.0.1:1",
                "--key",
                "unused",
                "--out",
                "/tmp/rendered.json",
                "--token",
                token,
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("a valid spec")
    }

    /// A derived `Debug` prints every field, and two of them are
    /// credentials. `{:?}` reaching a log is an ordinary accident — a
    /// `dbg!`, a `tracing::debug!(?spec)` — and an accident must not
    /// disclose a secret.
    #[test]
    fn the_debug_never_prints_a_credential() {
        let spec = spec_with_token("hunter2-planted-secret");

        let rendered = format!("{spec:?}");

        assert!(!rendered.contains("hunter2-planted-secret"), "{rendered}");
        assert!(rendered.contains("***"), "{rendered}");

        // And it still says the things a diagnostic is read for.
        assert!(rendered.contains("consul"), "{rendered}");
    }

    /// The accessor is what every store reads through, so the field's type
    /// can be a wiping one without half the call sites knowing.
    #[test]
    fn the_accessors_hand_over_the_value_the_stores_want() {
        let spec = spec_with_token("t0ken");

        assert_eq!(spec.token(), Some("t0ken"));
        assert_eq!(spec.password(), None);
    }

    /// The claim is narrow on purpose: the credentials that *reach* a store
    /// are wiped, and the resolved document is not — it has to be plaintext
    /// to be written to a file, and saying otherwise would be a promise
    /// this cannot keep.
    #[test]
    fn a_credential_is_zeroizing_and_the_document_is_not() {
        let spec = spec_with_token("t0ken");

        // The type is the guarantee: `Zeroizing` wipes on drop.
        let credential: &Credential = spec.token.as_ref().expect("a token");

        assert_eq!(credential.as_str(), "t0ken");
    }
}
