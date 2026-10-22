#![forbid(unsafe_code)]

use isoplane_abi::config::Limits;

fn main() {
    let limits = Limits {
        instances_per_application: 16,
        module_bytes_compressed: 52428800,
        package_bytes: 52428800,
        instance_memory_max: 536870912,
        occupancy_ms_per_dispatch: 300000,
        instance_open_connections: 32,
        pending_futures_per_dispatch: 4096,
    };
    println!("| Resource | Local default |\n| --- | --- |");
    println!("| Live instances | {} |", limits.instances_per_application);
    println!(
        "| Soft memory per instance | {} MiB ({} bytes) |",
        limits.instance_memory_max / (1 << 20),
        limits.instance_memory_max
    );
    println!(
        "| Running time per request | {} ms |",
        limits.occupancy_ms_per_dispatch
    );
    println!(
        "| Pending operations per request | {} |",
        limits.pending_futures_per_dispatch
    );
    println!(
        "| Concurrent outbound connections per instance | {} |",
        limits.instance_open_connections
    );
    println!(
        "| Uncompressed application package | {} MiB |",
        limits.package_bytes / (1 << 20)
    );
}
