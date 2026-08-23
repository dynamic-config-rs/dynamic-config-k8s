//! The sidecar loop, against stores that are not a network.
//!
//! Every interesting thing the agent does happens between a document
//! arriving and a file being written: a stream that pushes, a stream that
//! goes quiet, a connection that drops, a document that did not change.
//! None of it is reachable through the binary, and all of it is what a pod
//! depends on.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dynamic_config::{Error, Fetched, Format, RemoteSource, WatchCapability, Watching};
use dynamic_config_agent::sources::Built;
use dynamic_config_agent::spec::Spec;

/// A spec that renders a JSON document to `out`, watching on `interval`.
fn spec(out: &std::path::Path, interval: u64) -> Spec {
    Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--watch",
            &interval.to_string(),
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect("the arguments are a valid spec")
}

fn scratch(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join("dynamic-config-agent-sidecar");

    std::fs::create_dir_all(&directory).expect("the scratch directory is creatable");

    directory.join(name)
}

/// A store that pushes the documents it was given, then goes quiet.
struct Pushing {
    documents: Mutex<Vec<&'static str>>,
    fetches: Arc<AtomicUsize>,
    /// What a resync finds, which is not what the stream ever pushed.
    resync: &'static str,
}

impl RemoteSource for Pushing {
    fn fetch(&self) -> Result<Fetched, Error> {
        self.fetches.fetch_add(1, Ordering::SeqCst);

        Ok(Fetched::new(self.resync, Format::Json))
    }

    fn describe(&self) -> String {
        "a pushing store".to_owned()
    }

    fn watch_capability(&self) -> WatchCapability {
        WatchCapability::Native
    }

    fn watch(
        &self,
        watching: &Watching,
        _interval: Duration,
        on_change: &mut dyn FnMut(Fetched) -> Result<(), Error>,
    ) -> Result<(), Error> {
        for document in self.documents.lock().unwrap().drain(..) {
            on_change(Fetched::new(document, Format::Json))?;
        }

        // And now silence, which is what a forgotten subscription looks
        // like from in here.
        while watching.keep_going() {
            std::thread::sleep(Duration::from_millis(5));
        }

        Ok(())
    }
}

/// A store whose watch ends immediately, over and over.
struct Flapping {
    opened: Arc<AtomicUsize>,
}

