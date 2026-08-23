//! The stores the agent can speak to, behind the engine's own trait.
//!
//! Nothing here is new machinery: each arm builds one of the published
//! store crates' sources, and the trait they share is the engine's
//! `RemoteSource` — the same one every binding uses. `spec.rs` has
//! already refused every wrong combination, so the `expect`s below are
//! statements, not hopes.

use std::sync::Arc;

use std::time::Duration;

use dynamic_config::{
    AsyncRemoteSource, Fetched, Lease, RemoteSource, RenewableSource, WatchCapability, Watching,
};

use crate::spec::Spec;

/// What `build` answers: one of the engine's two source traits.
///
/// The blocking six run under `spawn_blocking` — honest async, no
/// `block_on`-inside-a-runtime hazard — and the async three are driven
/// directly. `Arc` rather than `Box` because a blocking fetch moves a
/// clone onto the blocking pool.
pub enum Built {
    Blocking(Arc<dyn RemoteSource>),
    /// A blocking store whose *documents* are leased — a dynamic secret
    /// engine, which mints a credential for this reader alone.
    ///
    /// A variant of its own rather than a flag beside `Blocking`, because
    /// the difference is not how it is read but what has to happen after:
    /// a lease is renewed on a timer whether or not anybody reads, and
    /// handed back when the pod stops needing it.
    Renewable(Arc<dyn RenewableSource>),
    Async(Arc<dyn AsyncRemoteSource>),
}

/// The one-document slot a watch delivers into.
///
/// A `tokio::sync::watch` channel rather than an `mpsc` of capacity one.
/// The two look alike and behave oppositely under load: an `mpsc` of
/// capacity one is a *queue* that holds one — a second document waits, or
/// is refused — while this **overwrites**, which is what the comment beside
/// the old channel claimed and the code did not do.
///
/// It matters because a document that has been superseded is worthless. A
/// burst of writes used to make the blocking stores block their own watch
/// loop and the async stores tear the connection down; now the newest
/// document is simply the one that gets rendered.
#[derive(Clone)]
pub struct Deliver {
    slot: tokio::sync::watch::Sender<Option<Fetched>>,
}

impl Deliver {
    /// Puts `document` in the slot, replacing whatever was there.
    ///
    /// # Errors
    ///
    /// When the renderer is gone — the only condition that ends a watch.
    pub fn send(&self, document: Fetched) -> Result<(), dynamic_config::Error> {
        self.slot
            .send(Some(document))
            .map_err(|_| dynamic_config::Error::remote("the renderer stopped"))
    }
}

/// The reading half.
pub struct Delivered {
    slot: tokio::sync::watch::Receiver<Option<Fetched>>,
}

impl Delivered {
    /// Waits for a document that has not been taken yet.
    ///
    /// `None` when every sender is gone.
    pub async fn recv(&mut self) -> Option<Fetched> {
        loop {
            self.slot.changed().await.ok()?;

            let document = self.slot.borrow_and_update().clone();

            if let Some(document) = document {
                return Some(document);
            }
        }
    }
}

/// A slot and its two halves.
#[must_use]
pub fn delivery() -> (Deliver, Delivered) {
    let (slot, receiver) = tokio::sync::watch::channel(None);

    (Deliver { slot }, Delivered { slot: receiver })
}

impl Built {
    /// The store's own redacted self-description.
    pub fn describe(&self) -> String {
        match self {
            Self::Blocking(source) => source.describe(),
            Self::Renewable(source) => source.describe(),
            Self::Async(source) => source.describe(),
        }
    }

    /// How this store learns that its document changed.
    ///
    /// What the agent plans its loop around: a store that pushes is
    /// watched and resynced, and a store that does not is polled — by the
    /// store's own cheap question where it has one, which for S3 is a
    /// `HEAD` rather than the object.
    pub fn capability(&self) -> WatchCapability {
        match self {
            Self::Blocking(source) => source.watch_capability(),
            Self::Renewable(source) => source.watch_capability(),
            Self::Async(source) => source.watch_capability(),
        }
    }

