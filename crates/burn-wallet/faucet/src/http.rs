//! The HTTP surface: `POST /drip` and `GET /status`, nothing else (no
//! admin endpoints). Requests are handled one at a time, which also makes
//! the check-then-record sequence in [`Faucet::drip`] race-free.

use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::config::FaucetConfig;
use crate::faucet::{DripError, Faucet, StatusReport};
use crate::node::Node;
use crate::state::ip_key;

/// Largest request body read for `/drip` (`{"address": "..."}` is < 150 B).
const MAX_BODY_BYTES: u64 = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DripBody {
    address: String,
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Which client a request is from, for the per-IP cooldown. The proxy
/// header is believed only when configured AND the socket peer is one of
/// `trusted_proxy_peers`; otherwise the socket peer is the client.
pub(crate) fn client_ip(
    cfg: &FaucetConfig,
    peer: Option<IpAddr>,
    header_value: Option<&str>,
) -> Option<IpAddr> {
    if cfg.trusted_proxy_header.is_some()
        && let Some(p) = peer
        && cfg.trusted_proxy_peers.contains(&p)
        && let Some(v) = header_value
    {
        // A single IP (CF-Connecting-IP), or a list where the last entry
        // is the one our trusted proxy appended (X-Forwarded-For).
        if let Some(ip) = v.rsplit(',').next().and_then(|s| s.trim().parse().ok()) {
            return Some(ip);
        }
    }
    peer
}

fn json_response(status: u16, body: &serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_data(body.to_string().into_bytes()).with_status_code(status);
    if let Ok(h) = Header::from_bytes("Content-Type", "application/json") {
        resp.add_header(h);
    }
    if let Ok(h) = Header::from_bytes("Cache-Control", "no-store") {
        resp.add_header(h);
    }
    resp
}

fn error_response(
    status: u16,
    code: &str,
    message: &str,
    retry_after: Option<u64>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut body = json!({ "error": code, "message": message });
    if let Some(secs) = retry_after {
        body["retry_after_secs"] = json!(secs);
    }
    let mut resp = json_response(status, &body);
    if let Some(secs) = retry_after
        && let Ok(h) = Header::from_bytes("Retry-After", secs.to_string())
    {
        resp.add_header(h);
    }
    resp
}

fn drip_error_response(e: &DripError) -> Response<std::io::Cursor<Vec<u8>>> {
    use crate::state::LimitRejection as L;
    let msg = e.to_string();
    match e {
        DripError::InvalidAddress(_) => error_response(400, "invalid_address", &msg, None),
        DripError::Limit(l) => {
            let code = match l {
                L::AddressCooldown { .. } => "address_cooldown",
                L::IpCooldown { .. } => "ip_cooldown",
                L::DailyCapReached { .. } => "daily_cap_reached",
            };
            error_response(429, code, &msg, Some(l.retry_after_secs()))
        }
        DripError::Busy => error_response(503, "busy", &msg, Some(75)),
        DripError::Maturing => error_response(503, "maturing", &msg, Some(600)),
        DripError::CoinbaseMustBeShielded => error_response(503, "coinbase_unshielded", &msg, None),
        DripError::InsufficientFunds => error_response(503, "faucet_empty", &msg, None),
        DripError::Halted => error_response(503, "paused", &msg, None),
        DripError::Node(_) | DripError::Rejected(_) | DripError::Build(_) => {
            error_response(502, "node_error", &msg, None)
        }
    }
}

/// Serves until the process is killed.
pub(crate) fn serve<N: Node>(
    cfg: &FaucetConfig,
    faucet: &mut Faucet<N>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = Server::http(cfg.listen)?;
    println!("sova-faucet listening on http://{}", cfg.listen);
    let mut status_cache: Option<(Instant, StatusReport)> = None;
    for request in server.incoming_requests() {
        let response = handle(cfg, faucet, &mut status_cache, request);
        if let Err(e) = response {
            eprintln!("warning: failed to answer a request: {e}");
        }
    }
    Ok(())
}

fn handle<N: Node>(
    cfg: &FaucetConfig,
    faucet: &mut Faucet<N>,
    status_cache: &mut Option<(Instant, StatusReport)>,
    mut request: Request,
) -> std::io::Result<()> {
    let path = request.url().split('?').next().unwrap_or("").to_string();
    let method = request.method().clone();
    match (method, path.as_str()) {
        (Method::Get, "/status") => {
            let fresh = status_cache
                .as_ref()
                .is_some_and(|(t, _)| t.elapsed().as_secs() < cfg.status_cache_secs);
            if !fresh {
                match faucet.status(unix_now()) {
                    Ok(s) => *status_cache = Some((Instant::now(), s)),
                    Err(e) => {
                        eprintln!("status: zebrad error: {e}");
                        return request.respond(error_response(
                            502,
                            "node_error",
                            "zcash node error",
                            None,
                        ));
                    }
                }
            }
            let body = status_cache
                .as_ref()
                .map_or(serde_json::Value::Null, |(_, s)| json!(s));
            request.respond(json_response(200, &body))
        }
        (Method::Post, "/drip") => {
            let peer = request.remote_addr().map(SocketAddr::ip);
            let header_value = cfg.trusted_proxy_header.as_ref().and_then(|name| {
                request
                    .headers()
                    .iter()
                    .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
                    .map(|h| h.value.as_str().to_string())
            });
            let Some(ip) = client_ip(cfg, peer, header_value.as_deref()) else {
                return request.respond(error_response(
                    400,
                    "no_client_ip",
                    "cannot determine client address",
                    None,
                ));
            };
            let mut body = Vec::new();
            if request
                .as_reader()
                .take(MAX_BODY_BYTES + 1)
                .read_to_end(&mut body)
                .is_err()
                || body.len() as u64 > MAX_BODY_BYTES
            {
                return request.respond(error_response(
                    413,
                    "bad_request",
                    "body must be {\"address\": \"<t-addr>\"} (max 1 KiB)",
                    None,
                ));
            }
            let Ok(parsed) = serde_json::from_slice::<DripBody>(&body) else {
                return request.respond(error_response(
                    400,
                    "bad_request",
                    "body must be {\"address\": \"<t-addr>\"}",
                    None,
                ));
            };
            let result = faucet.drip(&parsed.address, &ip_key(ip), unix_now());
            // A drip changes the balance and budget: drop the cached status.
            *status_cache = None;
            match result {
                Ok(receipt) => request.respond(json_response(200, &json!(receipt))),
                Err(e) => {
                    if let DripError::Node(inner) = &e {
                        eprintln!("drip: zebrad error: {inner}");
                    }
                    request.respond(drip_error_response(&e))
                }
            }
        }
        (_, "/status" | "/drip") => request.respond(error_response(
            405,
            "method_not_allowed",
            "use GET /status or POST /drip",
            None,
        )),
        _ => request.respond(error_response(
            404,
            "not_found",
            "endpoints: GET /status, POST /drip",
            None,
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cfg(extra: &str) -> FaucetConfig {
        FaucetConfig::parse(&format!(
            "network = \"test\"\nzebrad_rpc = \"http://127.0.0.1:1\"\nkeystore = \"/k\"\nstate_file = \"/s\"\n{extra}"
        ))
        .unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn proxy_header_ignored_unless_configured() {
        let c = cfg("");
        assert_eq!(
            client_ip(&c, Some(ip("127.0.0.1")), Some("9.9.9.9")),
            Some(ip("127.0.0.1"))
        );
    }

    #[test]
    fn proxy_header_believed_only_from_trusted_peers() {
        let c = cfg("trusted_proxy_header = \"CF-Connecting-IP\"\n");
        // From the tunnel on loopback: the header is the client.
        assert_eq!(
            client_ip(&c, Some(ip("127.0.0.1")), Some("9.9.9.9")),
            Some(ip("9.9.9.9"))
        );
        // From anyone else: the header is attacker-controlled; ignore it.
        assert_eq!(
            client_ip(&c, Some(ip("203.0.113.5")), Some("9.9.9.9")),
            Some(ip("203.0.113.5"))
        );
        // Trusted peer, but the header is missing or garbage: fall back to
        // the peer (all such requests then share one cooldown -- fail safe).
        assert_eq!(
            client_ip(&c, Some(ip("127.0.0.1")), None),
            Some(ip("127.0.0.1"))
        );
        assert_eq!(
            client_ip(&c, Some(ip("127.0.0.1")), Some("not-an-ip")),
            Some(ip("127.0.0.1"))
        );
        // X-Forwarded-For style list: the last hop is the one our proxy added.
        assert_eq!(
            client_ip(&c, Some(ip("127.0.0.1")), Some("1.1.1.1, 8.8.4.4")),
            Some(ip("8.8.4.4"))
        );
    }
}