impl RemoteSource for Flapping {
    fn fetch(&self) -> Result<Fetched, Error> {
        Ok(Fetched::new(r#"{"port":1}"#, Format::Json))
    }

    fn describe(&self) -> String {
        "a flapping store".to_owned()
    }

    fn watch_capability(&self) -> WatchCapability {
        WatchCapability::Native
    }

    fn watch(
        &self,
        _watching: &Watching,
        _interval: Duration,
        _on_change: &mut dyn FnMut(Fetched) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.opened.fetch_add(1, Ordering::SeqCst);

        Err(Error::remote("the connection dropped"))
    }
}

/// A store that pushes nothing, ever — which is what a healthy store looks
/// like between changes, and what every pod's first second looks like.
struct Quiet {
    document: &'static str,
}

impl RemoteSource for Quiet {
    fn fetch(&self) -> Result<Fetched, Error> {
        Ok(Fetched::new(self.document, Format::Json))
    }

    fn describe(&self) -> String {
        "a store with nothing to say".to_owned()
    }

    fn watch_capability(&self) -> WatchCapability {
        WatchCapability::Native
    }

    fn watch(
        &self,
        watching: &Watching,
        _interval: Duration,
        _on_change: &mut dyn FnMut(Fetched) -> Result<(), Error>,
    ) -> Result<(), Error> {
        // Connected and silent: the shape a subscription has when the
        // configuration is simply not changing.
        while watching.keep_going() {
            std::thread::sleep(Duration::from_millis(20));
        }

        Ok(())
    }
}

#[tokio::test]
async fn a_pushed_document_is_rendered() {
    let out = scratch("pushed.json");
    let _ = std::fs::remove_file(&out);

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#, r#"{"port":2}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":2}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 3600),
        &source,
        Duration::from_secs(3600),
        tokio::time::sleep(Duration::from_millis(300)),
    )
    .await
    .expect("the loop ends when it is told to");

    let rendered = std::fs::read_to_string(&out).expect("the file was written");

    assert!(
        rendered.contains('2'),
        "the last pushed document is what the file holds: {rendered}"
    );
}

/// **The file exists before anything changes.**
///
/// A watch delivers a *change*, and the stores keep that literally: the
/// current value is not delivered at startup. An agent is a caller that
/// wants it anyway — the app beside it opens the rendered file as soon as
/// the pod is ready — so the sidecar fetches and renders once before it
/// starts watching.
///
/// Pinned here because the cost of learning it elsewhere is a kind cluster:
/// this is the regression the e2e smoke caught when the loop began waiting
/// for a delivery that a quiet store never makes.
#[tokio::test]
async fn the_first_render_happens_before_any_change_arrives() {
    let out = scratch("first-render.json");
    let _ = std::fs::remove_file(&out);

    let source = Arc::new(Built::Blocking(Arc::new(Quiet {
        document: r#"{"port":9000}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 3600),
        &source,
        Duration::from_secs(3600),
        tokio::time::sleep(Duration::from_millis(200)),
    )
    .await
    .expect("the loop ends when it is told to");

    let rendered = std::fs::read_to_string(&out)
        .expect("the file is written before the first change, not after it");

    assert!(
        rendered.contains("9000"),
        "the current document is what the file holds: {rendered}"
    );
}

/// The failure a resync exists for: a stream that pushed once and then went
/// quiet, while the store moved on.
#[tokio::test]
async fn a_stream_that_goes_quiet_is_resynced() {
    let out = scratch("resynced.json");
    let _ = std::fs::remove_file(&out);

    let fetches = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#]),
        fetches: Arc::clone(&fetches),
        resync: r#"{"port":9}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 1),
        &source,
        Duration::from_millis(50),
        tokio::time::sleep(Duration::from_millis(400)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert!(
        fetches.load(Ordering::SeqCst) > 0,
        "a store that went quiet was never asked again"
    );

    let rendered = std::fs::read_to_string(&out).expect("the file was written");

    assert!(
        rendered.contains('9'),
        "the resync found what the stream never pushed: {rendered}"
    );
}

/// A watch that keeps ending is reopened, and the waits between attempts
/// grow rather than becoming a tight loop against a store that is down.
#[tokio::test]
async fn a_dropped_watch_is_reopened_and_backed_off_from() {
    let out = scratch("flapping.json");
    let _ = std::fs::remove_file(&out);

    let opened = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(Built::Blocking(Arc::new(Flapping {
        opened: Arc::clone(&opened),
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 1),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(500)),
    )
    .await
    .expect("the loop ends when it is told to");

    let attempts = opened.load(Ordering::SeqCst);

    assert!(attempts > 1, "the watch was never reopened");
    assert!(
        attempts < 25,
        "half a second of a store being down cost {attempts} attempts; \
         the backoff is not backing off"
    );
}

// ---------------------------------------------------------------------------
// The first fetch failing, and what is already on disk
// ---------------------------------------------------------------------------

/// A store that is simply down.
struct Unreachable;

impl RemoteSource for Unreachable {
    fn fetch(&self) -> Result<Fetched, Error> {
        Err(Error::remote("the store is down"))
    }

    fn describe(&self) -> String {
        "an unreachable store".to_owned()
    }
}

fn spec_with(out: &std::path::Path, policy: &str) -> Spec {
    Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--watch",
            "1",
            "--startup-policy",
            policy,
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect("the arguments are a valid spec")
}

/// The `emptyDir` survives a container restart, so an agent coming back
/// after a crash usually finds its own last render on disk. Refusing to
/// start on it turned a store outage into "every restarting pod stays
/// down", when the file it needed was already there.
#[tokio::test]
async fn a_failed_first_fetch_serves_the_file_already_on_disk() {
    let out = scratch("cached.json");
    std::fs::write(&out, r#"{"port":7}"#).expect("a previous render");

    let source = Arc::new(Built::Blocking(Arc::new(Unreachable)));

    dynamic_config_agent::sidecar::run(
        &spec_with(&out, "allow-cached"),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(100)),
    )
    .await
    .expect("the agent starts on what it had");

    let kept = std::fs::read_to_string(&out).expect("the file is still there");

    assert!(
        kept.contains('7'),
        "the cached document must survive: {kept}"
    );
}

/// And with nothing cached there is nothing to allow: the original
/// behaviour, for the original reason — an app reading a file that is not
/// there is worse than a restart with Kubernetes' backoff.
#[tokio::test]
async fn a_failed_first_fetch_with_nothing_cached_still_ends_the_agent() {
    let out = scratch("uncached.json");
    let _ = std::fs::remove_file(&out);

    let source = Arc::new(Built::Blocking(Arc::new(Unreachable)));

    let outcome = dynamic_config_agent::sidecar::run(
        &spec_with(&out, "allow-cached"),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(100)),
    )
    .await;

    assert!(outcome.is_err());
}

/// For a pod that must never start on a credential that may have been
/// rotated out from under it.
#[tokio::test]
async fn require_fresh_refuses_the_cached_document() {
    let out = scratch("require-fresh.json");
    std::fs::write(&out, r#"{"port":7}"#).expect("a previous render");

    let source = Arc::new(Built::Blocking(Arc::new(Unreachable)));

    let outcome = dynamic_config_agent::sidecar::run(
        &spec_with(&out, "require-fresh"),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(100)),
    )
    .await;

    assert!(
        outcome.is_err(),
        "a fresh document was required and none arrived"
    );
}

#[test]
fn an_unknown_startup_policy_is_refused_with_the_three_that_exist() {
    let out = scratch("policy.json");

    let error = Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--startup-policy",
            "whenever",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect_err("not a policy");

    assert!(error.contains("allow-cached"), "{error}");
    assert!(error.contains("require-fresh"), "{error}");
    assert!(error.contains("best-effort"), "{error}");
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

/// A store that mints a credential under a short lease, and counts what is
/// done with it.
struct Leased {
    renewals: Arc<AtomicUsize>,
    revocations: Arc<AtomicUsize>,
    fetches: Arc<AtomicUsize>,
    /// What the lease *says* — the flag the agent is meant to believe.
    renewable: bool,
    /// What `renew` actually does. Separate from `renewable` on purpose:
    /// a lease that claims to be renewable and is then refused is a real
    /// case (a role's `max_ttl`), and it is a different case from one that
    /// never claimed to be.
    refuse_renewal: bool,
}

impl RemoteSource for Leased {
    fn fetch(&self) -> Result<Fetched, Error> {
        let issued = self.fetches.fetch_add(1, Ordering::SeqCst);

        Ok(
            Fetched::new(format!(r#"{{"port":{}}}"#, issued + 1), Format::Json).with_lease(
                dynamic_config::Lease {
                    id: format!("database/creds/app/{issued}"),
                    // Short, so the renewal fires inside a test rather than
                    // in an hour.
                    ttl: Duration::from_millis(120),
                    renewable: self.renewable,
                },
            ),
        )
    }

    fn describe(&self) -> String {
        "a leasing store".to_owned()
    }

    fn watch_capability(&self) -> WatchCapability {
        // What Vault's dynamic mode reports: every read mints a new
        // credential, so there is no cheap "has it changed?" to ask.
        WatchCapability::Interval
    }

    /// Parks until it is stopped, delivering nothing.
    ///
    /// Overridden so the only fetches in this test are the ones the
    /// *lease* logic causes — the default watch is a poll, and its fetches
    /// would be indistinguishable from a renewal re-fetching.
    fn watch(
        &self,
        watching: &Watching,
        _interval: Duration,
        _on_change: &mut dyn FnMut(Fetched) -> Result<(), Error>,
    ) -> Result<(), Error> {
        while watching.keep_going() {
            std::thread::sleep(Duration::from_millis(10));
        }

        Ok(())
    }
}

impl dynamic_config::RenewableSource for Leased {
    fn renew(&self, lease: &dynamic_config::Lease) -> Result<dynamic_config::Lease, Error> {
        self.renewals.fetch_add(1, Ordering::SeqCst);

        if self.refuse_renewal {
            return Err(Error::remote("this lease is past its maximum"));
        }

        Ok(lease.clone())
    }

    fn revoke(&self, _lease: &dynamic_config::Lease) -> Result<(), Error> {
        self.revocations.fetch_add(1, Ordering::SeqCst);

        Ok(())
    }
}

fn leasing(
    renewable: bool,
    refuse_renewal: bool,
) -> (
    Arc<Built>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let renewals = Arc::new(AtomicUsize::new(0));
    let revocations = Arc::new(AtomicUsize::new(0));
    let fetches = Arc::new(AtomicUsize::new(0));

    let source = Arc::new(Built::Renewable(Arc::new(Leased {
        renewals: Arc::clone(&renewals),
        revocations: Arc::clone(&revocations),
        fetches: Arc::clone(&fetches),
        renewable,
        refuse_renewal,
    })));

    (source, renewals, revocations, fetches)
}

/// The lease is renewed on its own clock — not the store's poll interval,
/// which for a dynamic engine reports `Interval` and would never have
/// fired the way the resync is gated.
#[tokio::test]
async fn a_lease_is_renewed_on_its_own_clock() {
    let out = scratch("leased.json");
    let _ = std::fs::remove_file(&out);

    let (source, renewals, _revocations, fetches) = leasing(true, false);

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(400)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert!(
        renewals.load(Ordering::SeqCst) >= 2,
        "a 120ms lease over 400ms should renew more than once, not {}",
        renewals.load(Ordering::SeqCst)
    );

    // **A renewal is not a render.** Extending a lease keeps the same
    // credential; re-fetching would mint a new one and rewrite the file.
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        1,
        "renewing must not re-fetch"
    );
}

/// A renewal that fails is not waited out the way a read is: the credential
/// is expiring, and the only recovery is a new one — which *is* a new
/// document, so it renders.
///
/// The lease claims to be renewable here and is refused anyway, which is
/// what a role's `max_ttl` produces: the agent was right to ask, and the
/// answer changed the plan.
#[tokio::test]
async fn a_refused_renewal_re_fetches() {
    let out = scratch("unrenewable.json");
    let _ = std::fs::remove_file(&out);

    let (source, renewals, _revocations, fetches) = leasing(true, true);

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(400)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert!(
        renewals.load(Ordering::SeqCst) >= 1,
        "the renewal was tried"
    );
    assert!(
        fetches.load(Ordering::SeqCst) >= 2,
        "a credential that cannot be extended has to be re-issued: {} fetches",
        fetches.load(Ordering::SeqCst)
    );
}

/// The credential dies with the pod rather than outliving it.
#[tokio::test]
async fn a_lease_is_handed_back_on_shutdown() {
    let out = scratch("revoked.json");
    let _ = std::fs::remove_file(&out);

    let (source, _renewals, revocations, _fetches) = leasing(true, false);

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(50)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert_eq!(
        revocations.load(Ordering::SeqCst),
        1,
        "a credential minted for this pod alone must not outlive it"
    );
}

/// The opt-out, for a lease something else is still using.
#[tokio::test]
async fn revocation_can_be_declined() {
    let out = scratch("kept.json");
    let _ = std::fs::remove_file(&out);

    let (source, _renewals, revocations, _fetches) = leasing(true, false);

    let mut spec = spec(&out, 60);
    spec.revoke_on_shutdown = false;

    dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(50)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert_eq!(revocations.load(Ordering::SeqCst), 0);
}

/// A lease that says it cannot be renewed is **never sent a renewal**.
///
/// `pki/issue` answers `renewable: false` on every certificate it mints,
/// and so does a database credential past its role's maximum. Asking such
/// a lease to renew is a request that can only be refused: a round trip
/// per cycle per pod, and — worse — a `lease_renewal_failures_total` that
/// climbs steadily on a fleet where nothing is wrong. A failure counter
/// that is never zero is a counter nobody alerts on.
///
/// What happens instead is a re-fetch, later in the lease's life than a
/// renewal would have been: re-issuing early only shortens the credential
/// actually in use.
#[tokio::test]
async fn a_non_renewable_lease_is_never_sent_a_renewal() {
    let out = scratch("non-renewable.json");
    let _ = std::fs::remove_file(&out);

    let (source, renewals, _revocations, fetches) = leasing(false, false);

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(400)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert_eq!(
        renewals.load(Ordering::SeqCst),
        0,
        "a lease that says it is not renewable must not be asked to renew"
    );

    // The credential still has to keep coming: not renewing is not the
    // same as letting it expire.
    assert!(
        fetches.load(Ordering::SeqCst) >= 2,
        "a non-renewable credential is re-issued instead: {} fetches",
        fetches.load(Ordering::SeqCst)
    );

    // ... and later than a renewal would have fired. A 120ms lease
    // re-fetches at ~108ms rather than ~78ms, so 400ms holds fewer
    // re-issues than it would have held renewals.
    assert!(
        fetches.load(Ordering::SeqCst) <= 5,
        "re-issuing at 0.9 of the lease, not at 0.65: {} fetches in 400ms",
        fetches.load(Ordering::SeqCst)
    );
}

/// Something else in the pod wrote to the rendered file, and the agent
/// notices without the store having changed.
#[tokio::test]
async fn drift_is_noticed_and_repaired() {
    let out = scratch("drifted.json");
    let _ = std::fs::remove_file(&out);

    let mut spec = spec(&out, 60);
    spec.on_drift = dynamic_config_agent::spec::OnDrift::Repair;

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":1}"#,
    })));

    // The first render lands, then something overwrites it — and the store
    // has nothing new to say, so only the drift check can put it back.
    let vandalise = {
        let out = out.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            std::fs::write(&out, "someone else was here").expect("the file is writable");
        })
    };

    dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        // Short, so the drift timer fires several times inside the test.
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(300)),
    )
    .await
    .expect("the loop ends when it is told to");

    vandalise.await.expect("the write happened");

    let served = std::fs::read_to_string(&out).expect("the file is there");

    assert!(
        served.contains("\"port\""),
        "the rendered document was written back, not left as {served:?}"
    );
    assert!(
        dynamic_config_agent::metrics::DRIFT_TOTAL.load(Ordering::SeqCst) >= 1,
        "and it was counted"
    );
}

/// `warn` leaves the file alone. The agent owns what it wrote, not what the
/// pod does with it afterwards — but the difference stops being invisible.
#[tokio::test]
async fn drift_under_warn_is_reported_and_left_alone() {
    let out = scratch("drift-warned.json");
    let _ = std::fs::remove_file(&out);

    let spec = spec(&out, 60);

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":1}"#,
    })));

    let out_for_write = out.clone();

    let vandalise = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        std::fs::write(&out_for_write, "someone else was here").expect("writable");
    });

    dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(200)),
    )
    .await
    .expect("the loop ends when it is told to");

    vandalise.await.expect("the write happened");

    assert_eq!(
        std::fs::read_to_string(&out).expect("still there"),
        "someone else was here"
    );
}

/// A rotated CA ends the loop asking to be rebuilt — **not** as a failure,
/// and not as a pod restart. The process stays up and the rendered file
/// never leaves the volume; only the store's client is new.
#[tokio::test]
async fn rotated_trust_material_asks_for_a_rebuild() {
    let out = scratch("rotated.json");
    let _ = std::fs::remove_file(&out);

    let ca = scratch("rotating-ca.pem");
    std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nfirst\n").expect("writable");

    let spec = Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--watch",
            "60",
            "--ca",
            &ca.display().to_string(),
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect("a valid spec");

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":1}"#,
    })));

    let ca_for_rotation = ca.clone();

    let rotate = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        std::fs::write(&ca_for_rotation, "-----BEGIN CERTIFICATE-----\nsecond\n")
            .expect("writable");
    });

    let before = dynamic_config_agent::metrics::TLS_RELOADS_TOTAL.load(Ordering::SeqCst);

    let ended = dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        // Short, so the material is checked several times inside the test.
        Duration::from_millis(20),
        // Long enough that the rotation is what ends the loop, not this.
        tokio::time::sleep(Duration::from_secs(5)),
    )
    .await
    .expect("a rebuild is not a failure");

    rotate.await.expect("the rotation happened");

    assert_eq!(ended, dynamic_config_agent::sidecar::Ended::Rebuild);
    assert!(
        dynamic_config_agent::metrics::TLS_RELOADS_TOTAL.load(Ordering::SeqCst) > before,
        "and it was counted"
    );

    // The document it had already rendered is still there: a rebuild is
    // not a restart, and nothing on the volume is disturbed by one.
    assert!(std::fs::read_to_string(&out)
        .expect("the file survived")
        .contains("port"));
}

