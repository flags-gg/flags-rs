#[cfg(feature = "tower-middleware")]
use flags_rs::{Auth, Client, middleware::{FlagsLayer, RequestExt}};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use std::convert::Infallible;
use tower::{ServiceBuilder, ServiceExt};
use bytes::Bytes;

#[cfg(not(feature = "tower-middleware"))]
fn main() {
    eprintln!("This example requires the 'tower-middleware' feature. Run with:");
    eprintln!("cargo run --example tower_middleware --features tower-middleware");
}

#[cfg(feature = "tower-middleware")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let auth = Auth {
        project_id: std::env::var("FLAGS_PROJECT_ID").unwrap_or_else(|_| "test-project".to_string()),
        agent_id: std::env::var("FLAGS_AGENT_ID").unwrap_or_else(|_| "test-agent".to_string()),
        environment_id: std::env::var("FLAGS_ENVIRONMENT_ID").unwrap_or_else(|_| "development".to_string()),
    };

    let client = Client::builder()
        .with_auth(auth)
        .build();

    let flags_layer = FlagsLayer::new(client)
        .with_header_name("X-Feature-Flags");

    let service = ServiceBuilder::new()
        .layer(flags_layer)
        .service_fn(handle_request);

    let request = Request::builder()
        .uri("/")
        .header("X-Feature-Flags", "feature-1,feature-2,feature-3")
        .body(Empty::<Bytes>::new())?;

    let response = service.oneshot(request).await?;
    
    println!("Response status: {}", response.status());
    if let Some(enabled_flags) = response.headers().get("X-Enabled-Flags") {
        println!("Enabled flags: {:?}", enabled_flags);
    }

    let auth2 = Auth {
        project_id: std::env::var("FLAGS_PROJECT_ID").unwrap_or_else(|_| "test-project".to_string()),
        agent_id: std::env::var("FLAGS_AGENT_ID").unwrap_or_else(|_| "test-agent".to_string()),
        environment_id: std::env::var("FLAGS_ENVIRONMENT_ID").unwrap_or_else(|_| "development".to_string()),
    };
    
    let service = ServiceBuilder::new()
        .layer(FlagsLayer::new(Client::builder().with_auth(auth2).build()))
        .service_fn(handle_request_with_flag_check);

    let request = Request::builder()
        .uri("/protected")
        .body(Empty::<Bytes>::new())?;

    let response = service.oneshot(request).await?;
    println!("\nProtected endpoint response: {}", response.status());
    
    let body_bytes = response.into_body().collect().await?.to_bytes();
    let body_str = String::from_utf8_lossy(&body_bytes);
    println!("Response body: {}", body_str);

    Ok(())
}

#[cfg(feature = "tower-middleware")]
async fn handle_request<B>(_req: Request<B>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Full::new("Hello from the service!".into()))
        .unwrap())
}

#[cfg(feature = "tower-middleware")]
async fn handle_request_with_flag_check<B>(req: Request<B>) -> Result<Response<Full<Bytes>>, Infallible> {
    if let Some(client) = req.flags_client() {
        if client.is("premium-features").enabled().await {
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Full::new("Premium features are enabled!".into()))
                .unwrap())
        } else {
            Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Full::new("Premium features are not enabled".into()))
                .unwrap())
        }
    } else {
        Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new("Flags client not available".into()))
            .unwrap())
    }
}