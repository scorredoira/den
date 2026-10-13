//! Den's own files in its config folder (`config.json`, the terminals'
//! `layout.json`, `notes.json`), each kept whole: written beside it and
//! renamed over it, so a crash halfway leaves the one before; on a thread
//! of its own, so the disk never holds up the window; and one that can't be
//! read is kept aside before the next save writes over it.

use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

use serde::{Serialize, de::DeserializeOwned};

pub struct JsonFile {
    name: &'static str,
    /// How many times it was prepared for saving: each save's number.
    saves: AtomicU64,
    /// Held while writing: an older save never replaces a newer one, and a
    /// flush waits for the write under way.
    state: Mutex<State>,
    /// Wakes the thread that writes the saves made later.
    writer: OnceLock<mpsc::Sender<()>>,
    /// The tests' folder: they never touch the real one.
    #[cfg(test)]
    dir: Mutex<Option<PathBuf>>,
}

struct State {
    /// The newest save the writer hasn't got to, by its number.
    waiting: Option<(u64, Vec<u8>)>,
    /// The number of the last one written.
    written: Option<u64>,
}

/// The files saved later, for the quit to finish writing them.
static LATER: Mutex<Vec<&'static JsonFile>> = Mutex::new(Vec::new());

impl JsonFile {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            saves: AtomicU64::new(0),
            state: Mutex::new(State { waiting: None, written: None }),
            writer: OnceLock::new(),
            #[cfg(test)]
            dir: Mutex::new(None),
        }
    }

    #[cfg(not(test))]
    fn path(&self) -> Option<PathBuf> {
        proto::config_dir().ok().map(|dir| dir.join(self.name))
    }

    #[cfg(test)]
    fn path(&self) -> Option<PathBuf> {
        Some(self.dir.lock().unwrap().as_ref()?.join(self.name))
    }

    /// What it holds, if it's there.
    pub fn read(&self) -> Option<Vec<u8>> {
        std::fs::read(self.path()?).ok()
    }

    /// Keeps `bytes`, which couldn't all be read, aside (as `<name>.bad`)
    /// before the next save writes over them.
    pub fn keep_aside(&self, bytes: &[u8]) {
        if let Some(path) = self.path() {
            let _ = std::fs::write(path.with_extension("json.bad"), bytes);
        }
    }

    /// What it holds; the default if it isn't there, or if it can't be read
    /// (then it's kept aside).
    pub fn load<T: DeserializeOwned + Default>(&self) -> T {
        let Some(bytes) = self.read() else {
            return T::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            self.keep_aside(&bytes);
            T::default()
        })
    }

    /// Saves `value` now (on quit: nothing would be left to write it later).
    pub fn save<T: Serialize>(&self, value: &T) {
        if let Some(save) = self.number(value) {
            let mut state = self.lock();
            self.write(&mut state, save);
        }
    }

    /// Saves `value` on the file's own thread: only the newest of those
    /// waiting is written.
    pub fn save_later<T: Serialize>(&'static self, value: &T) {
        let Some(save) = self.number(value) else {
            return;
        };
        self.lock().waiting = Some(save);
        let writer = self.writer.get_or_init(|| {
            LATER.lock().unwrap_or_else(|err| err.into_inner()).push(self);
            let (tx, rx) = mpsc::channel::<()>();
            std::thread::spawn(move || {
                while rx.recv().is_ok() {
                    self.flush();
                }
            });
            tx
        });
        let _ = writer.send(());
    }

    /// Writes the save waiting, if there's one, once the one under way is.
    pub fn flush(&self) {
        let mut state = self.lock();
        if let Some(save) = state.waiting.take() {
            self.write(&mut state, save);
        }
    }

    /// `value` as it's written, numbered so an older one never replaces a
    /// newer one.
    fn number<T: Serialize>(&self, value: &T) -> Option<(u64, Vec<u8>)> {
        let bytes = serde_json::to_vec_pretty(value).ok()?;
        Some((self.saves.fetch_add(1, Ordering::Relaxed), bytes))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Writes `bytes` unless a newer save was written already.
    fn write(&self, state: &mut State, (generation, bytes): (u64, Vec<u8>)) {
        if state.written.is_some_and(|written| written > generation) {
            return;
        }
        if let Some(path) = self.path()
            && let Err(err) = write_whole(&path, &bytes)
        {
            eprintln!("couldn't save {}: {err:#}", path.display());
        }
        state.written = Some(generation);
    }
}

/// The saves made later are written (on quit, before the app goes).
pub fn flush_all() {
    let files = LATER.lock().unwrap_or_else(|err| err.into_inner()).clone();
    for file in files {
        file.flush();
    }
}

/// Whole or not at all: a crash halfway leaves the one before.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn file(name: &'static str) -> (&'static JsonFile, PathBuf) {
        let dir = std::env::temp_dir().join(format!("den-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file: &'static JsonFile = Box::leak(Box::new(JsonFile::new(name)));
        *file.dir.lock().unwrap() = Some(dir.clone());
        (file, dir)
    }

    /// One cut short reads as nothing, and is kept aside before a save
    /// writes over it.
    #[test]
    fn a_file_it_cant_read_is_kept_aside() {
        let (file, dir) = file("cut.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cut.json"), br#"{"a": "one", "b": "#).unwrap();
        let read: HashMap<String, String> = file.load();
        assert!(read.is_empty());
        assert_eq!(std::fs::read(dir.join("cut.json.bad")).unwrap(), br#"{"a": "one", "b": "#);
        file.save(&HashMap::from([("c", "three")]));
        let read: HashMap<String, String> = file.load();
        assert_eq!(read, HashMap::from([("c".to_string(), "three".to_string())]));
        assert!(!dir.join("cut.json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Saved later, then flushed: the newest is what's there, and an older
    /// one written after it doesn't replace it.
    #[test]
    fn the_newest_save_wins() {
        let (file, dir) = file("newest.json");
        let older = file.number(&"older").unwrap();
        file.save_later(&"later");
        file.save(&"newer");
        flush_all();
        file.write(&mut file.lock(), older);
        assert_eq!(file.load::<String>(), "newer");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
