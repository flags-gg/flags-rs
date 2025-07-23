use crate::{Client, FlagError};
use futures::future::BoxFuture;
use http::{Request, Response};
use http_body_util::BodyExt;
use pin_project::pin_project;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

#[derive(Clone)]
pub struct FlagsLayer {
    client: Arc<Client>,
    header_name: String,
}

impl FlagsLayer {
    pub fn new(client: Client) -> Self {
        Self {
            client: Arc::new(client),
            header_name: "X-Feature-Flags".to_string(),
        }
    }

    pub fn with_header_name(mut self, name: impl Into<String>) -> Self {
        self.header_name = name.into();
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
        }
    }
}

#[derive(Clone)]
pub struct FlagsMiddleware<S> {
    inner: S,
    client: Arc<Client>,
    header_name: String,
}

#[pin_project]
pub struct FlagsFuture<F, B> {
    #[pin]
    inner: F,
    client: Arc<Client>,
    header_name: String,
    flags_future: Option<BoxFuture<'static, Result<Vec<String>, FlagError>>>,
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
        let flags_from_header = req
            .headers()
            .get(&self.header_name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(',').map(|f| f.trim().to_string()).collect::<Vec<_>>());

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

        // If we have flags to check, we need to wait for them first
        if let Some(flags_future) = this.flags_future.as_mut() {
            match flags_future.as_mut().poll(cx) {
                Poll::Ready(Ok(enabled_flags)) => {
                    *this.flags_future = None;
                    
                    // Now poll the inner service
                    match this.inner.poll(cx) {
                        Poll::Ready(Ok(mut response)) => {
                            if !enabled_flags.is_empty() {
                                response.headers_mut().insert(
                                    "X-Enabled-Flags",
                                    enabled_flags.join(",").parse().unwrap(),
                                );
                            }
                            Poll::Ready(Ok(response))
                        }
                        Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                        Poll::Pending => Poll::Pending,
                    }
                }
                Poll::Ready(Err(_)) => {
                    *this.flags_future = None;
                    // Continue without flags on error
                    this.inner.poll(cx)
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            this.inner.poll(cx)
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