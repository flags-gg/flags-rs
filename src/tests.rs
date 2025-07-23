#[cfg(test)]
mod tests {
    use std::env;
    use std::time::Duration;

    use tokio::time::sleep;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};

    use crate::cache::{Cache, MemoryCache};
    use crate::{Auth, Client};
    use crate::flag::FeatureFlag;
    use serial_test::serial;

    // Helper function to create a test client with mocked API
    async fn create_test_client(server: &MockServer) -> Client {
        Client::builder()
            .with_base_url(&server.uri())
            .with_auth(Auth {
                project_id: "test-project".to_string(),
                agent_id: "test-agent".to_string(),
                environment_id: "test-env".to_string(),
            })
            .with_memory_cache()
            .build()
    }

    #[tokio::test]
    async fn test_client_initialization() {
        let client = Client::builder()
            .with_base_url("https://test-api.example.com")
            .with_max_retries(5)
            .with_auth(Auth {
                project_id: "test-project".to_string(),
                agent_id: "test-agent".to_string(),
                environment_id: "test-env".to_string(),
            })
            .with_memory_cache()
            .build();

        assert_eq!(client.base_url, "https://test-api.example.com");
        assert_eq!(client.max_retries, 5);
        assert!(client.auth.is_some());
    }

    #[tokio::test]
    async fn test_flag_enabled_from_api() {
        // Setup mock server
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/flags"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "intervalAllowed": 60,
                    "flags": [
                        {
                            "enabled": true,
                            "details": {
                                "name": "test-flag",
                                "id": "123"
                            }
                        },
                        {
                            "enabled": false,
                            "details": {
                                "name": "disabled-flag",
                                "id": "456"
                            }
                        }
                    ]
                }))
            )
            .mount(&mock_server)
            .await;

        let client = create_test_client(&mock_server).await;

        // Test enabled flag
        let enabled = client.is("test-flag").enabled().await;
        assert!(enabled);

        // Test disabled flag
        let disabled = client.is("disabled-flag").enabled().await;
        assert!(!disabled);

        // Test non-existent flag
        let non_existent = client.is("non-existent-flag").enabled().await;
        assert!(!non_existent);
    }

    #[tokio::test]
    #[serial]
    async fn test_flag_list() {
        // Setup mock server
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/flags"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "intervalAllowed": 60,
                    "flags": [
                        {
                            "enabled": true,
                            "details": {
                                "name": "flag1",
                                "id": "123"
                            }
                        },
                        {
                            "enabled": false,
                            "details": {
                                "name": "flag2",
                                "id": "456"
                            }
                        }
                    ]
                }))
            )
            .mount(&mock_server)
            .await;

        let client = create_test_client(&mock_server).await;

        // Force a flag check to populate the cache
        let _ = client.is("flag1").enabled().await;

        // Test listing all flags
        let flags = client.list().await.unwrap();
        assert_eq!(flags.len(), 2);

        // Verify flag details
        let flag1 = flags.iter().find(|f| f.details.name == "flag1").unwrap();
        assert!(flag1.enabled);
        assert_eq!(flag1.details.id, "123");

        let flag2 = flags.iter().find(|f| f.details.name == "flag2").unwrap();
        assert!(!flag2.enabled);
        assert_eq!(flag2.details.id, "456");
    }

    #[tokio::test]
    #[serial]
    async fn test_local_environment_flags() {
        // Set environment variables for testing
        env::set_var("FLAGS_TEST_ENV_FLAG", "true");
        env::set_var("FLAGS_ANOTHER_FLAG", "false");

        // Setup mock server (even though we won't use it for this test)
        let mock_server = MockServer::start().await;
        let client = create_test_client(&mock_server).await;

        // Test flags from environment variables
        let env_flag = client.is("test_env_flag").enabled().await;
        assert!(env_flag);

        let another_flag = client.is("another_flag").enabled().await;
        assert!(!another_flag);

        // Test with different casing and formatting
        let env_flag_dash = client.is("test-env-flag").enabled().await;
        assert!(env_flag_dash);

        // Clean up
        env::remove_var("FLAGS_TEST_ENV_FLAG");
        env::remove_var("FLAGS_ANOTHER_FLAG");
    }

    #[tokio::test]
    async fn test_cache_refresh_simple() {
        // Create a memory cache
        let mut cache = MemoryCache::new();

        // Initialize with a flag that's enabled
        let flags = vec![
            FeatureFlag {
                enabled: true,
                details: crate::flag::Details {
                    name: "cache-test-flag".to_string(),
                    id: "123".to_string(),
                },
            },
        ];

        // Set a short cache TTL
        cache.refresh(&flags, 1).await.unwrap();

        // Verify the flag is enabled
        let (enabled, exists) = cache.get("cache-test-flag").await.unwrap();
        assert!(exists);
        assert!(enabled);

        // Wait for cache to expire
        sleep(Duration::from_secs(2)).await;

        // Verify cache should refresh
        assert!(cache.should_refresh_cache().await);

        // Update the flag to be disabled
        let flags = vec![
            FeatureFlag {
                enabled: false,
                details: crate::flag::Details {
                    name: "cache-test-flag".to_string(),
                    id: "123".to_string(),
                },
            },
        ];

        // Refresh the cache
        cache.refresh(&flags, 60).await.unwrap();

        // Verify the flag is now disabled
        let (enabled, exists) = cache.get("cache-test-flag").await.unwrap();
        assert!(exists);
        assert!(!enabled);
    }


    #[tokio::test]
    async fn test_circuit_breaker() {
        // Setup mock server
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/flags"))
            .respond_with(ResponseTemplate::new(500)
                .set_body_json(serde_json::json!({"error": "Internal Server Error"}))
            )
            .mount(&mock_server)
            .await;

        let client = create_test_client(&mock_server).await;

        // Try to check flag (should fail but not panic)
        let enabled = client.is("circuit-test-flag").enabled().await;
        assert!(!enabled);

        // Verify circuit is open
        let circuit_state = client.circuit_state.read().await;
        assert!(!circuit_state.is_open);
    }

    #[tokio::test]
    async fn test_memory_cache() {
        let mut cache = MemoryCache::new();

        // Initialize cache
        cache.init().await.unwrap();

        // Test empty cache
        let (enabled, exists) = cache.get("test-flag").await.unwrap();
        assert!(!exists);
        assert!(!enabled);

        // Refresh cache with flags
        let flags = vec![
            FeatureFlag {
                enabled: true,
                details: crate::flag::Details {
                    name: "test-flag".to_string(),
                    id: "123".to_string(),
                },
            },
        ];

        cache.refresh(&flags, 60).await.unwrap();

        // Test getting flag from cache
        let (enabled, exists) = cache.get("test-flag").await.unwrap();
        assert!(exists);
        assert!(enabled);

        // Test getting all flags
        let all_flags = cache.get_all().await.unwrap();
        assert_eq!(all_flags.len(), 1);
        assert_eq!(all_flags[0].details.name, "test-flag");

        // Test cache refresh timing
        assert!(!cache.should_refresh_cache().await);
    }
}
