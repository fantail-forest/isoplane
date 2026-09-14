//! HTTP proxy routing, admission and request measurements.

#![forbid(unsafe_code)]

mod dispatch;
mod headers;
pub mod metrics;
mod request;
pub mod request_trace;
mod response;
pub mod routes;
pub mod server;
pub mod shed;

pub use metrics::{Histogram, ProxyMetrics};
pub use routes::{LocalRouteTable, RouteError, RouteResolver, };
pub use server::{Proxy, ProxyConfig};
pub use shed::{AdmissionControl, ShedConfig};
