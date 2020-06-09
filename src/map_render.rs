use std::process::Command;

use jobs;

// ----------------------------------------------------------------------------

byond_fn! { render_map(map_path) {
    invoker(map_path).ok()
} }

// Returns new job-id.
byond_fn! { start_render_job(map_path) {
    let map_path = map_path.to_string();
    Some(jobs::start(move || {
        match invoker(&map_path) {
            Ok(r) => r,
            Err(e) => e.to_string()
        }
    }))
} }

// Checks status of a job
byond_fn! { check_render_job(id) {
    Some(jobs::check(id))
} }

// Actual invoker for SpacemanDMM
fn invoker(path: &str) -> Result<String, String> {
    let output = if cfg!(target_os = "windows") {
        Command::new("tools/nanomap-renderers/renderer-windows.exe")
                .args(&["minimap", path])
                .output()
                .expect("Failed to execute render process")
    } else {
        Command::new("tools/nanomap-renderers/renderer-linux")
                .args(&["minimap", path])
                .output()
                .expect("Failed to execute render process")
    };

    let _ = output.stdout;
    Ok("SUCCESS".to_string())
}