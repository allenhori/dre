//! DRE destination plugins for object storage: `s3`, `gcs` and `azure_blob`.
//!
//! Each binary streams the local file up in parts, so memory stays flat however large the
//! output is: multipart uploads for S3 and Azure, a resumable JSON-API upload for GCS (which also
//! works against the fake-gcs-server emulator). The remote path is `s3://bucket/key`, `gs://bucket/key` or
//! `az://container/key`; a path without a scheme is a key inside the profile's `bucket` /
//! `container`.
//!
//! Profile output fields:
//!
//! - **s3**: `bucket`, `region`, `access_key_id` + `secret_access_key` (+ `session_token`), or
//!   none of them to use the ambient credential chain (environment, shared config, instance
//!   or container role); `endpoint` and `allow_http` for S3-compatible stores.
//! - **gcs**: `bucket`, `service_account_key_path` (a key file) or `service_account_key` (the
//!   key JSON), or neither for application default credentials; `endpoint` for emulators.
//! - **azure_blob**: `account_name`, `container`, and one of `connection_string`, `sas_token`,
//!   `access_key`, or `use_managed_identity: true`; `endpoint` for emulators.

use std::path::Path;
use std::sync::Arc;

use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{About, Destination, Result, conn_bool, conn_str, serve_destination};
use dre_protocol::util::percent_encode;
use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::azure::{AzureConfigKey, MicrosoftAzureBuilder};
use object_store::buffered::BufWriter;
use object_store::gcp::{GoogleCloudStorageBuilder, GoogleConfigKey};
use object_store::path::Path as ObjectPath;
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    S3,
    Gcs,
    Azure,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::S3 => "s3",
            Kind::Gcs => "gcs",
            Kind::Azure => "azure_blob",
        }
    }
    fn scheme(self) -> &'static str {
        match self {
            Kind::S3 => "s3",
            Kind::Gcs => "gs",
            Kind::Azure => "az",
        }
    }
}

/// Split `scheme://bucket/key` (or a bare key) into bucket and key.
fn locate(kind: Kind, remote: Option<&str>, c: &Map<String, Value>) -> Result<(String, String)> {
    let default_bucket = match kind {
        Kind::Azure => conn_str(c, "container"),
        _ => conn_str(c, "bucket"),
    };
    let remote = remote.ok_or("this destination needs `output.destination.path`")?;
    for scheme in [kind.scheme(), if kind == Kind::Azure { "azure" } else { "" }] {
        if scheme.is_empty() {
            continue;
        }
        if let Some(rest) = remote.strip_prefix(&format!("{scheme}://")) {
            let (bucket, key) = rest
                .split_once('/')
                .ok_or_else(|| format!("`{remote}` has no object key after the bucket"))?;
            return Ok((bucket.to_string(), key.to_string()));
        }
    }
    if remote.contains("://") {
        return Err(format!("`{remote}` isn't a {}:// path", kind.scheme()).into());
    }
    let bucket = default_bucket.ok_or_else(|| {
        format!(
            "`{remote}` has no {}:// prefix and the profile has no `{}`",
            kind.scheme(),
            if kind == Kind::Azure {
                "container"
            } else {
                "bucket"
            }
        )
    })?;
    Ok((bucket.to_string(), remote.trim_start_matches('/').to_string()))
}

