//! S3, GCS and Azure Blob destinations against local emulators. Each test runs when its
//! emulator's env var is set (CI starts them as service containers):
//! - `DRE_TEST_S3_ENDPOINT` (an S3-compatible emulator accepting unsigned bucket creation)
//! - `DRE_TEST_GCS_ENDPOINT` (fake-gcs-server started with `-external-url` set to this URL)
//! - `DRE_TEST_AZURITE_ENDPOINT` (Azurite blob endpoint, e.g. http://localhost:10000)

use std::path::Path;
use std::sync::Arc;

use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use object_store::{ObjectStore, ObjectStoreExt};
use serde_json::{Map, Value, json};

fn bin(name: &str) -> &'static Path {
    match name {
        "s3" => Path::new(env!("CARGO_BIN_EXE_dre-destination-s3")),
        "gcs" => Path::new(env!("CARGO_BIN_EXE_dre-destination-gcs")),
        _ => Path::new(env!("CARGO_BIN_EXE_dre-destination-azure_blob")),
    }
}

/// Azurite's documented development account.
const AZ_ACCOUNT: &str = "devstoreaccount1";
const AZ_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

fn deliver(kind: &str, remote: &str, conn: Value, bytes: &[u8]) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("report.csv");
    std::fs::write(&local, bytes).unwrap();
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(kind), log).unwrap();
    let Value::Object(c) = conn else { panic!() };
    p.deliver(local.to_str().unwrap(), Some(remote), c)
        .map_err(|e| e.to_string())
}

/// ~20 MB of varied bytes, enough for a multipart upload.
fn payload() -> Vec<u8> {
    (0..20_000_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

fn read_back(store: Arc<dyn ObjectStore>, key: &str) -> Vec<u8> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .get(&object_store::path::Path::from(key))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .to_vec()
    })
}

#[test]
fn all_three_conform_to_the_protocol() {
    for k in ["s3", "gcs", "azure"] {
        conformance::assert_conforms(bin(k));
    }
}

#[test]
fn s3_uploads_and_reports_failures() {
    let Ok(endpoint) = std::env::var("DRE_TEST_S3_ENDPOINT") else {
        eprintln!("skipped: set DRE_TEST_S3_ENDPOINT");
        return;
    };
    // The emulator accepts unsigned bucket creation.
    let _ = ureq::put(&format!("{endpoint}/reports")).send_empty();
    let conn = json!({"endpoint": endpoint, "region": "us-east-1", "access_key_id": "dre", "secret_access_key": "dre-secret"});
    let data = payload();
    let loc = deliver("s3", "s3://reports/monthly/2026/report.csv", conn.clone(), &data).unwrap();
    assert_eq!(loc, "s3://reports/monthly/2026/report.csv");
    let store = object_store::aws::AmazonS3Builder::new()
        .with_endpoint(&endpoint)
        .with_allow_http(true)
        .with_region("us-east-1")
        .with_bucket_name("reports")
        .with_access_key_id("dre")
        .with_secret_access_key("dre-secret")
        .build()
        .unwrap();
    assert_eq!(read_back(Arc::new(store), "monthly/2026/report.csv"), data);
    // A bare key goes into the profile's bucket.
    let mut c2 = conn.clone();
    c2["bucket"] = json!("reports");
    assert_eq!(
        deliver("s3", "small.csv", c2, b"a,b\r\n").unwrap(),
        "s3://reports/small.csv"
    );
    let err = deliver("s3", "s3://no-such-bucket/x.csv", conn, b"x").unwrap_err();
    assert!(
        err.contains("upload to s3://no-such-bucket/x.csv failed"),
        "{err}"
    );
}

