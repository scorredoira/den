//! Panics written to `crash.log` in the state folder: opened from the Dock,
//! the app's stderr goes nowhere and macOS's crash report has only the stack.

use std::{
    backtrace::Backtrace,
    io::Write as _,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = log(info);
        default(info);
    }));
}

fn log(info: &std::panic::PanicHookInfo) -> anyhow::Result<()> {
    let dir = proto::state_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log"))?;
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs());
    let thread = std::thread::current();
    writeln!(
        file,
        "--- unix {secs}, version {}, thread {}\n{info}\n{}",
        env!("CARGO_PKG_VERSION"),
        thread.name().unwrap_or("?"),
        Backtrace::force_capture(),
    )?;
    Ok(())
}
