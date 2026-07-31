use std::collections::BTreeMap;

pub const MAX_HEADER_BYTES: usize = 8 * 1024;
pub const MAX_BODY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub content_length: usize,
}

impl HttpRequest {
    pub fn token(&self) -> Option<&str> {
        if let Some(bearer) = self.headers.get("authorization") {
            return bearer
                .strip_prefix("Bearer ")
                .or_else(|| bearer.strip_prefix("bearer "))
                .map(str::trim);
        }
        self.headers.get("x-jumabek-token").map(|t| t.trim())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Bad {
    NotHttp,
    HeadersTooLarge,
    BodyTooLarge,
    NoLength,
}

impl Bad {
    pub fn status(&self) -> u16 {
        match self {
            Bad::NotHttp | Bad::NoLength => 400,
            Bad::HeadersTooLarge | Bad::BodyTooLarge => 413,
        }
    }

    pub fn why(&self) -> &'static str {
        match self {
            Bad::NotHttp => "not a readable HTTP request",
            Bad::HeadersTooLarge => "headers are too large",
            Bad::BodyTooLarge => "body is too large",
            Bad::NoLength => "Content-Length is required",
        }
    }
}

/// Parses only what this door needs. Anything unusual is refused rather than
/// interpreted — the surface is two routes on loopback, not a web server.
pub fn parse_head(head: &str) -> Result<HttpRequest, Bad> {
    if head.len() > MAX_HEADER_BYTES {
        return Err(Bad::HeadersTooLarge);
    }

    let mut lines = head.split("\r\n");
    let start = lines.next().ok_or(Bad::NotHttp)?;

    let mut parts = start.split_whitespace();
    let method = parts.next().ok_or(Bad::NotHttp)?.to_uppercase();
    let path = parts.next().ok_or(Bad::NotHttp)?.to_string();
    let version = parts.next().unwrap_or("");

    if !version.starts_with("HTTP/") {
        return Err(Bad::NotHttp);
    }

    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(Bad::NotHttp);
        };
        headers.insert(name.trim().to_lowercase(), value.trim().to_string());
    }

    let content_length = match headers.get("content-length") {
        Some(raw) => raw.parse::<usize>().map_err(|_| Bad::NoLength)?,
        None if method == "POST" => return Err(Bad::NoLength),
        None => 0,
    };

    if content_length > MAX_BODY_BYTES {
        return Err(Bad::BodyTooLarge);
    }

    Ok(HttpRequest {
        method,
        path: path.split('?').next().unwrap_or("/").to_string(),
        headers,
        content_length,
    })
}

pub fn response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "Error",
    };

    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        body.len(),
        body
    )
}

pub fn json_message(key: &str, value: &str) -> String {
    serde_json::json!({ key: value }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(extra: &str) -> String {
        format!(
            "POST /notify HTTP/1.1\r\nHost: localhost\r\nContent-Length: 12\r\n{}",
            extra
        )
    }

    #[test]
    fn a_plain_post_is_read() {
        let parsed = parse_head(&head("")).unwrap();

        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/notify");
        assert_eq!(parsed.content_length, 12);
    }

    #[test]
    fn a_bearer_token_is_found() {
        let parsed = parse_head(&head("Authorization: Bearer secret-token\r\n")).unwrap();
        assert_eq!(parsed.token(), Some("secret-token"));
    }

    #[test]
    fn the_plain_header_works_too() {
        let parsed = parse_head(&head("X-Jumabek-Token: secret-token\r\n")).unwrap();
        assert_eq!(parsed.token(), Some("secret-token"));
    }

    #[test]
    fn header_names_are_matched_whatever_the_case() {
        let parsed = parse_head(&head("AUTHORIZATION: Bearer abc\r\n")).unwrap();
        assert_eq!(parsed.token(), Some("abc"));
    }

    #[test]
    fn a_query_string_does_not_become_part_of_the_route() {
        let parsed = parse_head("POST /notify?x=1 HTTP/1.1\r\nContent-Length: 0\r\n").unwrap();
        assert_eq!(parsed.path, "/notify");
    }

    #[test]
    fn a_post_without_a_length_is_refused() {
        assert_eq!(parse_head("POST /notify HTTP/1.1\r\n"), Err(Bad::NoLength));
    }

    #[test]
    fn an_enormous_body_is_refused_before_it_is_read() {
        let head = format!(
            "POST /a HTTP/1.1\r\nContent-Length: {}\r\n",
            MAX_BODY_BYTES + 1
        );
        assert_eq!(parse_head(&head), Err(Bad::BodyTooLarge));
    }

    #[test]
    fn something_that_is_not_http_is_refused() {
        assert_eq!(parse_head("hello there"), Err(Bad::NotHttp));
        assert_eq!(parse_head(""), Err(Bad::NotHttp));
        assert_eq!(parse_head("GET /"), Err(Bad::NotHttp));
    }

    #[test]
    fn a_broken_header_line_is_refused_not_ignored() {
        let head = "POST /a HTTP/1.1\r\nContent-Length: 0\r\nnonsense\r\n";
        assert_eq!(parse_head(head), Err(Bad::NotHttp));
    }

    #[test]
    fn a_get_needs_no_length() {
        let parsed = parse_head("GET /health HTTP/1.1\r\n").unwrap();
        assert_eq!(parsed.content_length, 0);
    }

    #[test]
    fn the_response_carries_a_byte_length_not_a_character_count() {
        let body = json_message("error", "не найдено");
        let out = response(404, &body);

        let declared: usize = out
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        assert_eq!(declared, body.len());
        assert!(
            declared > body.chars().count(),
            "cyrillic was counted as one byte"
        );
    }
}
