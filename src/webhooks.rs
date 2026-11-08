//! One cancellable outbound HTTPS worker. It never owns or opens the installation database.
use crate::{
    Error, Result,
    alerts::{AlertDispatch, AlertReceipt, AlertResult},
    config::{AlertConfig, SecretRef},
    contracts::AlertErrorCode,
};
use reqwest::{
    Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER},
};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::watch;

fn invalid() -> Error {
    Error::new(
        "CONFIG_INVALID",
        "Outbound alerts require one bounded HTTPS destination and an exact safe address policy",
    )
}
fn endpoint(config: &AlertConfig) -> Result<Url> {
    if config.endpoint.is_empty() || config.endpoint.len() > 2048 {
        return Err(invalid());
    }
    let url = Url::parse(&config.endpoint).map_err(|_| invalid())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default().is_none_or(|p| p == 0)
    {
        return Err(invalid());
    }
    Ok(url)
}
fn literal_host(url: &Url) -> Option<IpAddr> {
    url.host_str().and_then(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .ok()
    })
}
fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
    }
}
/// Conservative unicast policy. Special-purpose ranges cannot be allowlisted into public addresses.
fn forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a == 0
                || a >= 224
                || ip.is_link_local()
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113)
                || ip.is_broadcast()
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            !ip.is_loopback()
                && (ip.is_unspecified() || ip.is_multicast() || ip.to_ipv4_mapped().is_some() ||
                (segments[0]&0xffc0)==0xfe80 ||
                // Public unicast is 2000::/3; exclude special-use blocks inside that allocation.
                (!private(IpAddr::V6(ip)) && ((segments[0]&0xe000)!=0x2000 ||
                  (segments[0]==0x2001 && segments[1]<0x0200) ||
                  (segments[0]==0x2001 && segments[1]==0x0db8) ||
                  (segments[0]==0x2002) || (segments[0]==0x3fff && segments[1]<0x1000))))
        }
    }
}
fn allowed(ip: IpAddr, approved: &[IpAddr]) -> bool {
    !forbidden(ip) && (!private(ip) || approved.contains(&ip))
}
pub fn validate_config(config: &AlertConfig) -> Result<()> {
    let url = endpoint(config)?;
    if config.allowed_private_addresses.len() > 16 {
        return Err(invalid());
    }
    let mut unique = std::collections::BTreeSet::new();
    for &ip in &config.allowed_private_addresses {
        if !private(ip) || forbidden(ip) || !unique.insert(ip) {
            return Err(invalid());
        }
    }
    if literal_host(&url).is_some_and(|ip| !allowed(ip, &config.allowed_private_addresses)) {
        return Err(invalid());
    }
    match &config.token_ref {
        SecretRef::Env { name }
            if name.is_empty()
                || name.len() > 256
                || !name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                }) =>
        {
            return Err(invalid());
        }
        SecretRef::File { path } if path.is_empty() || path.len() > 4096 || path.contains('\0') => {
            return Err(invalid());
        }
        _ => {}
    }
    if config
        .ca_file
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty() || path.as_os_str().len() > 4096)
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn destination_hash(config: &AlertConfig) -> Result<String> {
    validate_config(config)?;
    let mut hash = Sha256::new();
    hash.update(b"CronHarbor outbound alert destination v1\0");
    hash.update(endpoint(config)?.as_str().as_bytes());
    hash.update([0]);
    let mut addresses = config.allowed_private_addresses.clone();
    addresses.sort();
    for address in addresses {
        hash.update(address.to_string().as_bytes());
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn read_regular(
    path: &Path,
    limit: u64,
    secret: bool,
) -> std::result::Result<Vec<u8>, AlertErrorCode> {
    let code = if secret {
        AlertErrorCode::CredentialUnavailable
    } else {
        AlertErrorCode::TlsUnavailable
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| code)?;
    let metadata = file.metadata().map_err(|_| code)?;
    let current = nix::unistd::geteuid().as_raw();
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() > limit
        || (secret && (metadata.uid() != current || metadata.mode() & 0o077 != 0))
        || (!secret
            && ((metadata.uid() != 0 && metadata.uid() != current) || metadata.mode() & 0o022 != 0))
    {
        return Err(code);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| code)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(code);
    }
    Ok(bytes)
}
fn credential(reference: &SecretRef) -> std::result::Result<HeaderValue, AlertErrorCode> {
    let mut bytes = match reference {
        SecretRef::Env { name } => std::env::var(name)
            .map_err(|_| AlertErrorCode::CredentialUnavailable)?
            .into_bytes(),
        SecretRef::File { path } => {
            let mut bytes = read_regular(Path::new(path), 514, true)?;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            bytes
        }
    };
    if bytes.is_empty() || bytes.len() > 512 || !bytes.iter().all(u8::is_ascii_graphic) {
        return Err(AlertErrorCode::CredentialUnavailable);
    }
    let mut value = Vec::with_capacity(bytes.len() + 7);
    value.extend_from_slice(b"Bearer ");
    value.append(&mut bytes);
    let mut header =
        HeaderValue::from_bytes(&value).map_err(|_| AlertErrorCode::CredentialUnavailable)?;
    header.set_sensitive(true);
    Ok(header)
}
fn ca(config: &AlertConfig) -> std::result::Result<Vec<reqwest::Certificate>, AlertErrorCode> {
    match &config.ca_file {
        None => Ok(Vec::new()),
        Some(path) => {
            let bytes = read_regular(path, 1024 * 1024, false)?;
            let certificates = reqwest::Certificate::from_pem_bundle(&bytes)
                .map_err(|_| AlertErrorCode::TlsUnavailable)?;
            if certificates.is_empty() {
                return Err(AlertErrorCode::TlsUnavailable);
            }
            Ok(certificates)
        }
    }
}

type ResolveFn = Arc<dyn Fn(String, u16) -> std::io::Result<Vec<SocketAddr>> + Send + Sync>;
// OS lookups cannot be cancelled. Keep the lease process-wide, including across worker replacement.
static DNS_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
struct DnsPermit;
impl Drop for DnsPermit {
    fn drop(&mut self) {
        DNS_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    }
}
struct PendingResolution {
    receiver: Receiver<std::io::Result<Vec<SocketAddr>>>,
}
struct ResolverLease {
    resolver: ResolveFn,
    pending: Option<PendingResolution>,
}
impl ResolverLease {
    fn new() -> Self {
        Self {
            resolver: Arc::new(|host, port| {
                Ok((host.as_str(), port).to_socket_addrs()?.take(17).collect())
            }),
            pending: None,
        }
    }
    async fn resolve(
        &mut self,
        host: &str,
        port: u16,
    ) -> std::result::Result<Vec<SocketAddr>, AlertErrorCode> {
        if let Some(previous) = self.pending.take() {
            match previous.receiver.try_recv() {
                Err(TryRecvError::Empty) => {
                    self.pending = Some(previous);
                    return Err(AlertErrorCode::DnsUnavailable);
                }
                Ok(_) | Err(TryRecvError::Disconnected) => {} // A timed-out lease's late answer is never used.
            }
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let resolve = Arc::clone(&self.resolver);
        let host = host.to_owned();
        if DNS_ACTIVE
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(AlertErrorCode::DnsUnavailable);
        }
        let spawned = thread::Builder::new()
            .name("alert-dns".into())
            .spawn(move || {
                let _permit = DnsPermit;
                let _ = sender.send(resolve(host, port));
            });
        if spawned.is_err() {
            DNS_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
            return Err(AlertErrorCode::DnsUnavailable);
        }
        self.pending = Some(PendingResolution { receiver });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let received = self
                .pending
                .as_ref()
                .ok_or(AlertErrorCode::DnsUnavailable)?
                .receiver
                .try_recv();
            match received {
                Ok(Ok(addresses)) => {
                    self.pending = None;
                    return Ok(addresses);
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    return Err(AlertErrorCode::DnsUnavailable);
                }
                Err(TryRecvError::Empty) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AlertErrorCode::DnsUnavailable);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
fn validate_addresses(
    addresses: &[SocketAddr],
    approved: &[IpAddr],
) -> std::result::Result<(), AlertErrorCode> {
    if addresses.is_empty()
        || addresses.len() > 16
        || addresses
            .iter()
            .any(|address| !allowed(address.ip(), approved))
    {
        return Err(AlertErrorCode::InvalidDestination);
    }
    Ok(())
}
fn tls_error(error: &reqwest::Error) -> bool {
    let mut current = std::error::Error::source(error);
    while let Some(source) = current {
        if source.downcast_ref::<rustls::Error>().is_some() {
            return true;
        }
        if source.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput
            )
        }) {
            return true;
        }
        if let Some(io) = source.downcast_ref::<std::io::Error>()
            && let Some(inner) = io.get_ref()
        {
            current = Some(inner);
        } else {
            current = source.source();
        }
    }
    false
}
fn retry_after(value: Option<&HeaderValue>) -> Option<u32> {
    let text = value?.to_str().ok()?;
    if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(
        text.parse::<u64>()
            .unwrap_or(u64::MAX)
            .min(3600)
            .try_into()
            .unwrap_or(3600),
    )
}
fn http_receipt(status: u16, retry_after_seconds: Option<u32>) -> AlertReceipt {
    if !(100..=599).contains(&status) {
        return AlertReceipt::Failed {
            code: AlertErrorCode::HttpRejected,
            http_status: None,
        };
    }
    if (200..300).contains(&status) {
        AlertReceipt::Accepted {
            http_status: status,
        }
    } else if matches!(status, 408 | 425 | 429 | 500..=599) {
        AlertReceipt::Retry {
            code: AlertErrorCode::HttpRejected,
            http_status: Some(status),
            retry_after_seconds,
        }
    } else {
        AlertReceipt::Failed {
            code: AlertErrorCode::HttpRejected,
            http_status: Some(status),
        }
    }
}
async fn attempt(
    config: &AlertConfig,
    url: &Url,
    job: &AlertDispatch,
    resolver: &mut ResolverLease,
) -> AlertReceipt {
    let token = match credential(&config.token_ref) {
        Ok(token) => token,
        Err(code) => return AlertReceipt::Blocked { code },
    };
    let certificates = match ca(config) {
        Ok(ca) => ca,
        Err(code) => return AlertReceipt::Blocked { code },
    };
    let Some(host) = url.host_str() else {
        return AlertReceipt::Blocked {
            code: AlertErrorCode::InvalidDestination,
        };
    };
    let Some(port) = url.port_or_known_default() else {
        return AlertReceipt::Blocked {
            code: AlertErrorCode::InvalidDestination,
        };
    };
    let addresses = if let Some(ip) = literal_host(url) {
        vec![SocketAddr::new(ip, port)]
    } else {
        match resolver.resolve(host, port).await {
            Ok(addresses) => addresses,
            Err(code) => {
                return AlertReceipt::Retry {
                    code,
                    http_status: None,
                    retry_after_seconds: None,
                };
            }
        }
    };
    if let Err(code) = validate_addresses(&addresses, &config.allowed_private_addresses) {
        return AlertReceipt::Blocked { code };
    }
    // Hyper 1.12 limits incomplete headers at 417792 bytes; spare capacity permits completed overshoot.
    // Pinned bytes/Vec growth keeps its backing buffer below 2 MiB; TLS tests reject 600000-byte headers.
    let builder = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("CronHarbor/1")
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .http1_only()
        .http1_max_headers(64)
        .pool_max_idle_per_host(0)
        .tls_certs_merge(certificates)
        .resolve_to_addrs(host, &addresses);
    let client = match builder.build() {
        Ok(client) => client,
        Err(_) => {
            return AlertReceipt::Blocked {
                code: AlertErrorCode::TlsUnavailable,
            };
        }
    };
    let response = client
        .post(url.clone())
        .header(AUTHORIZATION, token)
        .header(CONTENT_TYPE, "application/json")
        .header("Idempotency-Key", &job.alert_id)
        .body(job.payload_json.clone())
        .send()
        .await;
    match response {
        Ok(response) => http_receipt(
            response.status().as_u16(),
            retry_after(response.headers().get(RETRY_AFTER)),
        ),
        Err(error) if error.is_timeout() => AlertReceipt::Retry {
            code: AlertErrorCode::RequestTimeout,
            http_status: None,
            retry_after_seconds: None,
        },
        Err(error) if tls_error(&error) => AlertReceipt::Blocked {
            code: AlertErrorCode::TlsUnavailable,
        },
        Err(_) => AlertReceipt::Retry {
            code: AlertErrorCode::TransportUnavailable,
            http_status: None,
            retry_after_seconds: None,
        },
    }
}

