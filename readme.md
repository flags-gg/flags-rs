# Flags.gg Rust Library

```rust
use flags_rs::{Auth, Client};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize the client
    let client = Client::builder()
        .with_auth(Auth {
            project_id: "your-project-id".to_string(),
            agent_id: "your-agent-id".to_string(),
            environment_id: "your-environment-id".to_string(),
        })
        .with_memory_cache()
        .build()?;

    // Check if a flag is enabled
    let is_feature_enabled = client.is("my-feature").enabled().await;
    println!("Feature 'my-feature' is enabled: {}", is_feature_enabled);

    // List all flags
    let all_flags = client.list().await?;
    println!("All flags:");
    for flag in all_flags {
        println!("  {} ({}): {}", flag.details.name, flag.details.id, flag.enabled);
    }

    Ok(())
}
```

## Tower middleware

With the `tower-middleware` feature, `FlagsLayer` puts the client into each request's
extensions so handlers can check flags via `req.flags_client()`:

```rust,ignore
use flags_rs::middleware::{FlagsLayer, RequestExt};

let service = tower::ServiceBuilder::new()
    .layer(FlagsLayer::new(client))
    .service_fn(handler);
```

The layer can also read flag names from a request header (`X-Feature-Flags` by default,
change it with `.with_header_name(..)`) and echo the enabled ones back in an
`X-Enabled-Flags` response header. This is **off by default**, since it lets any caller
probe your flag states. Enable it only for trusted clients with `.expose_header(true)`;
at most 50 names are evaluated per request.

The base URL must use `https` (plain `http` is only accepted for `localhost`,
`127.0.0.1` and `[::1]`).