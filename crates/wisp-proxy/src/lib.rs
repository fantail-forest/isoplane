//! HTTP proxy routing, admission and request measurements.

mod dispatch;
pub mod metrics;
mod request;
pub mod routes;
pub mod server;
pub mod shed;

pub use metrics::{Histogram, ProxyMetrics};
pub use routes::{LocalRouteTable, RouteError, RouteResolver, };
pub use server::{Proxy, ProxyConfig};
pub use shed::{AdmissionControl, ShedConfig};
