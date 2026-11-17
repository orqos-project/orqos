//! Bearer-token checks shared by the privileged control APIs (host and workload).
use axum::http::{header, HeaderMap};

/// True when `headers` carry `Authorization: Bearer <token>`. Secrets have a fixed public length, so every byte
/// is compared regardless of where a difference occurs.
pub fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    let Some(candidate) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    candidate.len() == token.len()
        && candidate
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

/// Reads a control token from the environment, requiring at least 32 bytes.
pub fn token_from_env(variable: &str) -> anyhow::Result<String> {
    let token = std::env::var(variable)
        .map_err(|_| anyhow::anyhow!("Set {variable} to a random secret of at least 32 bytes"))?;
    anyhow::ensure!(
        token.len() >= 32,
        "{variable} must contain at least 32 bytes"
    );
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn only_the_exact_bearer_token_matches() {
        let token = "a".repeat(32);
        assert!(bearer_matches(&headers(&format!("Bearer {token}")), &token));
        assert!(!bearer_matches(
            &headers(&format!("Bearer {token}x")),
            &token
        ));
        assert!(!bearer_matches(
            &headers(&format!("bearer {token}")),
            &token
        ));
        assert!(!bearer_matches(&headers(&"b".repeat(32)), &token));
        assert!(!bearer_matches(&HeaderMap::new(), &token));
    }
}
