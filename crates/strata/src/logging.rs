use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

/// Log with `tracing` to `<home>/logs/<name>.log`, appending.
pub fn init(home: &Path, name: &str) -> std::io::Result<()> {
    let dir = home.join("logs");
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{name}.log")))?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_to_home_logs() {
        let home = tempfile::tempdir().unwrap();
        super::init(home.path(), "test").unwrap();
        tracing::info!("hello from the test");
        let log = std::fs::read_to_string(home.path().join("logs/test.log")).unwrap();
        assert!(log.contains("hello from the test"));
    }
}
