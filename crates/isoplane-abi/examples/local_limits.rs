#![forbid(unsafe_code)]

fn main() {
    let limits = isoplane_abi::config::Limits::default();
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
