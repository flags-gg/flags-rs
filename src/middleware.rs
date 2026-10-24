use crate::{Client, FlagError};
use futures::future::BoxFuture;
use http::{Request, Response};
use pin_project::pin_project;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// Maximum number of flag names evaluated from a single request header.
const MAX_HEADER_FLAGS: usize = 50;

/// Adds the flags [`Client`] to every request's extensions (see [`RequestExt`]).
///
/// Optionally, with [`FlagsLayer::expose_header`], it evaluates the flag names a caller
/// lists in the request header (`X-Feature-Flags` by default) and echoes the enabled
/// ones back in the `X-Enabled-Flags` response header. This is off by default because
/// it lets any client probe your flag states.
#[derive(Clone)]
pub struct FlagsLayer {
    client: Arc<Client>,
    header_name: String,
    expose_header: bool,
}

impl FlagsLayer {
    pub fn new(client: Client) -> Self {
        Self {
            client: Arc::new(client),
            header_name: "X-Feature-Flags".to_string(),
            expose_header: false,
        }
    }

    pub fn with_header_name(mut self, name: impl Into<String>) -> Self {
        self.header_name = name.into();
        self
    }

    /// Evaluate flags named in the request header and echo the enabled ones in
    /// `X-Enabled-Flags`. Off by default; at most 50 names are evaluated per request.
    pub fn expose_header(mut self, enabled: bool) -> Self {
        self.expose_header = enabled;
        self
    }
}

impl<S> Layer<S> for FlagsLayer {
    type Service = FlagsMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FlagsMiddleware {
            inner,
            client: self.client.clone(),
            header_name: self.header_name.clone(),
            expose_header: self.expose_header,
        }
    }
}

#[derive(Clone)]
pub struct FlagsMiddleware<S> {
    inner: S,
    client: Arc<Client>,
    header_name: String,
    expose_header: bool,
}

#[pin_project]
pub struct FlagsFuture<F, B> {
    #[pin]
    inner: F,
    client: Arc<Client>,
    header_name: String,
    flags_future: Option<BoxFuture<'static, Result<Vec<String>, FlagError>>>,
    enabled_flags: Vec<String>,
    _phantom: std::marker::PhantomData<B>,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for FlagsMiddleware<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: http_body::Body + Send + 'static,
    ResBody::Error: std::error::Error + Send + Sync + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = FlagsFuture<S::Future, ResBody>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        let flags_from_header = if self.expose_header {
            req.headers()
                .get(&self.header_name)
                .and_then(|v| v.to_str().ok())
                .map(|s| {
                    s.split(',')
                        .map(|f| f.trim())
                        .filter(|f| !f.is_empty())
                        .take(MAX_HEADER_FLAGS)
                        .map(|f| f.to_string())
                        .collect::<Vec<_>>()
                })
        } else {
            None
        };

        let client = self.client.clone();
        let flags_future = if let Some(flags) = flags_from_header {
            let fut = async move {
                let mut enabled_flags = Vec::new();
                for flag in flags {
                    if client.is(&flag).enabled().await {
                        enabled_flags.push(flag);
                    }
                }
                Ok(enabled_flags)
            };
            Some(Box::pin(fut) as BoxFuture<'static, Result<Vec<String>, FlagError>>)
        } else {
            None
        };

        req.extensions_mut().insert(FlagsState {
            client: self.client.clone(),
        });

        let inner = self.inner.call(req);

        FlagsFuture {
            inner,
            client: self.client.clone(),
            header_name: self.header_name.clone(),
            flags_future,
            enabled_flags: Vec::new(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<F, ResBody, E> Future for FlagsFuture<F, ResBody>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
    ResBody: http_body::Body,
{
    type Output = Result<Response<ResBody>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // Resolve the requested flags first, keeping the result so it survives
        // the inner service returning Pending on later polls
        if let Some(flags_future) = this.flags_future.as_mut() {
            match flags_future.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(enabled_flags)) => *this.enabled_flags = enabled_flags,
                // Continue without flags on error
                Poll::Ready(Err(_)) => {}
            }
            *this.flags_future = None;
        }

        match this.inner.poll(cx) {
            Poll::Ready(Ok(mut response)) => {
                if !this.enabled_flags.is_empty() {
                    // Only add header if we can create a valid HeaderValue
                    if let Ok(header_value) = this.enabled_flags.join(",").parse() {
                        response.headers_mut().insert("X-Enabled-Flags", header_value);
                    }
                }
                Poll::Ready(Ok(response))
            }
            other => other,
        }
    }
}

#[derive(Clone)]
pub struct FlagsState {
    pub client: Arc<Client>,
}

pub trait RequestExt {
    fn flags_client(&self) -> Option<&Client>;
}

impl<T> RequestExt for Request<T> {
    fn flags_client(&self) -> Option<&Client> {
        self.extensions()
            .get::<FlagsState>()
            .map(|state| state.client.as_ref())
    }
}