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
        .build();

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
