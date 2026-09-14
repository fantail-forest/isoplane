use std::str::FromStr;

use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{logs::SdkLoggerProvider, Resource};
use serde::{Deserialize, Serialize};

/// A full HTTP/protobuf telemetry endpoint without credentials, query strings or fragments.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OtlpEndpoint(url::Url);

/// Invalid endpoint configuration, without echoing potentially secret input.
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("OTLP endpoint is not an absolute URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("OTLP endpoint must use HTTP or HTTPS and have a host")]
    Scheme,
    #[error("OTLP endpoint must not contain credentials, a query or a fragment; use OTLP headers for authentication")]
    EmbeddedCredentials,
}

impl FromStr for OtlpEndpoint {
    type Err = EndpointError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = url::Url::parse(value)?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(EndpointError::Scheme);
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(EndpointError::EmbeddedCredentials);
        }
        Ok(Self(url))
    }
}

impl TryFrom<String> for OtlpEndpoint {
    type Error = EndpointError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<OtlpEndpoint> for String {
    fn from(value: OtlpEndpoint) -> Self {
        value.0.into()
    }
}

pub(super) fn provider(
    resource: Resource,
    endpoint: &OtlpEndpoint,
) -> Result<SdkLoggerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint.0.as_str())
        .build()?;
    Ok(SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}

pub(super) fn trace_provider(
    resource: Resource,
    endpoint: &OtlpEndpoint,
) -> Result<opentelemetry_sdk::trace::SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint.0.as_str())
        .build()?;
    Ok(opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}

pub(super) fn resource(node: &str) -> Resource {
    Resource::builder_empty()
        .with_service_name("wispd")
        .with_attributes([
            KeyValue::new("wisp.node", node.to_owned()),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new(
                "service.instance.id",
                format!("{:032x}", rand::random::<u128>()),
            ),
        ])
        .build()
}
