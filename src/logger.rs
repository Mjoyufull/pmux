// Performance logging to file - simple and fast
use std::fs::OpenOptions;
use std::io::Write;

fn get_log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("pmux_performance.log")
}

#[allow(dead_code)]
pub fn init_logger() {
    let log_path = get_log_path();

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = writeln!(
            file,
            "\n=== PMUX Performance Log - {} ===",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        );
        let _ = writeln!(file, "Log file: {}", log_path.display());
    }
}

pub fn log_timing(component: &str, duration: std::time::Duration) {
    let msg = format!("[TIMING] {}: {:?}", component, duration);
    write_log(&msg);
}

#[allow(dead_code)]
pub fn log_info(msg: &str) {
    let formatted = format!("[INFO] {}", msg);
    write_log(&formatted);
}

#[allow(dead_code)]
pub fn log_debug(msg: &str) {
    let formatted = format!("[DEBUG] {}", msg);
    write_log(&formatted);
}

#[allow(dead_code)]
pub fn log_error(msg: &str) {
    let formatted = format!("[ERROR] {}", msg);
    write_log(&formatted);
}

fn write_log(msg: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(get_log_path())
    {
        let _ = writeln!(file, "{}", msg);
    }
}
