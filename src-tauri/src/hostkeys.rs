use russh::{
    client,
    keys::{HashAlg, PublicKeyOrCertificate},
};
use serde::Serialize;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct PendingKey {
    pub host: String,
    pub port: u16,
    pub line: String,
    pub fetched: Instant,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKey {
    pub token: String,
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
}
struct Capture(Arc<Mutex<Option<(String, String, String)>>>);
impl client::Handler for Capture {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if let PublicKeyOrCertificate::PublicKey { key, .. } = key {
            if let Ok(public) = key.to_openssh() {
                *self.0.lock().unwrap() = Some((
                    key.algorithm().to_string(),
                    key.fingerprint(HashAlg::Sha256).to_string(),
                    public,
                ));
            }
        }
        // Always reject: this is an untrusted candidate, never an authenticated session.
        Ok(false)
    }
}
pub fn validate_endpoint(host: &str, port: u16) -> Result<(), String> {
    if host.is_empty()
        || host.len() > 253
        || port == 0
        || host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || ",[]*?!|/#\\".contains(c))
    {
        return Err(
            "Enter a hostname or IP address and a valid SSH port first (IPv6 without brackets)"
                .into(),
        );
    }
    Ok(())
}
pub async fn fetch(host: String, port: u16) -> Result<(HostKey, PendingKey), String> {
    validate_endpoint(&host, port)?;
    let captured = Arc::new(Mutex::new(None));
    let mut config = client::Config {
        inactivity_timeout: Some(Duration::from_secs(10)),
        ..Default::default()
    };
    config
        .preferred
        .key
        .to_mut()
        .retain(|algorithm| !matches!(algorithm, russh::keys::Algorithm::Rsa { hash: None }));
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        client::connect(
            Arc::new(config),
            (host.as_str(), port),
            Capture(captured.clone()),
        ),
    )
    .await;
    // A rejected key is the expected outcome. No authentication calls are made.
    let (algorithm, fingerprint, public) = captured.lock().unwrap().take().ok_or_else(|| {
        if result.is_err() { "Host-key lookup timed out. Check the host, port and network." }
        else { "Could not read a supported SSH host key. Check the host, port and SSH server configuration." }.to_owned()
    })?;
    let name = if port == 22 {
        host.clone()
    } else {
        format!("[{host}]:{port}")
    };
    let pending = PendingKey {
        host: host.clone(),
        port,
        line: format!("{name} {public}\n"),
        fetched: Instant::now(),
    };
    Ok((
        HostKey {
            token: uuid::Uuid::new_v4().to_string(),
            host,
            port,
            algorithm,
            fingerprint,
        },
        pending,
    ))
}
impl PendingKey {
    pub fn verified_line(&self, host: &str, port: u16, verified: bool) -> Result<String, String> {
        if !verified {
            return Err("Compare the fingerprint with your provider or administrator and confirm the match first".into());
        }
        if self.host != host || self.port != port {
            return Err("The host or port changed. Fetch and verify its key again".into());
        }
        if self.fetched.elapsed() > Duration::from_secs(600) {
            return Err("The host-key lookup expired. Fetch and verify the key again".into());
        }
        Ok(self.line.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loopback_lookup_returns_public_key_without_authentication() {
        use russh::{
            keys::{ssh_key::private::Ed25519Keypair, PrivateKey},
            server,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct RejectAuth(Arc<AtomicUsize>);
        impl server::Handler for RejectAuth {
            type Error = russh::Error;
            async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(server::Auth::reject())
            }
            async fn auth_password(
                &mut self,
                _: &str,
                _: &str,
            ) -> Result<server::Auth, Self::Error> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(server::Auth::reject())
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let key = PrivateKey::from(Ed25519Keypair::from_seed(&[7; 32]));
            let expected = key.public_key().fingerprint(HashAlg::Sha256).to_string();
            let public = key.public_key().to_openssh().unwrap();
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let auth_count = Arc::new(AtomicUsize::new(0));
            let counter = auth_count.clone();
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let config = server::Config {
                    keys: vec![key],
                    ..Default::default()
                };
                if let Ok(session) =
                    server::run_stream(Arc::new(config), stream, RejectAuth(counter)).await
                {
                    let _ = session.await;
                }
            });
            let (candidate, pending) = fetch("127.0.0.1".into(), port).await.unwrap();
            assert_eq!(candidate.algorithm, "ssh-ed25519");
            assert_eq!(candidate.fingerprint, expected);
            assert_eq!(pending.line, format!("[127.0.0.1]:{port} {public}\n"));
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(auth_count.load(Ordering::SeqCst), 0);
        });
    }
    #[test]
    fn candidate_is_bound_to_verified_endpoint_and_expires() {
        let mut pending = PendingKey {
            host: "example.invalid".into(),
            port: 2222,
            line: "fixture".into(),
            fetched: Instant::now(),
        };
        assert!(pending
            .verified_line("example.invalid", 2222, false)
            .is_err());
        assert!(pending.verified_line("other.invalid", 2222, true).is_err());
        assert!(pending.verified_line("example.invalid", 22, true).is_err());
        assert_eq!(
            pending
                .verified_line("example.invalid", 2222, true)
                .unwrap(),
            "fixture"
        );
        pending.fetched = Instant::now() - Duration::from_secs(601);
        assert!(pending
            .verified_line("example.invalid", 2222, true)
            .is_err());
        for host in ["", "host,other", "[::1]", "*.example", "host\nother"] {
            assert!(validate_endpoint(host, 22).is_err());
        }
        assert!(validate_endpoint("::1", 2222).is_ok());
    }
}
