use url::Url;

use crate::error::RemoteControlError;

pub fn session_ingress_ws_url(
    api_base_url: &str,
    session_id: &str,
) -> Result<Url, RemoteControlError> {
    let base = Url::parse(api_base_url)?;
    let is_local = base
        .host_str()
        .map(|host| host == "localhost" || host == "127.0.0.1" || host == "::1")
        .unwrap_or(false);
    let scheme = if is_local { "ws" } else { "wss" };
    let version = if is_local { "v2" } else { "v1" };
    let host = base
        .host_str()
        .ok_or_else(|| RemoteControlError::Protocol("api base URL has no host".to_string()))?;
    let port = base
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(Url::parse(&format!(
        "{scheme}://{host}{port}/{version}/session_ingress/ws/{session_id}"
    ))?)
}

pub fn claude_ai_origin(session_id: &str, ingress_url: &str) -> Url {
    if session_id.contains("_local_") || ingress_url.contains("localhost") {
        Url::parse("http://localhost:4000").unwrap()
    } else if session_id.contains("_staging_") || ingress_url.contains("staging") {
        Url::parse("https://claude-ai.staging.ant.dev").unwrap()
    } else {
        Url::parse("https://claude.ai").unwrap()
    }
}

pub fn session_url(
    session_id: &str,
    ingress_url: &str,
    query: Option<&str>,
) -> Result<Url, RemoteControlError> {
    let mut url = claude_ai_origin(session_id, ingress_url).join(&format!("code/{session_id}"))?;
    if let Some(query) = query {
        url.set_query(Some(query));
    }
    Ok(url)
}

pub fn environment_connect_url(
    environment_id: &str,
    ingress_url: &str,
) -> Result<Url, RemoteControlError> {
    let mut origin = if ingress_url.contains("localhost") {
        Url::parse("http://localhost:4000").unwrap()
    } else if ingress_url.contains("staging") {
        Url::parse("https://claude-ai.staging.ant.dev").unwrap()
    } else {
        Url::parse("https://claude.ai").unwrap()
    };
    origin.set_path("code");
    origin.set_query(Some(&format!("environment={environment_id}")));
    Ok(origin)
}

pub fn worker_events_stream_url(session_base_url: &str) -> Result<Url, RemoteControlError> {
    let mut url = Url::parse(session_base_url)?;
    match url.scheme() {
        "ws" => {
            let _ = url.set_scheme("http");
        }
        "wss" => {
            let _ = url.set_scheme("https");
        }
        _ => {}
    }
    let mut path = url.path().trim_end_matches('/').to_string();
    path.push_str("/worker/events/stream");
    url.set_path(&path);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ws_ingress_urls_like_claude() {
        assert_eq!(
            session_ingress_ws_url("http://localhost:8000", "sess")
                .unwrap()
                .as_str(),
            "ws://localhost:8000/v2/session_ingress/ws/sess"
        );
        assert_eq!(
            session_ingress_ws_url("https://api.anthropic.com", "sess")
                .unwrap()
                .as_str(),
            "wss://api.anthropic.com/v1/session_ingress/ws/sess"
        );
    }

    #[test]
    fn builds_connect_urls() {
        assert_eq!(
            environment_connect_url("env_1", "https://api.anthropic.com")
                .unwrap()
                .as_str(),
            "https://claude.ai/code?environment=env_1"
        );
        assert_eq!(
            session_url("abc_staging_def", "https://staging.example", None)
                .unwrap()
                .as_str(),
            "https://claude-ai.staging.ant.dev/code/abc_staging_def"
        );
    }
}