/// `--no-tls-reload` leaves it alone, for a deployment that would rather
/// restart on its own terms.
#[tokio::test]
async fn the_rebuild_can_be_turned_off() {
    let out = scratch("not-rotated.json");
    let _ = std::fs::remove_file(&out);

    let ca = scratch("static-ca.pem");
    std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nfirst\n").expect("writable");

    let spec = Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--watch",
            "60",
            "--ca",
            &ca.display().to_string(),
            "--no-tls-reload",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect("a valid spec");

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":1}"#,
    })));

    let ca_for_rotation = ca.clone();

    let rotate = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        std::fs::write(&ca_for_rotation, "-----BEGIN CERTIFICATE-----\nsecond\n")
            .expect("writable");
    });

    let ended = dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(250)),
    )
    .await
    .expect("the loop ends when it is told to");

    rotate.await.expect("the rotation happened");

    assert_eq!(
        ended,
        dynamic_config_agent::sidecar::Ended::Stopped,
        "the rotation was ignored, and the stop is what ended it"
    );
}

/// The generation a render replaces is kept, so an incident can ask what
/// the file was before — which the rename that publishes the new one
/// otherwise destroys.
#[tokio::test]
async fn the_replaced_generation_is_kept() {
    let out = scratch("history.json");
    let _ = std::fs::remove_file(&out);
    let _ =
        std::fs::remove_dir_all(dynamic_config_agent::render::history_path(&out).expect("a name"));

    let mut spec = spec(&out, 60);
    spec.history = 3;

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![
            r#"{"port":4}"#,
            r#"{"port":3}"#,
            r#"{"port":2}"#,
            r#"{"port":1}"#,
        ]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":4}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(300)),
    )
    .await
    .expect("the loop ends when it is told to");

    let directory = dynamic_config_agent::render::history_path(&out).expect("a name");
    let mut kept: Vec<String> = std::fs::read_dir(&directory)
        .expect("the history directory")
        .filter_map(Result::ok)
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect();

    kept.sort();

    assert!(
        !kept.is_empty(),
        "four documents arrived and none of the replaced ones was kept"
    );
    assert!(
        kept.len() <= 3,
        "--history 3 keeps three, not {}: {kept:?}",
        kept.len()
    );

    // The document *currently* served is not in the history — history is
    // what was replaced, and the live file is where the live one lives.
    let served = std::fs::read_to_string(&out).expect("the render");

    assert!(
        !kept.contains(&served),
        "the live document must not also be a history entry"
    );
}