pub struct WebhookWorker {
    requests: SyncSender<AlertDispatch>,
    results: Receiver<AlertResult>,
    stop: watch::Sender<bool>,
    task: Option<thread::JoinHandle<()>>,
    busy: bool,
}
impl WebhookWorker {
    pub fn spawn(config: AlertConfig) -> Result<Self> {
        validate_config(&config)?;
        let url = endpoint(&config)?;
        let destination = destination_hash(&config)?;
        let (requests, receiver) = mpsc::sync_channel::<AlertDispatch>(1);
        let (sender, results) = mpsc::sync_channel(1);
        let (stop, mut stopped) = watch::channel(false);
        let task=thread::Builder::new().name("alert-webhook".into()).spawn(move || {
            let Ok(runtime)=tokio::runtime::Builder::new_current_thread().enable_all().build() else{return};
            runtime.block_on(async move {
                let mut resolver=ResolverLease::new();
                loop {
                    if *stopped.borrow(){break}
                    let job=match receiver.try_recv() {
                        Ok(job)=>job,
                        Err(TryRecvError::Disconnected)=>break,
                        Err(TryRecvError::Empty)=>{
                            tokio::select!{ _=stopped.changed()=>{},_=tokio::time::sleep(Duration::from_millis(10))=>{} }
                            continue;
                        }
                    };
                    if job.destination_hash!=destination {break}
                    let receipt=tokio::select!{
                        _=stopped.changed()=>break,
                        result=tokio::time::timeout(Duration::from_secs(10),attempt(&config,&url,&job,&mut resolver))=>result.unwrap_or(AlertReceipt::Retry{code:AlertErrorCode::RequestTimeout,http_status:None,retry_after_seconds:None}),
                    };
                    let result=AlertResult{alert_id:job.alert_id,nonce:job.nonce,destination_hash:job.destination_hash,receipt};
                    if sender.try_send(result).is_err(){break}
                }
            });
            runtime.shutdown_timeout(Duration::from_millis(100));
        }).map_err(|_|Error::new("WORKER_UNAVAILABLE","Cannot start outbound alert worker"))?;
        Ok(Self {
            requests,
            results,
            stop,
            task: Some(task),
            busy: false,
        })
    }
    pub fn try_send(&mut self, job: AlertDispatch) -> Result<()> {
        if self.busy || !self.available() {
            return Err(Error::new(
                "WORKER_UNAVAILABLE",
                "Outbound alert worker is unavailable",
            ));
        }
        self.requests.try_send(job).map_err(|_| {
            Error::new("WORKER_UNAVAILABLE", "Outbound alert worker is unavailable")
        })?;
        self.busy = true;
        Ok(())
    }
    pub fn try_result(&mut self) -> Option<AlertResult> {
        let result = self.results.try_recv().ok();
        if result.is_some() {
            self.busy = false;
        }
        result
    }
    pub fn available(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished()) && !*self.stop.borrow()
    }
    pub fn idle(&self) -> bool {
        !self.busy && self.available()
    }
    pub fn shutdown(&mut self) {
        let _ = self.stop.send(true);
        let deadline = Instant::now() + Duration::from_millis(500);
        while self.task.as_ref().is_some_and(|task| !task.is_finished())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        if self
            .task
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
            && let Some(task) = self.task.take()
        {
            let _ = task.join();
        }
    }
}
impl Drop for WebhookWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_dns_answer_is_checked_and_retry_after_parser_is_strict() {
        let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
        let private: SocketAddr = "127.0.0.1:443".parse().unwrap();
        assert!(validate_addresses(&[public], &[]).is_ok());
        assert!(validate_addresses(&[public, private], &[]).is_err());
        assert!(validate_addresses(&vec![public; 17], &[]).is_err());
        assert!(validate_addresses(&[], &[]).is_err());
        for value in ["-1", " 1", "1 ", "1.5", "Thu, 01 Jan 2026 00:00:00 GMT"] {
            assert_eq!(
                retry_after(Some(&HeaderValue::from_str(value).unwrap())),
                None
            );
        }
        assert_eq!(
            retry_after(Some(&HeaderValue::from_static("99999999999999999999"))),
            Some(3600)
        );
    }
    #[test]
    fn stuck_resolver_lease_is_global_and_late_answers_never_start_another_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (release_sender, release_receiver) = mpsc::sync_channel::<()>(1);
        let release = Arc::new(std::sync::Mutex::new(release_receiver));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_resolver = Arc::clone(&calls);
        let fake: ResolveFn = Arc::new(move |_, port| {
            calls_for_resolver.fetch_add(1, Ordering::Relaxed);
            release.lock().unwrap().recv().unwrap();
            Ok(vec![SocketAddr::from(([8, 8, 8, 8], port))])
        });
        let mut first = ResolverLease {
            resolver: Arc::clone(&fake),
            pending: None,
        };
        runtime.block_on(async {
            assert!(
                tokio::time::timeout(Duration::from_millis(30), first.resolve("one.example", 443))
                    .await
                    .is_err()
            );
            assert_eq!(
                first.resolve("two.example", 443).await.unwrap_err(),
                AlertErrorCode::DnsUnavailable
            );
            let mut replacement = ResolverLease {
                resolver: Arc::clone(&fake),
                pending: None,
            };
            assert_eq!(
                replacement
                    .resolve("replacement.example", 443)
                    .await
                    .unwrap_err(),
                AlertErrorCode::DnsUnavailable
            );
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        });
        drop(first);
        release_sender.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while DNS_ACTIVE.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let mut next = ResolverLease {
            resolver: Arc::new(|_, port| Ok(vec![SocketAddr::from(([8, 8, 4, 4], port))])),
            pending: None,
        };
        assert_eq!(
            runtime
                .block_on(next.resolve("fresh.example", 443))
                .unwrap()[0]
                .ip(),
            IpAddr::from([8, 8, 4, 4])
        );
    }
}
