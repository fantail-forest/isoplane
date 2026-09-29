//! Managed behaviour through real C# guests on Mono.
//! One binary so the fixtures share one harness and one loaded template;
//! each module proves one family of behaviour.

#[path = "../common/bindings.rs"]
mod bindings;
#[path = "../common/dotnet.rs"]
#[macro_use]
mod dotnet;

mod activation;
mod certificates;
mod hashing;
mod memory;
mod randomness;
mod streaming;
