use serde_json::Value;
use std::net::{IpAddr, ToSocketAddrs};

#[derive(Debug, thiserror::Error)]
pub enum McpValidationError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("SSRF Protection: Blocked target host '{0}' (cloud metadata or private network)")]
    SsrfBlocked(String),
    #[error("Schema validation error: {0}")]
    SchemaMismatch(String),
}

pub struct EndpointValidator {
    pub allow_localhost: bool,
}

impl Default for EndpointValidator {
    fn default() -> Self {
        Self {
            allow_localhost: true, // Developers frequently run local MCP servers on localhost
        }
    }
}

impl EndpointValidator {
    pub fn new(allow_localhost: bool) -> Self {
        Self { allow_localhost }
    }

    fn is_ip_blocked(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => {
                if ipv4.is_link_local() || ipv4.octets() == [169, 254, 169, 254] {
                    return true;
                }
                if ipv4.is_loopback() && !self.allow_localhost {
                    return true;
                }
            }
            IpAddr::V6(ipv6) => {
                if ipv6.is_loopback() && !self.allow_localhost {
                    return true;
                }
            }
        }
        false
    }

    /// Validates an MCP remote endpoint to protect against Server-Side Request Forgery (SSRF).
    pub fn validate_url(&self, raw_url: &str) -> Result<url::Url, McpValidationError> {
        let parsed =
            url::Url::parse(raw_url).map_err(|e| McpValidationError::InvalidUrl(e.to_string()))?;

        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(McpValidationError::InvalidUrl(format!(
                "Unsupported scheme '{scheme}'. Only http and https are permitted."
            )));
        }

        let host_str = parsed.host_str().ok_or_else(|| {
            McpValidationError::InvalidUrl("Missing host in endpoint URL".to_string())
        })?;

        // 1. Block Cloud Metadata service hostname patterns
        if host_str == "169.254.169.254"
            || host_str.eq_ignore_ascii_case("metadata.google.internal")
            || host_str.eq_ignore_ascii_case("instance-data")
        {
            return Err(McpValidationError::SsrfBlocked(host_str.to_string()));
        }

        // 2. Parse direct IP address if provided
        if let Ok(ip) = host_str.parse::<IpAddr>() {
            if self.is_ip_blocked(ip) {
                return Err(McpValidationError::SsrfBlocked(host_str.to_string()));
            }
        } else {
            // Check for localhost alias
            if (host_str.eq_ignore_ascii_case("localhost") || host_str.ends_with(".localhost"))
                && !self.allow_localhost
            {
                return Err(McpValidationError::SsrfBlocked(host_str.to_string()));
            }

            // 3. DNS resolution validation (defend against DNS rebinding to metadata or blocked IPs)
            let port = parsed.port_or_known_default().unwrap_or(80);
            let addr_str = format!("{host_str}:{port}");
            if let Ok(addrs) = addr_str.to_socket_addrs() {
                for socket_addr in addrs {
                    if self.is_ip_blocked(socket_addr.ip()) {
                        return Err(McpValidationError::SsrfBlocked(format!(
                            "{host_str} resolved to blocked IP {}",
                            socket_addr.ip()
                        )));
                    }
                }
            }
        }

        Ok(parsed)
    }

    /// Validates an MCP remote endpoint and resolves its IP address, returning the validated URL
    /// along with the pinned socket address to eliminate Time-of-Check / Time-of-Use (TOCTOU) DNS rebinding attacks.
    pub fn validate_and_pin_endpoint(
        &self,
        raw_url: &str,
    ) -> Result<(url::Url, Option<std::net::SocketAddr>), McpValidationError> {
        let parsed = self.validate_url(raw_url)?;
        let host_str = parsed.host_str().ok_or_else(|| {
            McpValidationError::InvalidUrl("Missing host in endpoint URL".to_string())
        })?;

        // 1. Direct IP address
        if let Ok(ip) = host_str.parse::<IpAddr>() {
            let port = parsed.port_or_known_default().unwrap_or(80);
            return Ok((parsed, Some(std::net::SocketAddr::new(ip, port))));
        }

        // 2. Domain name: resolve via DNS and pin the verified address
        let port = parsed.port_or_known_default().unwrap_or(80);
        let addr_str = format!("{host_str}:{port}");
        if let Ok(addrs) = addr_str.to_socket_addrs() {
            let addrs_vec: Vec<std::net::SocketAddr> = addrs.collect();
            if let Some(&first_addr) = addrs_vec.first() {
                // Ensure none of the resolved IPs are blocked
                for socket_addr in &addrs_vec {
                    if self.is_ip_blocked(socket_addr.ip()) {
                        return Err(McpValidationError::SsrfBlocked(format!(
                            "{host_str} resolved to blocked IP {}",
                            socket_addr.ip()
                        )));
                    }
                }
                return Ok((parsed, Some(first_addr)));
            }
        }

        Ok((parsed, None))
    }

    /// Validates tool input against its declared JSON schema (required fields and property types).
    pub fn validate_tool_arguments(
        schema: &Value,
        arguments: &Value,
    ) -> Result<(), McpValidationError> {
        // Validate required fields
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for req in required {
                if let Some(field_name) = req.as_str() {
                    if arguments.get(field_name).is_none() {
                        return Err(McpValidationError::SchemaMismatch(format!(
                            "Missing required argument: '{field_name}'"
                        )));
                    }
                }
            }
        }

        // Validate property types if specified
        if let (Some(properties), Some(args_obj)) = (
            schema.get("properties").and_then(|p| p.as_object()),
            arguments.as_object(),
        ) {
            for (arg_key, arg_val) in args_obj {
                if let Some(prop_schema) = properties.get(arg_key) {
                    if let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) {
                        let type_valid = match expected_type {
                            "string" => arg_val.is_string(),
                            "number" => arg_val.is_number(),
                            "integer" => arg_val.is_i64() || arg_val.is_u64(),
                            "boolean" => arg_val.is_boolean(),
                            "array" => arg_val.is_array(),
                            "object" => arg_val.is_object(),
                            _ => true, // custom or unconstrained types pass
                        };
                        if !type_valid {
                            return Err(McpValidationError::SchemaMismatch(format!(
                                "Argument '{arg_key}' expected type '{expected_type}', found {arg_val}"
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssrf_blocks_cloud_metadata() {
        let validator = EndpointValidator::default();
        let err = validator.validate_url("http://169.254.169.254/latest/meta-data");
        assert!(err.is_err());
        assert!(matches!(
            err.unwrap_err(),
            McpValidationError::SsrfBlocked(_)
        ));
    }

    #[test]
    fn test_ssrf_disallows_loopback_when_configured() {
        let validator = EndpointValidator::new(false);
        let err = validator.validate_url("http://127.0.0.1:8080/mcp");
        assert!(err.is_err());
        assert!(matches!(
            err.unwrap_err(),
            McpValidationError::SsrfBlocked(_)
        ));
    }

    #[test]
    fn test_ssrf_allows_localhost_when_enabled() {
        let validator = EndpointValidator::new(true);
        let res = validator.validate_url("http://localhost:3000/mcp");
        assert!(res.is_ok());
    }

    #[test]
    fn test_tool_schema_validation() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["query", "limit"]
        });

        let valid_args = serde_json::json!({
            "query": "Rust",
            "limit": 10
        });
        assert!(EndpointValidator::validate_tool_arguments(&schema, &valid_args).is_ok());

        let invalid_args = serde_json::json!({
            "query": "Rust"
        });
        assert!(EndpointValidator::validate_tool_arguments(&schema, &invalid_args).is_err());

        // Type mismatch test
        let schema_typed = serde_json::json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer" },
                "enabled": { "type": "boolean" }
            }
        });
        let invalid_type_args = serde_json::json!({
            "count": "not a number",
            "enabled": true
        });
        assert!(
            EndpointValidator::validate_tool_arguments(&schema_typed, &invalid_type_args).is_err()
        );

        let valid_type_args = serde_json::json!({
            "count": 42,
            "enabled": true
        });
        assert!(
            EndpointValidator::validate_tool_arguments(&schema_typed, &valid_type_args).is_ok()
        );
    }
}
