use std::io;
use std::path::Path;
use std::process::Command;

// Only the short-lived child opens the executable inode for writing. Closing a
// parent writer and renaming its file is insufficient: a concurrent fork can
// retain that writer, and rename keeps the same inode, causing ETXTBSY at exec.
pub fn copy(source: &Path, destination: &Path) -> io::Result<()> {
    let status = Command::new("/bin/cp")
        .args(["--preserve=mode", "--"])
        .arg(source)
        .arg(destination)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("/bin/cp failed: {status}")));
    }
    Ok(())
}

pub fn script(destination: &Path, body: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let staging = destination.with_extension("writing");
    let mut writer = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&staging)?;
    let written = writer.write_all(body.as_bytes());
    drop(writer);
    let result = written
        .and_then(|()| copy(&staging, destination))
        .and_then(|()| {
            std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))
        });
    let cleanup = std::fs::remove_file(staging);
    result.and(cleanup)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("executable-fixture-{}-{id}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn copied_executable_preserves_mode_and_runs_while_source_is_open_for_writing() {
        let directory = Directory::new();
        let source = directory.0.join("source");
        let mut writer = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&source)
            .unwrap();
        writer
            .write_all(b"#!/bin/sh\nprintf '%s' \"$1\"\n")
            .unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o751)).unwrap();
        let destination = directory.0.join("copied fixture '$'");

        super::copy(&source, &destination).unwrap();

        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
        let output = Command::new(&destination)
            .arg("copied fixture")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"copied fixture");
        drop(writer);
    }

    #[test]
    fn script_is_executable_only_by_its_owner_and_leaves_no_staging_file() {
        let directory = Directory::new();
        let destination = directory.0.join("script fixture '$'");

        super::script(&destination, "#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();

        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let output = Command::new(&destination)
            .arg("script fixture")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"script fixture");
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    }
}