/// Off unless asked. The rendered volume is the pod's memory by default,
/// so keeping copies is a choice somebody makes rather than one made for
/// them.
#[tokio::test]
async fn history_is_off_by_default() {
    let out = scratch("no-history.json");
    let _ = std::fs::remove_file(&out);
    let _ =
        std::fs::remove_dir_all(dynamic_config_agent::render::history_path(&out).expect("a name"));

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":2}"#, r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":2}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(200)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert!(
        !dynamic_config_agent::render::history_path(&out)
            .expect("a name")
            .exists(),
        "nothing asked for a history and one was written anyway"
    );
}

/// A pod outside the cohort keeps what it is serving, and publishes the
/// held document when the percentage grows past its bucket — **without a
/// restart**, which is the whole difference between a canary and a rollout.
#[tokio::test]
async fn a_held_document_is_published_when_the_cohort_widens() {
    let out = scratch("canary.json");
    let _ = std::fs::remove_file(&out);

    let percent = scratch("canary-percent");

    // Zero admits nobody, so the first change is held whatever this pod's
    // bucket turns out to be — which keeps the test independent of the
    // hash.
    std::fs::write(&percent, "0").expect("writable");

    let mut spec = spec(&out, 60);
    spec.canary = Some(percent.clone());

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":2}"#, r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":2}"#,
    })));

    let widen = {
        let percent = percent.clone();

        tokio::spawn(async move {
            // Long enough that the second document has arrived and been
            // held, short enough to leave the loop time to notice.
            tokio::time::sleep(Duration::from_millis(150)).await;
            std::fs::write(&percent, "100").expect("writable");
        })
    };

    dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(400)),
    )
    .await
    .expect("the loop ends when it is told to");

    widen.await.expect("the cohort widened");

    let served = std::fs::read_to_string(&out).expect("the render");

    assert!(
        served.contains("\"port\": 2") || served.contains("\"port\":2"),
        "the held document was published once the cohort widened: {served}"
    );
    assert_eq!(
        dynamic_config_agent::metrics::CANARY_HOLDING.load(Ordering::SeqCst),
        0,
        "and the gauge came back down"
    );
}

/// Nothing configured is not a canary of zero: every pod publishes, which
/// is what every pod without the annotation does.
#[tokio::test]
async fn without_a_canary_everything_publishes() {
    let out = scratch("no-canary.json");
    let _ = std::fs::remove_file(&out);

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":2}"#, r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":2}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(200)),
    )
    .await
    .expect("the loop ends when it is told to");

    let served = std::fs::read_to_string(&out).expect("the render");

    assert!(served.contains('2'), "{served}");
}

/// A store that issues no leases is untouched by any of it — no renewal
/// timer that fires, no revocation on the way out.
#[tokio::test]
async fn a_store_without_leases_is_unaffected() {
    let out = scratch("unleased.json");
    let _ = std::fs::remove_file(&out);

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":1}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(120)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert!(std::fs::read_to_string(&out).is_ok_and(|text| text.contains('1')));
}

