//! The stores the agent can speak to, behind the engine's own trait.
//!
//! Nothing here is new machinery: each arm builds one of the published
//! store crates' sources, and the trait they share is the engine's
//! `RemoteSource` — the same one every binding uses. `spec.rs` has
//! already refused every wrong combination, so the `expect`s below are
//! statements, not hopes.

use dynamic_config::RemoteSource;

use crate::spec::Spec;

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

        tls
    }};
}

pub fn build(spec: &Spec) -> Result<Box<dyn RemoteSource>, Box<dyn std::error::Error>> {
    let wants_tls = spec.ca.is_some() || spec.tls_cert.is_some();

    let source: Box<dyn RemoteSource> = match spec.source.as_str() {
        "consul" => {
            let mut store = dynamic_config_consul::Consul::new(&spec.endpoint, spec.key.as_str());

            use dynamic_config_consul::Auth;

            store = match spec.auth.as_deref() {
                None => match &spec.token {
                    Some(token) => store.with_token(token),
                    None => store,
                },
                Some("token") => store.with_token(spec.token.as_deref().expect("validated")),
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
                    spec.token.as_deref().expect("validated"),
                )),
                Some(_) => unreachable!("validated"),
            };

            if wants_tls {
                store = store.with_tls(tls_config!(spec, dynamic_config_consul::TlsConfig));
            }

            Box::new(store)
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
                None | Some("token") => Auth::token(spec.token.as_deref().expect("validated")),
                Some("kubernetes") => {
                    let mut auth = Auth::kubernetes(spec.auth_role.as_deref().expect("validated"));

                    if let Some(path) = &spec.auth_token_path {
                        auth = auth.with_token_path(path);
                    }

                    auth
                }
                Some("approle") => Auth::app_role(
                    spec.auth_role.as_deref().expect("validated"),
                    spec.password.as_deref().expect("validated"),
                ),
                Some("jwt") => {
                    let mut auth = Auth::jwt(spec.token.as_deref().expect("validated"));

                    if let Some(role) = &spec.auth_role {
                        auth = auth.with_role(role);
                    }

                    auth
                }
                Some("userpass") => Auth::userpass(
                    spec.auth_username.as_deref().expect("validated"),
                    spec.password.as_deref().expect("validated"),
                ),
                Some("ldap") => Auth::ldap(
                    spec.auth_username.as_deref().expect("validated"),
                    spec.password.as_deref().expect("validated"),
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

            Box::new(store)
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

            if let Some(token) = &spec.token {
                store = store.with_token(token);
            }

            if wants_tls {
                // The client takes store-core's `TlsConfig`; the name the
                // server crate re-exports is the *server's* TLS section.
                store =
                    store.with_tls(tls_config!(spec, dynamic_config_store_core::tls::TlsConfig));
            }

            Box::new(store)
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
                Some("access-token") => {
                    Auth::access_token(spec.token.as_deref().expect("validated"))
                }
                Some("emulator") => Auth::Emulator,
                Some(_) => unreachable!("validated"),
            });

            if let Some(url) = &spec.api_url {
                store = store.with_endpoint(url);
            }

            if wants_tls {
                store = store.with_tls(tls_config!(spec, dynamic_config_firestore::TlsConfig));
            }

            Box::new(store)
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
                None => match &spec.token {
                    Some(token) => Credential::token(token),
                    None => Credential::anonymous(),
                },
                Some("anonymous") => Credential::anonymous(),
                Some("token") => {
                    let token = spec.token.as_deref().expect("validated");

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

    Ok(source)
}
