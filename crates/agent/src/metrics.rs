use axum::Json;
use hfr_proto::Metrics;
use sysinfo::{Disks, System};

pub async fn metrics() -> Json<Metrics> {
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();
    let (du, dt) =
        disks.iter().max_by_key(|d| d.total_space()).map(|d| (d.total_space() - d.available_space(), d.total_space())).unwrap_or((0, 0));
    Json(Metrics {
        cpu_total: sys.global_cpu_usage(),
        cpu_cores: sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
        mem_used: sys.used_memory(),
        mem_total: sys.total_memory(),
        swap_used: sys.used_swap(),
        swap_total: sys.total_swap(),
        load1: System::load_average().one,
        disk_used: du,
        disk_total: dt,
    })
}