/// A burst of writes renders the newest document, not every document — and
/// crucially does not tear the watch down.
///
/// The slot used to be an `mpsc` of capacity one, which is a *queue* that
/// holds one rather than a slot that overwrites: the blocking stores
/// blocked their own watch loop on it, and the async stores returned an
/// error that ended the connection and counted a reconnect. So the loudest
/// signal for a sick stream fired hardest when the stream was healthiest.
#[tokio::test]
async fn a_burst_renders_the_newest_document_without_reopening_the_watch() {
    let out = scratch("burst.json");
    let _ = std::fs::remove_file(&out);

    struct Bursting {
        opened: Arc<AtomicUsize>,
    }

    impl RemoteSource for Bursting {
        fn fetch(&self) -> Result<Fetched, Error> {
            Ok(Fetched::new(r#"{"port":0}"#, Format::Json))
        }

        fn describe(&self) -> String {
            "a bursting store".to_owned()
        }

        fn watch_capability(&self) -> WatchCapability {
            WatchCapability::Native
        }

        fn watch(
            &self,
            watching: &Watching,
            _interval: Duration,
            on_change: &mut dyn FnMut(Fetched) -> Result<(), Error>,
        ) -> Result<(), Error> {
            self.opened.fetch_add(1, Ordering::SeqCst);

            // Faster than the renderer can possibly keep up with.
            for port in 1..=200 {
                on_change(Fetched::new(format!(r#"{{"port":{port}}}"#), Format::Json))?;
            }

            while watching.keep_going() {
                std::thread::sleep(Duration::from_millis(10));
            }

            Ok(())
        }
    }

    let opened = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(Built::Blocking(Arc::new(Bursting {
        opened: Arc::clone(&opened),
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(300)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert_eq!(
        opened.load(Ordering::SeqCst),
        1,
        "a burst is not a reason to reopen the watch"
    );

    let rendered = std::fs::read_to_string(&out).expect("something was rendered");

    assert!(
        rendered.contains("200"),
        "the newest document is the one that must land: {rendered}"
    );
}

/// A slow resync must not stop everything else.
///
/// The fetch used to be awaited inside the `select!` arm body, which holds
/// the whole loop: one hung `GET` stopped deliveries, lease renewals and
/// the shutdown branch along with the resync it belonged to.
#[tokio::test]
async fn a_slow_resync_does_not_stall_the_loop() {
    let out = scratch("slow-resync.json");
    let _ = std::fs::remove_file(&out);

    struct SlowFetch {
        pushed: Arc<AtomicUsize>,
        fetches: Arc<AtomicUsize>,
    }

    impl RemoteSource for SlowFetch {
        fn fetch(&self) -> Result<Fetched, Error> {
            // The first one answers — the initial render has to wait for a
            // fetch whatever happens, and that is not what this is about.
            // Every one after it is the hung `GET`.
            // Two seconds rather than thirty: long enough that the loop
            // cannot have waited for it, short enough that the blocking
            // pool does not hold the test binary open afterwards.
            if self.fetches.fetch_add(1, Ordering::SeqCst) > 0 {
                std::thread::sleep(Duration::from_secs(2));
            }

            Ok(Fetched::new(r#"{"port":0}"#, Format::Json))
        }

        fn describe(&self) -> String {
            "a store whose reads hang".to_owned()
        }

        fn watch_capability(&self) -> WatchCapability {
            WatchCapability::Native
        }

        fn watch(
            &self,
            watching: &Watching,
            _interval: Duration,
            on_change: &mut dyn FnMut(Fetched) -> Result<(), Error>,
        ) -> Result<(), Error> {
            let mut port = 0;

            while watching.keep_going() {
                port += 1;
                self.pushed.fetch_add(1, Ordering::SeqCst);

                on_change(Fetched::new(format!(r#"{{"port":{port}}}"#), Format::Json))?;

                std::thread::sleep(Duration::from_millis(30));
            }

            Ok(())
        }
    }

    let pushed = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(Built::Blocking(Arc::new(SlowFetch {
        pushed: Arc::clone(&pushed),
        fetches: Arc::new(AtomicUsize::new(0)),
    })));

    let started = std::time::Instant::now();

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(300)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert!(
        started.elapsed() < Duration::from_millis(900),
        "the shutdown branch waited on a hung fetch: {:?}",
        started.elapsed()
    );

    assert!(
        pushed.load(Ordering::SeqCst) > 1,
        "the stream kept delivering while the resync was stuck: {} pushes",
        pushed.load(Ordering::SeqCst)
    );

    let rendered = std::fs::read_to_string(&out).expect("something was rendered");

    assert!(
        rendered.contains("port"),
        "a push landed while the resync hung: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// The meta file, and the staleness ceiling
// ---------------------------------------------------------------------------

/// The question an application cannot otherwise ask: *which configuration
/// am I actually running?* Two pods holding the same file is a claim nobody
/// can check from inside either of them; two pods printing the same digest
/// is one anybody can.
#[tokio::test]
async fn a_meta_file_describes_the_render_without_containing_it() {
    let out = scratch("described.json");
    let meta = out.with_file_name(".described.json.meta");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&meta);

    struct Versioned;

    impl RemoteSource for Versioned {
        fn fetch(&self) -> Result<Fetched, Error> {
            Ok(Fetched::new(r#"{"password":"hunter2"}"#, Format::Json)
                .with_revision(dynamic_config::Revision::Counter(42)))
        }

        fn describe(&self) -> String {
            "a versioned store".to_owned()
        }
    }

    let source = Arc::new(Built::Blocking(Arc::new(Versioned)));

    let mut spec = spec(&out, 60);
    spec.meta = true;

    dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(80)),
    )
    .await
    .expect("the loop ends when it is told to");

    let described = std::fs::read_to_string(&meta).expect("the meta file is written");

    assert!(described.contains("\"revision\": \"42\""), "{described}");
    assert!(described.contains("sha256:"), "{described}");
    assert!(described.contains("rendered_at"), "{described}");

    // The one thing a file sitting beside a rendered secret must not do.
    assert!(
        !described.contains("hunter2"),
        "the meta file describes the render; it does not contain it: {described}"
    );

    // And the digest is checkable: it is the digest of the bytes on disk.
    let document = std::fs::read_to_string(&out).expect("the document");
    assert!(
        described.contains(&dynamic_config_agent::render::digest(&document)),
        "the digest must match the file it describes"
    );
}

/// Off unless asked for: it is a second file in a directory an application
/// may be globbing.
#[tokio::test]
async fn no_meta_file_unless_one_was_asked_for() {
    let out = scratch("undescribed.json");
    let meta = out.with_file_name(".undescribed.json.meta");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&meta);

    let source = Arc::new(Built::Blocking(Arc::new(Pushing {
        documents: Mutex::new(vec![r#"{"port":1}"#]),
        fetches: Arc::new(AtomicUsize::new(0)),
        resync: r#"{"port":1}"#,
    })));

    dynamic_config_agent::sidecar::run(
        &spec(&out, 60),
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(80)),
    )
    .await
    .expect("the loop ends when it is told to");

    assert!(std::fs::read_to_string(&out).is_ok());
    assert!(std::fs::read_to_string(&meta).is_err());
}

/// Last-known-good answers "is there a document"; it leaves "is it too old
/// to trust" open. A credential may be worthless after five minutes and a
/// feature flag fine after a week, so the ceiling is off unless set.
#[test]
fn the_staleness_ceiling_decides_readiness_only_when_one_is_set() {
    use dynamic_config_agent::metrics;

    metrics::set_max_staleness(0);
    metrics::rendered();

    assert!(
        metrics::rendered_at_least_once(),
        "no ceiling means a rendered document is always ready"
    );

    // A ceiling of one second, against a success timestamp that has not
    // been set by this test — which is what a store that has never answered
    // looks like.
    metrics::set_max_staleness(1);

    // Put it back, so the ordering of tests in this binary cannot matter.
    metrics::set_max_staleness(0);
}

// ---------------------------------------------------------------------------
// Several files, one generation
// ---------------------------------------------------------------------------

fn spec_with_also(out: &std::path::Path, also: &[&str]) -> Spec {
    let mut arguments = vec![
        "--source".to_owned(),
        "consul".to_owned(),
        "--endpoint".to_owned(),
        "http://127.0.0.1:1".to_owned(),
        "--key".to_owned(),
        "unused".to_owned(),
        "--out".to_owned(),
        out.display().to_string(),
        "--watch".to_owned(),
        "60".to_owned(),
    ];

    for rendering in also {
        arguments.push("--also".to_owned());
        arguments.push((*rendering).to_owned());
    }

    Spec::from_args(arguments.into_iter()).expect("the arguments are a valid spec")
}

/// One document, two files, and they move together.
#[tokio::test]
async fn several_renderings_of_one_document_are_published_together() {
    let main = scratch("both-main.json");
    let extra = scratch("both-extra.json");
    let _ = std::fs::remove_file(&main);
    let _ = std::fs::remove_file(&extra);

    struct Sectioned;

    impl RemoteSource for Sectioned {
        fn fetch(&self) -> Result<Fetched, Error> {
            Ok(Fetched::new(
                r#"{"db": {"port": 5432}, "cache": {"port": 6379}}"#,
                Format::Json,
            ))
        }

        fn describe(&self) -> String {
            "a store with two sections".to_owned()
        }
    }

    let source = Arc::new(Built::Blocking(Arc::new(Sectioned)));

    let spec = spec_with_also(&main, &[&format!("out={},section=cache", extra.display())]);

    dynamic_config_agent::sidecar::run(
        &spec,
        &source,
        Duration::from_secs(60),
        tokio::time::sleep(Duration::from_millis(80)),
    )
    .await
    .expect("the loop ends when it is told to");

    let first = std::fs::read_to_string(&main).expect("the main file");
    let second = std::fs::read_to_string(&extra).expect("the extra file");

    assert!(first.contains("5432"), "{first}");
    assert!(second.contains("6379"), "{second}");
}

/// **The property.** A failure in the second rendering must not leave the
/// first one published: an application reading both never sees one from
/// before a change and one from after it.
///
/// The failure used here is a schema refusal, because it is the realistic
/// one — a section that is missing is *not* an error (the engine treats
/// every layer as optional, so an absent section resolves to an empty
/// document), and pretending otherwise would test a behaviour this does
/// not have.
///
/// The only schema-using test in this binary: the compiled schema is a
/// process-wide `OnceLock`, which is right for an agent that takes one
/// `--schema` and wrong for a second test that wanted a different one.
#[tokio::test]
async fn a_failure_in_one_rendering_publishes_none_of_them() {
    let main = scratch("none-main.json");
    let extra = scratch("none-extra.json");
    let schema = scratch("none-schema.json");

    // Both hold a previous generation, so "nothing was written" is
    // distinguishable from "nothing was there".
    std::fs::write(&main, r#"{"port":1}"#).expect("a previous render");
    std::fs::write(&extra, r#"{"port":1}"#).expect("a previous render");

    std::fs::write(&schema, r#"{"type": "object", "required": ["port"]}"#)
        .expect("the schema writes");

    // `db` has a port; `cache` is not in the document at all, so it
    // resolves to an empty object — which the schema refuses.
    let document = Fetched::new(r#"{"db": {"port": 5432}}"#, Format::Json);

    let spec = Spec::from_args(
        [
            "--source".to_owned(),
            "consul".to_owned(),
            "--endpoint".to_owned(),
            "http://127.0.0.1:1".to_owned(),
            "--key".to_owned(),
            "unused".to_owned(),
            "--out".to_owned(),
            main.display().to_string(),
            "--section".to_owned(),
            "db".to_owned(),
            "--schema".to_owned(),
            schema.display().to_string(),
            "--also".to_owned(),
            format!("out={},section=cache", extra.display()),
        ]
        .into_iter(),
    )
    .expect("a valid spec");

    let outcome = dynamic_config_agent::render::render_all(&document, &spec);

    let error = outcome
        .err()
        .expect("the second rendering does not satisfy the schema")
        .to_string();

    // The message names the file that failed, so an operator knows which
    // of the set to look at.
    assert!(error.contains("none-extra.json"), "{error}");

    // And nothing moved. This is the whole property: the *first* rendering
    // was perfectly good, and it was not published either.
    assert_eq!(
        std::fs::read_to_string(&main).expect("the main file"),
        r#"{"port":1}"#,
        "the good rendering must not be published when a later one fails"
    );
    assert_eq!(
        std::fs::read_to_string(&extra).expect("the extra file"),
        r#"{"port":1}"#
    );
}

/// An `--also` without a `mode` takes the main render's, so two files
/// holding the same secret do not land with different permissions.
#[test]
fn a_rendering_inherits_the_file_mode_unless_it_names_one() {
    let main = scratch("modes.json");
    let extra = scratch("modes-extra.json");

    let spec = Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &main.display().to_string(),
            "--file-mode",
            "0600",
            "--also",
            &format!("out={}", extra.display()),
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect("a valid spec");

    assert_eq!(spec.file_mode, Some(0o600));
    assert_eq!(spec.also[0].file_mode, Some(0o600));
}

#[test]
fn a_rendering_needs_an_output_and_a_known_format() {
    let out = scratch("bad.json");

    let missing = Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--also",
            "section=db",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect_err("no `out`");

    assert!(missing.contains("out"), "{missing}");

    let unknown = Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--also",
            "out=/config/app.bin",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect_err("not a format this renders");

    assert!(unknown.contains("format"), "{unknown}");
}

// ---------------------------------------------------------------------------
// A document that stopped being there
// ---------------------------------------------------------------------------

/// A store that answers "that path holds nothing" — the shape a deleted
/// Vault secret or a removed Consul key arrives in.
struct Vanished;

impl RemoteSource for Vanished {
    fn fetch(&self) -> Result<Fetched, Error> {
        Err(Error::absent("the key holds no value"))
    }

    fn describe(&self) -> String {
        "a store whose document was deleted".to_owned()
    }

    fn watch_capability(&self) -> WatchCapability {
        // `Native`, so the resync arm — where the policy is applied —
        // actually fires.
        WatchCapability::Native
    }

    fn watch(
        &self,
        watching: &Watching,
        _interval: Duration,
        _on_change: &mut dyn FnMut(Fetched) -> Result<(), Error>,
    ) -> Result<(), Error> {
        while watching.keep_going() {
            std::thread::sleep(Duration::from_millis(10));
        }

        Ok(())
    }
}

fn spec_on_delete(out: &std::path::Path, policy: &str) -> Spec {
    Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--watch",
            "1",
            "--startup-policy",
            "allow-cached",
            "--on-delete",
            policy,
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect("a valid spec")
}

/// The default. A delete is very often a mistake somebody is about to
/// undo, and keeping the last good document cannot lose an application its
/// configuration.
#[tokio::test]
async fn a_deleted_document_keeps_the_last_good_file_by_default() {
    let out = scratch("deleted-retain.json");
    std::fs::write(&out, r#"{"port":1}"#).expect("a previous render");

    let source = Arc::new(Built::Blocking(Arc::new(Vanished)));

    dynamic_config_agent::sidecar::run(
        &spec_on_delete(&out, "retain"),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(150)),
    )
    .await
    .expect("retain keeps the agent running");

    assert_eq!(
        std::fs::read_to_string(&out).expect("the file"),
        r#"{"port":1}"#
    );
}

/// For a credential whose disappearance is meant to take effect: an
/// application that reads an empty file and fails is better than one that
/// goes on using a secret somebody revoked.
#[tokio::test]
async fn remove_empties_the_file() {
    let out = scratch("deleted-remove.json");
    std::fs::write(&out, r#"{"port":1}"#).expect("a previous render");

    let source = Arc::new(Built::Blocking(Arc::new(Vanished)));

    dynamic_config_agent::sidecar::run(
        &spec_on_delete(&out, "remove"),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(150)),
    )
    .await
    .expect("remove keeps the agent running");

    assert_eq!(std::fs::read_to_string(&out).expect("the file"), "");
}

/// And `fail` ends the agent, so the pod restarts rather than serving a
/// document the store no longer has.
#[tokio::test]
async fn fail_ends_the_agent() {
    let out = scratch("deleted-fail.json");
    std::fs::write(&out, r#"{"port":1}"#).expect("a previous render");

    let source = Arc::new(Built::Blocking(Arc::new(Vanished)));

    let outcome = dynamic_config_agent::sidecar::run(
        &spec_on_delete(&out, "fail"),
        &source,
        Duration::from_millis(20),
        tokio::time::sleep(Duration::from_millis(400)),
    )
    .await;

    assert!(outcome.is_err(), "fail must end the agent");
}

#[test]
fn an_unknown_delete_policy_is_refused_with_the_three_that_exist() {
    let out = scratch("policy-delete.json");

    let error = Spec::from_args(
        [
            "--source",
            "consul",
            "--endpoint",
            "http://127.0.0.1:1",
            "--key",
            "unused",
            "--out",
            &out.display().to_string(),
            "--on-delete",
            "whatever",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .expect_err("not a policy");

    assert!(error.contains("retain"), "{error}");
    assert!(error.contains("remove"), "{error}");
    assert!(error.contains("fail"), "{error}");
}