#[test]
fn gcs_uploads_through_the_emulator() {
    let Ok(endpoint) = std::env::var("DRE_TEST_GCS_ENDPOINT") else {
        eprintln!("skipped: set DRE_TEST_GCS_ENDPOINT");
        return;
    };
    // fake-gcs-server: create the bucket, then authenticate with a key that disables OAuth.
    let _ = ureq::post(&format!("{endpoint}/storage/v1/b"))
        .header("Content-Type", "application/json")
        .send(json!({"name": "reports"}).to_string());
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key.json");
    std::fs::write(&key, json!({"gcs_base_url": endpoint, "disable_oauth": true, "client_email": "", "private_key": "", "private_key_id": ""}).to_string()).unwrap();
    let conn = json!({"service_account_key_path": key.to_str().unwrap()});
    let data = payload();
    assert_eq!(
        deliver("gcs", "gs://reports/out/report.csv", conn, &data).unwrap(),
        "gs://reports/out/report.csv"
    );
    let store = object_store::gcp::GoogleCloudStorageBuilder::new()
        .with_bucket_name("reports")
        .with_service_account_path(key.to_str().unwrap())
        .build()
        .unwrap();
    assert_eq!(read_back(Arc::new(store), "out/report.csv"), data);
}

/// Create an Azurite container with a Shared Key–signed request.
fn azurite_container(endpoint: &str, container: &str) {
    use base64::Engine;
    use hmac::{Hmac, KeyInit, Mac};
    let date = httpdate::fmt_http_date(std::time::SystemTime::now());
    let version = "2021-08-06";
    let to_sign = format!(
        "PUT\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:{date}\nx-ms-version:{version}\n/{AZ_ACCOUNT}/{AZ_ACCOUNT}/{container}\nrestype:container"
    );
    let key = base64::engine::general_purpose::STANDARD.decode(AZ_KEY).unwrap();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&key).unwrap();
    mac.update(to_sign.as_bytes());
    let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    let url = format!("{endpoint}/{AZ_ACCOUNT}/{container}?restype=container");
    let r = ureq::put(&url)
        .header("x-ms-date", &date)
        .header("x-ms-version", version)
        .header("Authorization", &format!("SharedKey {AZ_ACCOUNT}:{sig}"))
        .send_empty();
    match r {
        Ok(_) => {}
        Err(ureq::Error::StatusCode(409)) => {}
        Err(e) => panic!("creating the Azurite container failed: {e}"),
    }
}

#[test]
fn azure_uploads_with_a_connection_string_or_a_key() {
    let Ok(endpoint) = std::env::var("DRE_TEST_AZURITE_ENDPOINT") else {
        eprintln!("skipped: set DRE_TEST_AZURITE_ENDPOINT");
        return;
    };
    azurite_container(&endpoint, "reports");
    let blob = format!("{endpoint}/{AZ_ACCOUNT}");
    let cs = format!(
        "DefaultEndpointsProtocol=http;AccountName={AZ_ACCOUNT};AccountKey={AZ_KEY};BlobEndpoint={blob};"
    );
    let data = payload();
    assert_eq!(
        deliver(
            "azure",
            "az://reports/cs/report.csv",
            json!({"connection_string": cs}),
            &data
        )
        .unwrap(),
        "az://reports/cs/report.csv"
    );
    let conn =
        json!({"account_name": AZ_ACCOUNT, "access_key": AZ_KEY, "endpoint": blob, "container": "reports"});
    assert_eq!(
        deliver("azure", "key/report.csv", conn, b"a\r\n").unwrap(),
        "az://reports/key/report.csv"
    );
    let store = object_store::azure::MicrosoftAzureBuilder::new()
        .with_account(AZ_ACCOUNT)
        .with_access_key(AZ_KEY)
        .with_container_name("reports")
        .with_endpoint(blob.clone())
        .with_allow_http(true)
        .build()
        .unwrap();
    assert_eq!(read_back(Arc::new(store), "cs/report.csv"), data);
    let bad = json!({"account_name": AZ_ACCOUNT, "access_key": "d3Jvbmc=", "endpoint": blob});
    let err = deliver("azure", "az://reports/x.csv", bad, b"x").unwrap_err();
    assert!(err.contains("upload to az://reports/x.csv failed"), "{err}");
}

#[test]
fn a_path_for_another_store_is_rejected() {
    let err = deliver(
        "s3",
        "gs://bucket/x.csv",
        json!({"region": "us-east-1", "access_key_id": "a", "secret_access_key": "b"}),
        b"x",
    )
    .unwrap_err();
    assert!(err.contains("isn't a s3:// path"), "{err}");
    let _unused: Map<String, Value> = Map::new();
}
