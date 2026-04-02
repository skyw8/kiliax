use std::env;
use std::fs;
use std::io;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use http::Uri;
use opentelemetry_otlp::tonic_types::transport::Certificate as TonicCertificate;
use opentelemetry_otlp::tonic_types::transport::ClientTlsConfig;
use opentelemetry_otlp::tonic_types::transport::Identity as TonicIdentity;
use opentelemetry_otlp::OTEL_EXPORTER_OTLP_TIMEOUT;
use opentelemetry_otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Certificate as ReqwestCertificate, Identity as ReqwestIdentity};

use kiliax_core::config::OtelTlsConfig;

pub(crate) fn build_header_map(headers: &std::collections::BTreeMap<String, String>) -> HeaderMap {
    let mut header_map = HeaderMap::new();
    for (key, value) in headers {
        if let Ok(name) = HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(val) = HeaderValue::from_str(value) {
                header_map.insert(name, val);
            }
        }
    }
    header_map
}

pub(crate) fn headers_to_hashmap(
    headers: &std::collections::BTreeMap<String, String>,
) -> std::collections::HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub(crate) fn build_grpc_tls_config(
    endpoint: &str,
    tls_config: ClientTlsConfig,
    tls: &OtelTlsConfig,
) -> anyhow::Result<ClientTlsConfig> {
    let uri: Uri = endpoint.parse()?;
    let host = uri.host().ok_or_else(|| {
        config_error(format!(
            "OTLP gRPC endpoint {endpoint} does not include a host"
        ))
    })?;

    let mut config = tls_config.domain_name(host.to_owned());

    if let Some(path) = tls.ca_cert.as_ref() {
        let pem = read_bytes(path)?;
        config = config.ca_certificate(TonicCertificate::from_pem(pem));
    }

    match (&tls.client_cert, &tls.client_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert_pem = read_bytes(cert_path)?;
            let key_pem = read_bytes(key_path)?;
            config = config.identity(TonicIdentity::from_pem(cert_pem, key_pem));
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(config_error(
                "client_cert and client_key must both be provided for mTLS",
            ));
        }
        (None, None) => {}
    }

    Ok(config)
}

/// Build a blocking HTTP client with TLS configuration for OTLP HTTP exporters.
///
/// OTEL exporters can run on OS threads that are not backed by tokio.
pub(crate) fn build_http_client(
    tls: &OtelTlsConfig,
    timeout_var: &str,
) -> anyhow::Result<reqwest::blocking::Client> {
    if current_tokio_runtime_is_multi_thread() {
        tokio::task::block_in_place(|| build_http_client_inner(tls, timeout_var))
    } else if tokio::runtime::Handle::try_current().is_ok() {
        let tls = tls.clone();
        let timeout_var = timeout_var.to_string();
        std::thread::spawn(move || build_http_client_inner(&tls, &timeout_var))
            .join()
            .map_err(|_| config_error("failed to join OTLP blocking HTTP client builder thread"))?
    } else {
        build_http_client_inner(tls, timeout_var)
    }
}

pub(crate) fn current_tokio_runtime_is_multi_thread() -> bool {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread,
        Err(_) => false,
    }
}

fn build_http_client_inner(
    tls: &OtelTlsConfig,
    timeout_var: &str,
) -> anyhow::Result<reqwest::blocking::Client> {
    let mut builder =
        reqwest::blocking::Client::builder().timeout(resolve_otlp_timeout(timeout_var));

    if let Some(path) = tls.ca_cert.as_ref() {
        let pem = read_bytes(path)?;
        let certificate = ReqwestCertificate::from_pem(pem.as_slice()).map_err(|error| {
            config_error(format!(
                "failed to parse certificate {}: {error}",
                path.display()
            ))
        })?;
        builder = builder
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate);
    }

    match (&tls.client_cert, &tls.client_key) {
        (Some(cert_path), Some(key_path)) => {
            let mut cert_pem = read_bytes(cert_path)?;
            let key_pem = read_bytes(key_path)?;
            cert_pem.extend_from_slice(key_pem.as_slice());
            let identity = ReqwestIdentity::from_pem(cert_pem.as_slice()).map_err(|error| {
                config_error(format!(
                    "failed to parse client identity using {} and {}: {error}",
                    cert_path.display(),
                    key_path.display()
                ))
            })?;
            builder = builder.identity(identity).https_only(true);
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(config_error(
                "client_cert and client_key must both be provided for mTLS",
            ));
        }
        (None, None) => {}
    }

    Ok(builder.build()?)
}

pub(crate) fn build_async_http_client(
    tls: Option<&OtelTlsConfig>,
    timeout_var: &str,
) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(resolve_otlp_timeout(timeout_var));

    if let Some(tls) = tls {
        if let Some(path) = tls.ca_cert.as_ref() {
            let pem = read_bytes(path)?;
            let certificate = ReqwestCertificate::from_pem(pem.as_slice()).map_err(|error| {
                config_error(format!(
                    "failed to parse certificate {}: {error}",
                    path.display()
                ))
            })?;
            builder = builder
                .tls_built_in_root_certs(false)
                .add_root_certificate(certificate);
        }

        match (&tls.client_cert, &tls.client_key) {
            (Some(cert_path), Some(key_path)) => {
                let mut cert_pem = read_bytes(cert_path)?;
                let key_pem = read_bytes(key_path)?;
                cert_pem.extend_from_slice(key_pem.as_slice());
                let identity = ReqwestIdentity::from_pem(cert_pem.as_slice()).map_err(|error| {
                    config_error(format!(
                        "failed to parse client identity using {} and {}: {error}",
                        cert_path.display(),
                        key_path.display()
                    ))
                })?;
                builder = builder.identity(identity).https_only(true);
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(config_error(
                    "client_cert and client_key must both be provided for mTLS",
                ));
            }
            (None, None) => {}
        }
    }

    Ok(builder.build()?)
}

pub(crate) fn resolve_otlp_timeout(signal_var: &str) -> Duration {
    if let Some(timeout) = read_timeout_env(signal_var) {
        return timeout;
    }
    if let Some(timeout) = read_timeout_env(OTEL_EXPORTER_OTLP_TIMEOUT) {
        return timeout;
    }
    OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT
}

fn read_timeout_env(var: &str) -> Option<Duration> {
    let value = env::var(var).ok()?;
    let parsed = value.parse::<i64>().ok()?;
    if parsed < 0 {
        return None;
    }
    Some(Duration::from_millis(parsed as u64))
}

fn read_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn config_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(io::Error::new(ErrorKind::InvalidData, message.into()))
}
