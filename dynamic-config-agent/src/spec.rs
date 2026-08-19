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
  --watch       poll interval in seconds; absent = one shot (init mode)
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
";

#[derive(Debug)]
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
    pub token: Option<String>,
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
    /// `DYNAMIC_CONFIG_AGENT_PASSWORD`, and no flag: a password in a pod
    /// spec's `args` is a password in `kubectl describe pod`.
    pub password: Option<String>,
}

impl Spec {
    pub fn from_args(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut source = None;
        let mut endpoint = std::env::var("DYNAMIC_CONFIG_AGENT_ENDPOINT").ok();
        let mut key = None;
        let mut out = None;
        let mut watch = None;
        let mut token = std::env::var("DYNAMIC_CONFIG_AGENT_TOKEN").ok();
        let mut section = None;
        let mut file_mode = None;
        let mut metrics_addr = None;
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
                "--token" => token = Some(value("--token")?),
                "--section" => section = Some(value("--section")?),
                "--metrics-addr" => metrics_addr = Some(value("--metrics-addr")?),
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
            password: std::env::var("DYNAMIC_CONFIG_AGENT_PASSWORD").ok(),
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
