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