fn build(kind: Kind, bucket: &str, c: &Map<String, Value>) -> Result<Arc<dyn ObjectStore>> {
    Ok(match kind {
        Kind::S3 => {
            let explicit = conn_str(c, "access_key_id").is_some();
            // No keys: the ambient chain (environment, shared config, instance/container role).
            let mut b = if explicit {
                AmazonS3Builder::new()
            } else {
                AmazonS3Builder::from_env()
            };
            b = b.with_bucket_name(bucket);
            if let Some(r) = conn_str(c, "region") {
                b = b.with_region(r);
            }
            if explicit {
                b = b
                    .with_access_key_id(conn_str(c, "access_key_id").unwrap())
                    .with_secret_access_key(
                        conn_str(c, "secret_access_key")
                            .ok_or("`access_key_id` needs `secret_access_key`")?,
                    );
                if let Some(t) = conn_str(c, "session_token") {
                    b = b.with_token(t);
                }
            }
            if let Some(e) = conn_str(c, "endpoint") {
                b = b.with_endpoint(e).with_virtual_hosted_style_request(false);
            }
            if conn_bool(c, "allow_http").unwrap_or(false)
                || conn_str(c, "endpoint").is_some_and(|e| e.starts_with("http://"))
            {
                b = b.with_allow_http(true);
            }
            Arc::new(b.build()?)
        }
        Kind::Gcs => {
            let mut b = GoogleCloudStorageBuilder::from_env().with_bucket_name(bucket);
            if let Some(p) = conn_str(c, "service_account_key_path") {
                b = b.with_service_account_path(p);
            } else if let Some(k) = conn_str(c, "service_account_key") {
                b = b.with_service_account_key(k);
            }
            if let Some(e) = conn_str(c, "endpoint") {
                b = b.with_config(GoogleConfigKey::BaseUrl, e);
            }
            Arc::new(b.build()?)
        }
        Kind::Azure => {
            let mut b = MicrosoftAzureBuilder::new().with_container_name(bucket);
            let mut account = conn_str(c, "account_name").map(str::to_string);
            let mut endpoint = conn_str(c, "endpoint").map(str::to_string);
            if let Some(cs) = conn_str(c, "connection_string") {
                let parts: std::collections::HashMap<String, String> = cs
                    .split(';')
                    .filter_map(|kv| {
                        kv.split_once('=')
                            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    })
                    .collect();
                if let Some(a) = parts.get("accountname") {
                    account = Some(a.clone());
                }
                if let Some(k) = parts.get("accountkey") {
                    b = b.with_access_key(k);
                }
                if let Some(sas) = parts.get("sharedaccesssignature") {
                    b = b.with_config(AzureConfigKey::SasKey, sas);
                }
                if let Some(e) = parts.get("blobendpoint") {
                    endpoint = Some(e.clone());
                }
            } else if let Some(sas) = conn_str(c, "sas_token") {
                b = b.with_config(AzureConfigKey::SasKey, sas.trim_start_matches('?'));
            } else if let Some(k) = conn_str(c, "access_key") {
                b = b.with_access_key(k);
            } else if conn_bool(c, "use_managed_identity").unwrap_or(false) {
                b = MicrosoftAzureBuilder::from_env().with_container_name(bucket);
            } else {
                return Err("azure_blob needs `connection_string`, `sas_token`, `access_key` or `use_managed_identity: true`".into());
            }
            let account =
                account.ok_or("azure_blob needs `account_name` (or a connection string with AccountName)")?;
            b = b.with_account(account);
            if let Some(e) = endpoint {
                if e.starts_with("http://") {
                    b = b.with_allow_http(true);
                }
                b = b.with_endpoint(e);
            }
            Arc::new(b.build()?)
        }
    })
}

pub struct ObjectStoreDestination {
    kind: Kind,
}

impl Destination for ObjectStoreDestination {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        match self.kind {
            Kind::S3 => vec![
                ConnectionField::new("bucket", "default bucket (or use s3://bucket/... paths)"),
                ConnectionField::new("region", "AWS region, e.g. ap-southeast-2"),
                ConnectionField::new(
                    "access_key_id",
                    "access key id (leave empty to use the ambient credential chain)",
                )
                .secret(),
                ConnectionField::new("secret_access_key", "secret access key").secret(),
            ],
            Kind::Gcs => vec![
                ConnectionField::new("bucket", "default bucket (or use gs://bucket/... paths)"),
                ConnectionField::new(
                    "service_account_key_path",
                    "service-account key file (empty: application default credentials)",
                ),
            ],
            Kind::Azure => vec![
                ConnectionField::new("account_name", "storage account name").required(),
                ConnectionField::new("container", "default container (or use az://container/... paths)"),
                ConnectionField::new(
                    "connection_string",
                    "connection string (or set sas_token / access_key)",
                )
                .secret(),
            ],
        }
    }

    fn deliver(&mut self, local: &Path, remote: Option<&str>, c: &Map<String, Value>) -> Result<String> {
        let (bucket, key) = locate(self.kind, remote, c)?;
        if self.kind == Kind::Gcs {
            return gcs_upload(&bucket, &key, local, c);
        }
        let store = build(self.kind, &bucket, c)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let location = format!("{}://{bucket}/{key}", self.kind.scheme());
        rt.block_on(async {
            let mut file = tokio::fs::File::open(local)
                .await
                .map_err(|e| format!("can't read {}: {e}", local.display()))?;
            let mut w = BufWriter::with_capacity(store, ObjectPath::from(key.as_str()), 8 * 1024 * 1024);
            if let Err(e) = tokio::io::copy(&mut file, &mut w).await {
                let _ = w.abort().await;
                return Err(format!("upload to {location} failed: {e}"));
            }
            w.shutdown()
                .await
                .map_err(|e| format!("upload to {location} failed: {e}"))
        })?;
        Ok(location)
    }
}

