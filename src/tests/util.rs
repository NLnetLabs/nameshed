#[cfg(test)]
pub(crate) mod internal {
    use std::sync::Arc;

    use env_logger::Env;

    use crate::metrics::{self, OutputFormat, Target};

    pub const _MOCK_ROUTER_ID: &str = "mock-router";

    /// Tries to enable logging. Intended for use in tests.
    ///
    /// Accepts a log level name as a string, e.g. "trace".
    #[allow(dead_code)]
    pub(crate) fn enable_logging(log_level: &str) {
        let _ = env_logger::Builder::from_env(Env::default().default_filter_or(log_level))
            .is_test(true)
            .try_init();
    }

    pub(crate) fn get_testable_metrics_snapshot(
        metrics: &Arc<impl metrics::Source + ?Sized>,
    ) -> Target {
        let mut target = Target::new(OutputFormat::Test);
        metrics.append("testunit", &mut target);
        target
    }
}

pub fn assert_json_eq(actual_json: serde_json::Value, expected_json: serde_json::Value) {
    use assert_json_diff::{assert_json_matches_no_panic, CompareMode};

    let config = assert_json_diff::Config::new(CompareMode::Strict);
    if let Err(err) = assert_json_matches_no_panic(&actual_json, &expected_json, config) {
        eprintln!(
            "Actual JSON: {}",
            serde_json::to_string_pretty(&actual_json).unwrap()
        );
        eprintln!(
            "Expected JSON: {}",
            serde_json::to_string_pretty(&expected_json).unwrap()
        );
        panic!("JSON doesn't match expectations: {err}");
    }
}

#[cfg(test)]
pub mod net {
    use std::{future::Future, net::SocketAddr, sync::Arc};

    use tokio::net::TcpStream;

    use crate::common::net::{TcpListener, TcpListenerFactory, TcpStreamWrapper};

    /// A mock TcpListenerFactory that stores a callback supplied by the
    /// unit test thereby allowing the unit test to determine if binding to
    /// the given address should succeed or not, and on success delegates to
    /// MockTcpListener.
    pub struct MockTcpListenerFactory<T, U, Fut>
    where
        T: Fn(String) -> std::io::Result<MockTcpListener<U, Fut>>,
        U: Fn() -> Fut,
        Fut: Future<Output = std::io::Result<(MockTcpStreamWrapper, SocketAddr)>>,
    {
        pub bind_cb: T,
        pub binds: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl<T, U, Fut> MockTcpListenerFactory<T, U, Fut>
    where
        T: Fn(String) -> std::io::Result<MockTcpListener<U, Fut>>,
        U: Fn() -> Fut,
        Fut: Future<Output = std::io::Result<(MockTcpStreamWrapper, SocketAddr)>>,
    {
        pub fn new(bind_cb: T) -> Self {
            Self {
                bind_cb,
                binds: Arc::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl<T, U, Fut> TcpListenerFactory<MockTcpListener<U, Fut>> for MockTcpListenerFactory<T, U, Fut>
    where
        T: Fn(String) -> std::io::Result<MockTcpListener<U, Fut>> + std::marker::Sync,
        U: Fn() -> Fut,
        Fut: Future<Output = std::io::Result<(MockTcpStreamWrapper, SocketAddr)>>,
    {
        async fn bind(&self, addr: String) -> std::io::Result<MockTcpListener<U, Fut>> {
            let listener = (self.bind_cb)(addr.clone())?;
            self.binds.lock().unwrap().push(addr);
            Ok(listener)
        }
    }

    /// A mock TcpListener that stores a callback supplied by the unit test
    /// thereby allowing the unit test to determine if accepting incoming
    /// connections should appear to succeed or fail, and on success delegates
    /// to MockTcpStreamWrapper.
    pub struct MockTcpListener<T, Fut>(T)
    where
        T: Fn() -> Fut,
        Fut: Future<Output = std::io::Result<(MockTcpStreamWrapper, SocketAddr)>>;

    impl<T, Fut> MockTcpListener<T, Fut>
    where
        T: Fn() -> Fut,
        Fut: Future<Output = std::io::Result<(MockTcpStreamWrapper, SocketAddr)>>,
    {
        pub fn new(listen_cb: T) -> Self {
            Self(listen_cb)
        }
    }

    #[async_trait::async_trait]
    impl<Fut, T> TcpListener<MockTcpStreamWrapper> for MockTcpListener<T, Fut>
    where
        T: Fn() -> Fut + Sync + Send,
        Fut: Future<Output = std::io::Result<(MockTcpStreamWrapper, SocketAddr)>> + Send,
    {
        async fn accept(&self) -> std::io::Result<(MockTcpStreamWrapper, SocketAddr)> {
            self.0().await
        }
    }

    /// A mock TcpStreamWraper that is not actually usable, but can be passed
    /// in place of a StandardTcpStream in order to avoid needing to create a
    /// real TcpStream which would interact with the actual operating system
    /// network stack.
    pub struct MockTcpStreamWrapper;

    impl TcpStreamWrapper for MockTcpStreamWrapper {
        fn into_inner(self) -> std::io::Result<TcpStream> {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
}
