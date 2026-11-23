//! The HTTP client GPUI asks for but does not provide.
//!
//! `gpui_platform` builds an application with a `NullHttpClient`; every
//! `cx.http_client()` call then fails with "No HttpClient available". Link
//! previews, KLIPY search and the page's own fetch path all reach for that
//! one method, so an application has to install a real client before it
//! opens a window. This is the desktop half: one `ureq` agent and a thread
//! per request. A preview is one or two requests, not a service, and a
//! dedicated runtime would cost more than it saved.

#[cfg(not(target_family = "wasm"))]
pub use imp::install;

#[cfg(target_family = "wasm")]
mod imp {
    /// A page keeps the null client until its Fetch adapter is wired.
    ///
    /// The web build still compiles and draws; previews and KLIPY are the
    /// features that wait. Naming this here is better than a silent `cfg` at
    /// the startup call, which is where the next reader would look for it.
    pub fn install(application: gpui::Application) -> gpui::Application {
        log::warn!("no fetch-backed HttpClient is installed on the web build");
        application
    }
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::sync::Arc;
    use std::time::Duration;

    use gpui::http_client::http::{HeaderValue, Method, Request, Response};
    use gpui::http_client::{AsyncBody, HttpClient, anyhow};

    /// The most one response may put in memory.
    ///
    /// The link parser now has to read past YouTube's 846 KiB of metadata,
    /// and a caller's own `take` happens after the body has already been
    /// buffered here. The limit is what bounds that buffer; no request this
    /// window makes is a multi-megabyte download (media goes through the
    /// daemon's cache instead).
    const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

    /// The one family every request presents. Distinguishable in a server's
    /// log from a generic runtime's, and honest about who is asking.
    const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Wasp/1.2";

    /// A client for the handful of pages the window reads for itself.
    pub struct UreqHttpClient {
        agent: ureq::Agent,
        user_agent: HeaderValue,
    }

    impl UreqHttpClient {
        pub fn new() -> Self {
            let config = ureq::Agent::config_builder()
                // Follow the redirects pages and image CDNs use; the limit is
                // ureq's default and is plenty for a canonical page.
                .max_redirects(10)
                // 4xx/5xx are answers, not transport failures: the caller
                // wants the status and body so its own log can say `404`.
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(20)))
                .user_agent(USER_AGENT)
                .build();
            Self {
                agent: config.into(),
                user_agent: HeaderValue::from_static(USER_AGENT),
            }
        }
    }

    impl Default for UreqHttpClient {
        fn default() -> Self {
            Self::new()
        }
    }

    impl HttpClient for UreqHttpClient {
        fn send(
            &self,
            request: Request<AsyncBody>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Response<AsyncBody>>> + Send>,
        > {
            let agent = self.agent.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            // A request is blocking; a thread of its own keeps the UI thread
            // out of the socket. It is bounded by the caller's timeout and
            // dies with the response it sends back.
            let spawned = std::thread::Builder::new()
                .name("velo-http".to_string())
                .spawn(move || {
                    let _ = sender.send(perform(&agent, request));
                });
            Box::pin(async move {
                if spawned.is_err() {
                    return Err(anyhow!("could not start the HTTP worker"));
                }
                receiver
                    .await
                    .map_err(|_| anyhow!("the HTTP worker stopped before answering"))?
            })
        }

        fn user_agent(&self) -> Option<&HeaderValue> {
            Some(&self.user_agent)
        }

        fn proxy(&self) -> Option<&gpui::http_client::Url> {
            None
        }
    }

    fn perform(
        agent: &ureq::Agent,
        request: Request<AsyncBody>,
    ) -> anyhow::Result<Response<AsyncBody>> {
        // Everything this window fetches is a GET: a page's Open Graph tags,
        // its image, or a KLIPY search. Anything else is a mistake rather
        // than a body this client cannot stream.
        if request.method() != Method::GET {
            return Err(anyhow!(
                "the native client only supports GET, not {}",
                request.method()
            ));
        }
        let uri = request.uri().to_string();
        let mut call = agent.get(&uri);
        for (name, value) in request.headers() {
            if let Ok(value) = value.to_str() {
                call = call.header(name.as_str(), value);
            }
        }
        let mut response = call.call().map_err(|error| anyhow!("{uri}: {error}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|error| anyhow!("{uri}: could not read the body: {error}"))?;
        Response::builder()
            .status(status)
            .body(AsyncBody::from(body))
            .map_err(|error| anyhow!("{uri}: could not build the response: {error}"))
    }

    /// Install the client on the application before its first frame.
    pub fn install(application: gpui::Application) -> gpui::Application {
        log::info!("http client: native ureq installed");
        application.with_http_client(Arc::new(UreqHttpClient::new()))
    }
}