    /// Watches, sending every document the store delivers.
    ///
    /// The blocking six go to the blocking pool and hand documents back
    /// over the channel; the async three are driven on the reactor. One
    /// place renders, whichever kind ran.
    pub async fn watch(
        &self,
        watching: Watching,
        interval: Duration,
        deliver: Deliver,
    ) -> Result<(), dynamic_config::Error> {
        match self {
            Self::Blocking(_) | Self::Renewable(_) => {
                let source = self.blocking().expect("a blocking arm");

                tokio::task::spawn_blocking(move || {
                    source.watch(&watching, interval, &mut |document| {
                        // Latest wins. A burst of writes produces a burst of
                        // documents, and every one but the newest is about
                        // to be superseded — so the slot is overwritten
                        // rather than queued, and nothing waits.
                        deliver.send(document)
                    })
                })
                .await
                .map_err(|error| dynamic_config::Error::remote(format!("watch task: {error}")))?
            }
            Self::Async(source) => {
                // The same slot, and the same rule. This arm used to *end
                // the watch* when the channel was full — a burst against
                // etcd or NATS tore the connection down and was counted as
                // a reconnect, so the loudest signal an operator had for a
                // sick stream fired hardest when the stream was working.
                source
                    .watch(&watching, interval, &mut move |document| {
                        deliver.send(document)
                    })
                    .await
            }
        }
    }

    /// One document, whichever trait fetches it.
    pub async fn fetch(&self) -> Result<dynamic_config::Fetched, dynamic_config::Error> {
        match self {
            Self::Blocking(_) | Self::Renewable(_) => {
                let source = self.blocking().expect("a blocking arm");

                tokio::task::spawn_blocking(move || source.fetch())
                    .await
                    .map_err(|error| {
                        dynamic_config::Error::remote(format!("fetch task: {error}"))
                    })?
            }
            Self::Async(source) => source.fetch().await,
        }
    }

    /// The blocking half of either blocking arm, as one type.
    ///
    /// `RenewableSource` is a `RemoteSource`, so reading is the same call
    /// on both — only the lease calls need to know which one this is.
    fn blocking(&self) -> Option<Arc<dyn RemoteSource>> {
        match self {
            Self::Blocking(source) => Some(Arc::clone(source)),
            Self::Renewable(source) => Some(Arc::clone(source) as Arc<dyn RemoteSource>),
            Self::Async(_) => None,
        }
    }

    /// Extends a lease, when this store issues them.
    ///
    /// `None` from a store that does not — which is not a failure, and is
    /// why this is an `Option` of a `Result` rather than a `Result` with an
    /// unsupported variant nobody would match on.
    pub async fn renew(&self, lease: Lease) -> Option<Result<Lease, dynamic_config::Error>> {
        let Self::Renewable(source) = self else {
            return None;
        };

        let source = Arc::clone(source);

        // Blocking, like every other Vault call: it goes to the pool
        // rather than parking the reactor for a network round trip.
        Some(
            tokio::task::spawn_blocking(move || source.renew(&lease))
                .await
                .unwrap_or_else(|error| {
                    Err(dynamic_config::Error::remote(format!(
                        "renewal task: {error}"
                    )))
                }),
        )
    }

    /// Hands a lease back. `None` from a store that issues none.
    pub async fn revoke(&self, lease: Lease) -> Option<Result<(), dynamic_config::Error>> {
        let Self::Renewable(source) = self else {
            return None;
        };

        let source = Arc::clone(source);

        Some(
            tokio::task::spawn_blocking(move || source.revoke(&lease))
                .await
                .unwrap_or_else(|error| {
                    Err(dynamic_config::Error::remote(format!(
                        "revocation task: {error}"
                    )))
                }),
        )
    }
}

/// The same three TLS settings, spelled as data, for whichever store
/// takes them. Each store crate re-exports the one shared `TlsConfig`
/// type, which is why one macro serves them all.
macro_rules! tls_config {
    ($spec:expr, $ty:ty) => {{
        let mut tls = <$ty>::new();

        if let Some(ca) = &$spec.ca {
            tls = tls.with_ca_certificate_file(ca);
        }

        if let (Some(cert), Some(key)) = (&$spec.tls_cert, &$spec.tls_key) {
            tls = tls.with_client_certificate_files(cert, key);
        }

        if let Some(name) = &$spec.tls_server_name {
            tls = tls.with_server_name(name);
        }

        if $spec.tls_skip_verify {
            tls = tls.with_skip_verification(true);
        }

        tls
    }};
}

/// `--timeout` where the store has a door for it.
///
/// Every store here defaults to ten seconds, which is right for a store on
/// the same network and wrong for a Git remote across a WAN or a bucket in
/// another region. The workload knows which it has; until this, it had no
/// way to say so and its only recourse was a fetch that kept timing out.
macro_rules! timed {
    ($store:expr, $spec:expr) => {
        match $spec.timeout {
            Some(timeout) => $store.with_timeout(timeout),
            None => $store,
        }
    };
}

