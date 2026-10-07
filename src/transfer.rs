use crate::{Error, Result, backend::validate_name};
use rand::{RngCore, rngs::OsRng};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Clone)]
pub struct Ticket {
    pub direction: Direction,
    pub image: String,
    pub size: Option<u64>,
    expires: Instant,
}
pub struct Tickets {
    entries: Mutex<HashMap<[u8; 32], Ticket>>,
    max_bytes: u64,
}

#[derive(Serialize)]
pub struct Grant {
    pub url: String,
    pub method: &'static str,
    pub expires_at_unix: u64,
    pub content_length: Option<u64>,
    pub single_use: bool,
    pub ticket_id: String,
}

impl Tickets {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_bytes,
        }
    }
    pub fn mint(
        &self,
        origin: &url::Url,
        direction: Direction,
        image: String,
        size: Option<u64>,
        ttl: u64,
    ) -> Result<Grant> {
        validate_name(&image)?;
        if !(10..=3600).contains(&ttl) {
            return Err(Error::Invalid("expiry must be 10..3600 seconds"));
        }
        match direction {
            Direction::Upload if !size.is_some_and(|s| s > 0 && s <= self.max_bytes) => {
                return Err(Error::Invalid(
                    "upload requires a positive exact size within configured maximum",
                ));
            }
            Direction::Download if size.is_some() => {
                return Err(Error::Invalid("download does not accept an upload size"));
            }
            _ => (),
        }
        let mut entries = self.entries.lock().map_err(|_| Error::Busy)?;
        entries.retain(|_, t| t.expires > Instant::now());
        if entries.len() >= 128 {
            return Err(Error::Busy);
        }
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let token = hex(&random);
        let key = digest(&token);
        entries.insert(
            key,
            Ticket {
                direction,
                image,
                size,
                expires: Instant::now() + Duration::from_secs(ttl),
            },
        );
        Ok(Grant {
            url: origin
                .join(&format!("transfer/{token}"))
                .map_err(|_| Error::Protocol)?
                .to_string(),
            method: if direction == Direction::Upload {
                "PUT"
            } else {
                "GET"
            },
            expires_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Error::Protocol)?
                .as_secs()
                + ttl,
            content_length: size,
            single_use: true,
            ticket_id: hex(&key),
        })
    }
    /// Validation occurs before removal; wrong methods/lengths don't consume the grant.
    pub fn claim(
        &self,
        token: &str,
        direction: Direction,
        content_length: Option<u64>,
    ) -> Result<Ticket> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Ticket);
        }
        let key = digest(token);
        let mut entries = self.entries.lock().map_err(|_| Error::Busy)?;
        entries.retain(|_, t| t.expires > Instant::now());
        let ticket = entries.get(&key).ok_or(Error::Ticket)?;
        if ticket.direction != direction {
            return Err(Error::Invalid("wrong transfer method"));
        }
        if direction == Direction::Upload && ticket.size != content_length {
            return Err(Error::Invalid("Content-Length must match grant exactly"));
        }
        entries.remove(&key).ok_or(Error::Ticket)
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| Error::Busy)?;
        entries.retain(|key, _| hex(key) != id);
        Ok(())
    }
}
fn digest(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn origin() -> url::Url {
        "http://127.0.0.1:9100".parse().unwrap()
    }
    #[test]
    fn grants_are_scoped_single_use_and_revocable() {
        let tickets = Tickets::new(100);
        let grant = tickets
            .mint(&origin(), Direction::Upload, "a.iso".into(), Some(10), 10)
            .unwrap();
        let token = grant.url.rsplit('/').next().unwrap();
        assert!(tickets.claim(token, Direction::Download, None).is_err());
        assert!(tickets.claim(token, Direction::Upload, Some(11)).is_err());
        assert_eq!(
            tickets
                .claim(token, Direction::Upload, Some(10))
                .unwrap()
                .image,
            "a.iso"
        );
        assert!(tickets.claim(token, Direction::Upload, Some(10)).is_err());
        let grant = tickets
            .mint(&origin(), Direction::Download, "a.iso".into(), None, 10)
            .unwrap();
        tickets.revoke(&grant.ticket_id).unwrap();
        assert!(
            tickets
                .claim(
                    grant.url.rsplit('/').next().unwrap(),
                    Direction::Download,
                    None
                )
                .is_err()
        );
        assert!(
            tickets
                .mint(&origin(), Direction::Upload, "a.iso".into(), Some(101), 10)
                .is_err()
        );
    }
    #[test]
    fn expired_ticket_is_rejected() {
        let tickets = Tickets::new(100);
        let grant = tickets
            .mint(&origin(), Direction::Download, "a.iso".into(), None, 10)
            .unwrap();
        tickets
            .entries
            .lock()
            .unwrap()
            .values_mut()
            .next()
            .unwrap()
            .expires = Instant::now();
        assert!(
            tickets
                .claim(
                    grant.url.rsplit('/').next().unwrap(),
                    Direction::Download,
                    None
                )
                .is_err()
        );
    }

    #[test]
    fn concurrent_claim_has_exactly_one_winner() {
        let tickets = std::sync::Arc::new(Tickets::new(100));
        let grant = tickets
            .mint(&origin(), Direction::Download, "a.iso".into(), None, 10)
            .unwrap();
        let token = grant.url.rsplit('/').next().unwrap().to_owned();
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let tickets = tickets.clone();
                let token = token.clone();
                std::thread::spawn(move || tickets.claim(&token, Direction::Download, None).is_ok())
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            1
        );
    }

    #[test]
    fn registry_is_bounded_and_restarts_invalidate_grants() {
        let tickets = Tickets::new(100);
        let grant = tickets
            .mint(&origin(), Direction::Download, "a.iso".into(), None, 10)
            .unwrap();
        for _ in 1..128 {
            tickets
                .mint(&origin(), Direction::Download, "a.iso".into(), None, 10)
                .unwrap();
        }
        assert!(matches!(
            tickets.mint(&origin(), Direction::Download, "a.iso".into(), None, 10),
            Err(Error::Busy)
        ));
        let replacement = Tickets::new(100);
        assert!(
            replacement
                .claim(
                    grant.url.rsplit('/').next().unwrap(),
                    Direction::Download,
                    None
                )
                .is_err()
        );
    }
}
