//! A secret file that is read again when it changes, so that a new secret
//! takes effect without a restart. The value never reaches a log line.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const CHECK_EVERY: Duration = Duration::from_secs(60);

pub struct Secret {
    path: PathBuf,
    state: Mutex<State>,
}

struct State {
    value: String,
    stamp: Option<(SystemTime, u64)>,
    checked: Instant,
    failing: bool,
}

fn stamp(path: &Path) -> Result<(SystemTime, u64)> {
    let m = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    Ok((m.modified()?, m.len()))
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))
        .map(|s| s.trim().to_string())
}

impl Secret {
    /// No path turns the feature off. A path that is set must be readable
    /// at start.
    pub fn open(path: Option<PathBuf>) -> Result<Option<Self>> {
        path.map(Self::load).transpose()
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let stamp = stamp(&path).ok();
        let value = read(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(State {
                value,
                stamp,
                checked: Instant::now(),
                failing: false,
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current value. A file that cannot be read keeps the last value.
    pub fn get(&self) -> String {
        self.get_at(Instant::now())
    }

    fn get_at(&self, now: Instant) -> String {
        let mut s = self.state.lock().unwrap();
        if now.saturating_duration_since(s.checked) < CHECK_EVERY {
            return s.value.clone();
        }
        s.checked = now;
        let fresh = match stamp(&self.path) {
            Ok(t) if Some(t) == s.stamp => return s.value.clone(),
            Ok(t) => read(&self.path).map(|v| (t, v)),
            Err(e) => Err(e),
        };
        match fresh {
            Ok((t, v)) => {
                s.stamp = Some(t);
                s.failing = false;
                if v != s.value {
                    tracing::info!("{} changed; the new secret is in use", self.path.display());
                    s.value = v;
                }
            }
            Err(e) => {
                if !s.failing {
                    tracing::warn!("{e:#}; the previous secret stays in use");
                }
                s.failing = true;
            }
        }
        s.value.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn write(path: &Path, text: &str, mtime: u64) {
        std::fs::write(path, text).unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime))
            .unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mm-secret-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("secret")
    }

    #[test]
    fn rereads_a_changed_file_at_most_once_a_minute() {
        let path = temp("change");
        write(&path, "old\n", 1_000);
        let s = Secret::load(path.clone()).unwrap();
        let t0 = Instant::now();
        assert_eq!(s.get_at(t0), "old");
        write(&path, "new\n", 2_000);
        assert_eq!(s.get_at(t0 + Duration::from_secs(30)), "old");
        assert_eq!(s.get_at(t0 + Duration::from_secs(61)), "new");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn same_mtime_and_size_is_not_read_again() {
        let path = temp("stamp");
        write(&path, "aaa", 1_000);
        let s = Secret::load(path.clone()).unwrap();
        write(&path, "bbb", 1_000);
        assert_eq!(s.get_at(Instant::now() + Duration::from_secs(61)), "aaa");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_missing_file_keeps_the_last_value() {
        let path = temp("missing");
        write(&path, "kept", 1_000);
        let s = Secret::load(path.clone()).unwrap();
        std::fs::remove_file(&path).unwrap();
        let t = Instant::now() + Duration::from_secs(61);
        assert_eq!(s.get_at(t), "kept");
        write(&path, "back", 3_000);
        assert_eq!(s.get_at(t + Duration::from_secs(61)), "back");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn no_path_is_off_and_an_unreadable_path_fails_at_start() {
        assert!(Secret::open(None).unwrap().is_none());
        let absent = std::env::temp_dir().join(format!("mm-secret-{}-absent", std::process::id()));
        assert!(Secret::open(Some(absent)).is_err());
    }
}