pub async fn build(spec: &Spec) -> Result<Built, Box<dyn std::error::Error>> {
    // The async three first: their construction awaits.
    match spec.source.as_str() {
        "etcd" => return etcd(spec).await,
        "nats" => return nats(spec).await,
        "s3" => return s3(spec).await,
        _ => {}
    }

    let wants_tls = spec.ca.is_some()
        || spec.tls_cert.is_some()
        || spec.tls_server_name.is_some()
        || spec.tls_skip_verify;

    let source: Box<dyn RemoteSource> = match spec.source.as_str() {
        "consul" => {
            let mut store = dynamic_config_consul::Consul::new(&spec.endpoint, spec.key.as_str());

            use dynamic_config_consul::Auth;

            store = match spec.auth.as_deref() {
                None => match spec.token() {
                    Some(token) => store.with_token(token),
                    None => store,
                },
                Some("token") => store.with_token(spec.token().expect("validated")),
                Some("kubernetes") => {
                    let method = spec.auth_mount.as_deref().expect("validated");
                    let mut auth = Auth::kubernetes(method);

                    if let Some(path) = &spec.auth_token_path {
                        auth = auth.with_bearer_file(path);
                    }

                    store.with_auth(auth)
                }
                Some("jwt") => store.with_auth(Auth::jwt(
                    spec.auth_mount.as_deref().expect("validated"),
                    spec.token().expect("validated"),
                )),
                Some(_) => unreachable!("validated"),
            };

            if wants_tls {
                store = store.with_tls(tls_config!(spec, dynamic_config_consul::TlsConfig));
            }

            Box::new(timed!(store, spec))
        }
        "vault" => {
            // `--key mount/path`, the way vault CLI users write it.
            let (mount, path) = spec
                .key
                .split_once('/')
                .ok_or("vault's --key is <mount>/<path>")?;

            let mut store = dynamic_config_vault::Vault::new(&spec.endpoint, mount, path);

            use dynamic_config_vault::Auth;

            let mut auth = match spec.auth.as_deref() {
                None | Some("token") => Auth::token(spec.token().expect("validated")),
                Some("kubernetes") => {
                    let mut auth = Auth::kubernetes(spec.auth_role.as_deref().expect("validated"));

                    if let Some(path) = &spec.auth_token_path {
                        auth = auth.with_token_path(path);
                    }

                    auth
                }
                Some("approle") => Auth::app_role(
                    spec.auth_role.as_deref().expect("validated"),
                    spec.password().expect("validated"),
                ),
                Some("jwt") => {
                    let mut auth = Auth::jwt(spec.token().expect("validated"));

                    if let Some(role) = &spec.auth_role {
                        auth = auth.with_role(role);
                    }

                    auth
                }
                Some("userpass") => Auth::userpass(
                    spec.auth_username.as_deref().expect("validated"),
                    spec.password().expect("validated"),
                ),
                Some("ldap") => Auth::ldap(
                    spec.auth_username.as_deref().expect("validated"),
                    spec.password().expect("validated"),
                ),
                Some("cert") => {
                    let mut auth = Auth::certificate();

                    if let Some(role) = &spec.auth_role {
                        auth = auth.with_role(role);
                    }

                    auth
                }
                Some(_) => unreachable!("validated"),
            };

            if let Some(mount) = &spec.auth_mount {
                auth = auth.at_mount(mount);
            }

            store = store.with_auth(auth);

            if let Some(namespace) = &spec.namespace {
                store = store.with_namespace(namespace);
            }

            if wants_tls {
                store = store.with_tls(tls_config!(spec, dynamic_config_vault::TlsConfig));
            }

            // A dynamic engine leaves here by a different door: the lease it
            // issues has to reach the loop that renews it, and a
            // `Box<dyn RemoteSource>` has nowhere to put one.
            if spec.dynamic {
                return Ok(Built::Renewable(Arc::new(
                    timed!(store, spec).with_dynamic(),
                )));
            }

            Box::new(timed!(store, spec))
        }
        "config-server" => {
            let (application, profile) = spec
                .key
                .split_once('/')
                .ok_or("config-server's --key is <application>/<profile>")?;

            let mut store = dynamic_config_server::client::ConfigServer::new(
                &spec.endpoint,
                application,
                profile,
            );

            if spec.auth.as_deref() == Some("kubernetes") {
                // The pod's own projected token, re-read per fetch (it
                // rotates); the server's [kubernetes] auth reviews it.
                // A thin adapter rather than a client knob: building a
                // `ConfigServer` is free (its I/O happens at fetch), so
                // rebuilding one around the CURRENT token each fetch is
                // the whole rotation story.
                let path = spec.auth_token_path.clone().unwrap_or_else(|| {
                    "/var/run/secrets/kubernetes.io/serviceaccount/token".to_owned()
                });

                let described = format!("{} (kubernetes identity)", store.describe());
                let endpoint = spec.endpoint.clone();
                let application = application.to_owned();
                let profile = profile.to_owned();
                let tls =
                    wants_tls.then(|| tls_config!(spec, dynamic_config_store_core::tls::TlsConfig));

                return Ok(Built::Blocking(Arc::new(RotatingBearer {
                    build: Box::new(move || {
                        let mut store = dynamic_config_server::client::ConfigServer::new(
                            &endpoint,
                            &application,
                            &profile,
                        );

                        if let Some(tls) = &tls {
                            store = store.with_tls(tls.clone());
                        }

                        store
                    }),
                    token_path: path.into(),
                    described,
                })));
            }

            if let Some(token) = spec.token() {
                store = store.with_token(token);
            }

            if wants_tls {
                // The client takes store-core's `TlsConfig`; the name the
                // server crate re-exports is the *server's* TLS section.
                store =
                    store.with_tls(tls_config!(spec, dynamic_config_store_core::tls::TlsConfig));
            }

            Box::new(timed!(store, spec))
        }
        "firestore" => {
            // `--endpoint <project>` or `<project>/<database>`.
            let (project, database) = match spec.endpoint.split_once('/') {
                Some((project, database)) => (project, Some(database)),
                None => (spec.endpoint.as_str(), None),
            };

            let mut store = dynamic_config_firestore::Firestore::new(project, spec.key.as_str());

            if let Some(database) = database {
                store = store.with_database(database);
            }

            use dynamic_config_firestore::Auth;

            store = store.with_auth(match spec.auth.as_deref() {
                // The workload's own identity is the default: no secret
                // distributed, and the token renews itself.
                None | Some("metadata-server") => Auth::metadata_server(),
                Some("access-token") => Auth::access_token(spec.token().expect("validated")),
                Some("emulator") => Auth::Emulator,
                Some(_) => unreachable!("validated"),
            });

            if let Some(url) = &spec.api_url {
                store = store.with_endpoint(url);
            }

            if wants_tls {
                store = store.with_tls(tls_config!(spec, dynamic_config_firestore::TlsConfig));
            }

            Box::new(timed!(store, spec))
        }
        "git" => {
            let mut builder =
                dynamic_config_git::GitSource::builder(&spec.endpoint).path(spec.key.as_str());

            builder = match spec.reference.as_deref() {
                None => builder,
                Some(reference) => match reference.split_once(':') {
                    Some(("branch", name)) => builder.branch(name),
                    Some(("tag", name)) => builder.tag(name),
                    Some(("commit", sha)) => builder.commit(sha),
                    Some((kind, _)) => {
                        return Err(format!(
                            "--ref {kind}:…: branch:, tag: or commit: (a bare value \
                             is a branch)"
                        )
                        .into())
                    }
                    None => builder.branch(reference),
                },
            };

            use dynamic_config_git::Credential;

            builder = builder.credential(match spec.auth.as_deref() {
                None => match spec.token() {
                    Some(token) => Credential::token(token),
                    None => Credential::anonymous(),
                },
                Some("anonymous") => Credential::anonymous(),
                Some("token") => {
                    let token = spec.token().expect("validated");

                    match &spec.auth_username {
                        // The host that turns out to care about the user half.
                        Some(username) => Credential::basic(username, token),
                        None => Credential::token(token),
                    }
                }
                // The kubelet gives the agent no ssh-agent; this arm is for
                // the agent run outside a pod. The book says so out loud.
                Some("ssh") => Credential::ssh_agent(),
                Some("ssh-key") => Credential::ssh_key(spec.ssh_key.as_deref().expect("validated")),
                Some(_) => unreachable!("validated"),
            });

            if wants_tls {
                builder = builder.tls(tls_config!(spec, dynamic_config_git::TlsConfig));
            }

            Box::new(builder.build()?)
        }
        "redis" => {
            // Credentials travel in the url — redis has no other place for
            // them — so a url with a password comes in through
            // DYNAMIC_CONFIG_AGENT_ENDPOINT rather than a flag.
            if wants_tls {
                Box::new(dynamic_config_redis::Redis::with_tls(
                    &spec.endpoint,
                    spec.key.as_str(),
                    &tls_config!(spec, dynamic_config_redis::TlsConfig),
                )?)
            } else {
                Box::new(dynamic_config_redis::Redis::new(
                    &spec.endpoint,
                    spec.key.as_str(),
                )?)
            }
        }
        other => return Err(format!("unreachable: {other} passed spec validation").into()),
    };

    Ok(Built::Blocking(Arc::from(source)))
}

