//! A memory readout for profiling: launched with `ONICODE_MEMORY_LOG=<file>`,
//! the app appends its footprint and GPU usage to that file every 10 seconds.
//! `open --env ONICODE_MEMORY_LOG=/tmp/monocode-memory.log "Onicode.app"`
use std::io::Write;
use std::time::Duration;

use gpui::App;

const PERIOD: Duration = Duration::from_secs(10);

pub(crate) fn start(cx: &mut App) {
    let Some(path) = std::env::var_os("ONICODE_MEMORY_LOG") else {
        return;
    };
    let executor = cx.background_executor().clone();
    cx.background_executor()
        .spawn(async move {
            loop {
                let line = sample();
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let _ = writeln!(file, "{line}");
                }
                executor.timer(PERIOD).await;
            }
        })
        .detach();
}

fn sample() -> String {
    let gpu = gpui_apple::fast::gpu_stats::snapshot();
    let mb = |bytes: usize| bytes as f64 / (1024. * 1024.);
    format!(
        "{} footprint={:.1}MB gpu={:.1}MB instance_buffer={:.1}MB backdrop={:.1}MB paths={:.1}MB \
         tiles={:.1}MB windows={} atlas={:.1}MB atlas_textures={}",
        chrono::Local::now().format("%H:%M:%S"),
        mb(footprint() as usize),
        mb(gpu.device),
        mb(gpu.pooled_buffers * gpu.buffer_size),
        mb(gpu.backdrop),
        mb(gpu.paths),
        mb(gpu.tiles),
        gpu.renderers,
        mb(gpu.atlas),
        gpu.atlas_textures,
    )
}

/// What Activity Monitor shows as Memory.
fn footprint() -> u64 {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: the buffer is a rusage_info_v2, the flavor this call fills.
    let status = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast(),
        )
    };
    if status == 0 {
        // SAFETY: a successful call initialized the whole struct.
        unsafe { info.assume_init() }.ri_phys_footprint
    } else {
        0
    }
}
