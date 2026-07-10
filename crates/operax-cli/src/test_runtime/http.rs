use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ParsedHttpUrl {
    pub base: String,
    pub host: String,
    pub port: u16,
}

impl ParsedHttpUrl {
    pub fn parse(input: &str, label: &str) -> Result<Self, String> {
        let base = input.trim().trim_end_matches('/').to_string();
        let rest = base
            .strip_prefix("http://")
            .ok_or_else(|| format!("{label} must look like http://host:port"))?;
        let host_port = rest
            .split('/')
            .next()
            .ok_or_else(|| format!("{label} must look like http://host:port"))?;
        let (host, port) = host_port
            .rsplit_once(':')
            .ok_or_else(|| format!("{label} must include a port"))?;
        let port = port
            .parse::<u16>()
            .map_err(|_| format!("{label} has an invalid port"))?;
        if host.trim().is_empty() {
            return Err(format!("{label} must include a host"));
        }
        let host = host.to_string();
        Ok(Self { base, host, port })
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

pub fn parse_http_json_response(bytes: &[u8]) -> Result<Value, String> {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "HTTP response did not include headers".to_string())?;
    let headers = String::from_utf8_lossy(&bytes[..split]);
    let status_line = headers
        .lines()
        .next()
        .ok_or_else(|| "HTTP response was empty".to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("invalid HTTP status line: {status_line}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}: {status_line}"));
    }
    serde_json::from_slice(&bytes[split + 4..])
        .map_err(|err| format!("HTTP response body is invalid JSON: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_url() {
        let url = ParsedHttpUrl::parse("http://127.0.0.1:8080", "test").unwrap();
        assert_eq!(url.host, "127.0.0.1");
        assert_eq!(url.port, 8080);
        assert_eq!(url.base, "http://127.0.0.1:8080");
    }

    #[test]
    fn parse_url_with_trailing_slash() {
        let url = ParsedHttpUrl::parse("http://localhost:9090/", "test").unwrap();
        assert_eq!(url.host, "localhost");
        assert_eq!(url.port, 9090);
        assert_eq!(url.base, "http://localhost:9090");
    }

    #[test]
    fn parse_url_with_path() {
        let url = ParsedHttpUrl::parse("http://example.com:443/some/path", "test").unwrap();
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 443);
    }

    #[test]
    fn parse_url_with_whitespace() {
        let url = ParsedHttpUrl::parse("  http://host:1234  ", "test").unwrap();
        assert_eq!(url.host, "host");
        assert_eq!(url.port, 1234);
    }

    #[test]
    fn parse_url_rejects_non_http() {
        let err = ParsedHttpUrl::parse("https://host:443", "myurl").unwrap_err();
        assert!(err.contains("myurl"));
        assert!(err.contains("http://host:port"));
    }

    #[test]
    fn parse_url_rejects_missing_port() {
        let err = ParsedHttpUrl::parse("http://host-only", "myurl").unwrap_err();
        assert!(err.contains("myurl"));
        assert!(err.contains("port"));
    }

    #[test]
    fn parse_url_rejects_invalid_port() {
        let err = ParsedHttpUrl::parse("http://host:notaport", "myurl").unwrap_err();
        assert!(err.contains("invalid port"));
    }

    #[test]
    fn parse_url_rejects_empty_host() {
        let err = ParsedHttpUrl::parse("http://:8080", "myurl").unwrap_err();
        assert!(err.contains("host"));
    }

    #[test]
    fn bind_addr_formats_correctly() {
        let url = ParsedHttpUrl::parse("http://0.0.0.0:3000", "test").unwrap();
        assert_eq!(url.bind_addr(), "0.0.0.0:3000");
    }

    #[test]
    fn parse_response_extracts_json_body() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
        let value = parse_http_json_response(response).unwrap();
        assert_eq!(value, serde_json::json!({"ok": true}));
    }

    #[test]
    fn parse_response_rejects_missing_headers() {
        let err = parse_http_json_response(b"no headers here").unwrap_err();
        assert!(err.contains("did not include headers"));
    }

    #[test]
    fn parse_response_rejects_empty_status_line() {
        let err = parse_http_json_response(b"\r\n\r\n{}").unwrap_err();
        assert!(err.contains("HTTP response was empty"));
    }

    #[test]
    fn parse_response_rejects_invalid_status_line() {
        let err = parse_http_json_response(b"INVALID\r\n\r\n{}").unwrap_err();
        assert!(err.contains("invalid HTTP status line"));
    }

    #[test]
    fn parse_response_rejects_error_status() {
        let err = parse_http_json_response(b"HTTP/1.1 404 Not Found\r\n\r\n{}").unwrap_err();
        assert!(err.contains("HTTP 404"));
    }

    #[test]
    fn parse_response_rejects_server_error() {
        let err =
            parse_http_json_response(b"HTTP/1.1 500 Internal Server Error\r\n\r\n{}").unwrap_err();
        assert!(err.contains("HTTP 500"));
    }

    #[test]
    fn parse_response_rejects_invalid_json() {
        let err = parse_http_json_response(b"HTTP/1.1 200 OK\r\n\r\nnot json").unwrap_err();
        assert!(err.contains("invalid JSON"));
    }

    #[test]
    fn parse_response_status_299_accepted() {
        let response = b"HTTP/1.1 299 Custom\r\n\r\n{\"a\":1}";
        let value = parse_http_json_response(response).unwrap();
        assert_eq!(value, serde_json::json!({"a": 1}));
    }

    #[test]
    fn parse_response_status_300_rejected() {
        let err = parse_http_json_response(b"HTTP/1.1 300 Redirect\r\n\r\n{}").unwrap_err();
        assert!(err.contains("HTTP 300"));
    }
}