/// A config-server client whose bearer is a FILE, re-read per fetch:
/// projected service-account tokens rotate underneath a long-lived
/// sidecar, and the token a fetch presents must be the current one.
/// Builds a fresh client each fetch — construction is free (the
/// client's I/O happens inside `fetch`), so this IS the rotation story.
struct RotatingBearer {
    build: Box<dyn Fn() -> dynamic_config_server::client::ConfigServer + Send + Sync>,
    token_path: std::path::PathBuf,
    described: String,
}

impl RemoteSource for RotatingBearer {
    fn fetch(&self) -> Result<dynamic_config::Fetched, dynamic_config::Error> {
        let token = std::fs::read_to_string(&self.token_path).map_err(|error| {
            dynamic_config::Error::auth(format!(
                "reading the service-account token at {}: {error}",
                self.token_path.display()
            ))
        })?;

        (self.build)().with_token(token.trim()).fetch()
    }

    fn describe(&self) -> String {
        self.described.clone()
    }
}

/// etcd, with the two methods etcd itself speaks — TLS client
/// certificates (the credential IS the certificate) and
/// username/password — both first-class, per the roadmap's identity
/// policy: etcd has no Kubernetes auth method to log into, and a
/// contract that punishes such stores punishes their users.
async fn etcd(spec: &Spec) -> Result<Built, Box<dyn std::error::Error>> {
    use dynamic_config_etcd::{ConnectOptions, Etcd};

    let endpoints: Vec<&str> = spec.endpoint.split(',').map(str::trim).collect();
    let mut options = ConnectOptions::new();

    if let Some(user) = &spec.auth_username {
        options = options.with_user(user, spec.password().expect("validated"));
    }

    let wants_tls = spec.ca.is_some()
        || spec.tls_cert.is_some()
        || spec.tls_server_name.is_some()
        || spec.tls_skip_verify;
    let store = if wants_tls {
        let tls = tls_config!(spec, dynamic_config_etcd::TlsConfig);

        Etcd::with_tls(endpoints, spec.key.as_str(), options, &tls).await?
    } else {
        Etcd::with_options(endpoints, spec.key.as_str(), options).await?
    };

    Ok(Built::Async(Arc::new(store)))
}

