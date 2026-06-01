use std::time::Duration;

use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use crate::error::RelayError;

pub const MAX_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RelayClaims {
    pub sub: SubClaim,
    #[serde(rename = "interviewId")]
    pub interview_id: i64,
    #[serde(default)]
    pub iat: Option<i64>,
    pub exp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubClaim {
    Str(String),
    Int(i64),
}

impl SubClaim {
    pub fn as_key_segment(&self) -> String {
        match self {
            SubClaim::Str(s) => s.clone(),
            SubClaim::Int(n) => n.to_string(),
        }
    }
}

pub fn bearer_token(headers: &HeaderMap) -> Result<String, RelayError> {
    let h = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(RelayError::Auth)?;
    let rest = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
        .ok_or(RelayError::Auth)?;
    if rest.is_empty() {
        return Err(RelayError::Auth);
    }
    Ok(rest.to_string())
}

pub fn decode_claims(token: &str, key: &DecodingKey) -> Result<RelayClaims, RelayError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 5;
    let token_data = jsonwebtoken::decode::<RelayClaims>(token, key, &validation)?;
    Ok(token_data.claims)
}

pub fn validate_claims(claims: &RelayClaims, interview_id: i64) -> Result<(), RelayError> {
    if claims.interview_id != interview_id {
        return Err(RelayError::IdMismatch);
    }
    if let Some(iat) = claims.iat {
        let ttl = claims.exp.saturating_sub(iat);
        if ttl > MAX_TOKEN_TTL.as_secs() as i64 {
            return Err(RelayError::TtlTooLong);
        }
    }
    Ok(())
}