const GCS_CHUNK: usize = 8 * 1024 * 1024;

/// Upload to GCS with the JSON API's resumable protocol, `GCS_CHUNK` bytes at a time.
/// Credentials (service-account key, application default credentials) come from object_store.
fn gcs_upload(bucket: &str, key: &str, local: &Path, c: &Map<String, Value>) -> Result<String> {
    let location = format!("gs://{bucket}/{key}");
    let base = conn_str(c, "endpoint")
        .map(str::to_string)
        .or_else(|| {
            let p = conn_str(c, "service_account_key_path")?;
            let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
            v.get("gcs_base_url")?.as_str().map(str::to_string)
        })
        .unwrap_or_else(|| "https://storage.googleapis.com".to_string());
    let base = base.trim_end_matches('/');
    let mut b = GoogleCloudStorageBuilder::from_env().with_bucket_name(bucket);
    if let Some(p) = conn_str(c, "service_account_key_path") {
        b = b.with_service_account_path(p);
    } else if let Some(k) = conn_str(c, "service_account_key") {
        b = b.with_service_account_key(k);
    }
    let store = b.build()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let token = rt
        .block_on(async { store.credentials().get_credential().await })
        .map_err(|e| format!("GCS credentials: {e}"))?;
    let auth = (!token.bearer.is_empty()).then(|| format!("Bearer {}", token.bearer));
    let size = std::fs::metadata(local)
        .map_err(|e| format!("can't read {}: {e}", local.display()))?
        .len();
    let fail = |what: &str| format!("upload to {location} failed: {what}");
    // 308 means "resume incomplete" in this protocol, not a redirect.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();

    let start = format!(
        "{base}/upload/storage/v1/b/{}/o?uploadType=resumable&name={}",
        percent_encode(bucket, false),
        percent_encode(key, false)
    );
    let mut req = agent
        .post(&start)
        .header("X-Upload-Content-Length", &size.to_string())
        .header("Content-Type", "application/json");
    if let Some(a) = &auth {
        req = req.header("Authorization", a);
    }
    let mut resp = req.send("{}").map_err(|e| fail(&e.to_string()))?;
    if !resp.status().is_success() {
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(fail(&format!(
            "HTTP {}: {}",
            resp.status(),
            body.chars().take(300).collect::<String>()
        ))
        .into());
    }
    let session = resp
        .headers()
        .get("Location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| fail("the server didn't start an upload session"))?
        .to_string();

    use std::io::Read;
    let mut file = std::fs::File::open(local)?;
    let mut buf = vec![0u8; GCS_CHUNK];
    let mut offset = 0u64;
    loop {
        let mut n = 0;
        while n < buf.len() {
            let r = file.read(&mut buf[n..])?;
            if r == 0 {
                break;
            }
            n += r;
        }
        let last = offset + n as u64 >= size;
        let range = if n == 0 {
            format!("bytes */{size}")
        } else {
            format!("bytes {offset}-{}/{size}", offset + n as u64 - 1)
        };
        let mut put = agent.put(&session).header("Content-Range", &range);
        if let Some(a) = &auth {
            put = put.header("Authorization", a);
        }
        let mut resp = put.send(&buf[..n]).map_err(|e| fail(&e.to_string()))?;
        let status = resp.status().as_u16();
        match status {
            200 | 201 => break,
            308 if !last => {}
            _ => {
                let body = resp.body_mut().read_to_string().unwrap_or_default();
                return Err(fail(&format!(
                    "HTTP {status}: {}",
                    body.chars().take(300).collect::<String>()
                ))
                .into());
            }
        }
        offset += n as u64;
    }
    Ok(location)
}

pub fn serve(kind: Kind) -> ! {
    serve_destination(
        About::new(kind.name(), env!("CARGO_PKG_VERSION")),
        ObjectStoreDestination { kind },
    )
}