/// NATS JetStream KV: `--key <bucket>/<key>`; a `.creds` file — the way
/// a NATS account authenticates — comes in as `--auth-token-path`.
async fn nats(spec: &Spec) -> Result<Built, Box<dyn std::error::Error>> {
    use dynamic_config_nats::{ConnectOptions, Nats};

    let (bucket, key) = spec
        .key
        .split_once('/')
        .ok_or("nats' --key is <bucket>/<key>")?;

    let options = match (&spec.auth_token_path, spec.token()) {
        (Some(path), _) => ConnectOptions::with_credentials_file(path)
            .await
            .map_err(|error| format!("nats credentials file: {error}"))?,
        (None, Some(token)) => ConnectOptions::with_token(token.to_owned()),
        (None, None) => ConnectOptions::new(),
    };

    let store = Nats::with_options(spec.endpoint.as_str(), bucket, key, options).await?;

    Ok(Built::Async(Arc::new(store)))
}

/// S3 (and everything speaking its API): `--endpoint <bucket>`, the
/// object key in `--key`. Credentials come from the ambient chain —
/// which on EKS is IRSA, the workload's own identity, no secret
/// distributed. `--api-url` overrides the endpoint for MinIO/Ceph/R2.
async fn s3(spec: &Spec) -> Result<Built, Box<dyn std::error::Error>> {
    use dynamic_config_s3::S3;

    let store = match &spec.api_url {
        Some(url) => {
            // An explicit endpoint means a non-AWS S3 (MinIO, Ceph, R2).
            // There is no IMDS there, so the SDK's region lookup would
            // hang and then leave the client region-less — which the SDK
            // refuses. The value itself is ignored by these servers.
            let region = aws_config::meta::region::RegionProviderChain::default_provider()
                .or_else("us-east-1");
            let config = aws_config::from_env()
                .region(region)
                .endpoint_url(url)
                .load()
                .await;

            S3::with_config(&config, spec.endpoint.as_str(), spec.key.as_str())
        }
        None => S3::new(spec.endpoint.as_str(), spec.key.as_str()).await?,
    };

    Ok(Built::Async(Arc::new(store)))
}
