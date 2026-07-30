//! Gathering an Explorer multi-select back into a single conversion.
//!
//! A classic shell verb is launched once per selected file, so right-clicking
//! twelve images and choosing "Convert to WebP" starts twelve processes, each
//! knowing about one file. Twelve folder dialogs is not a feature.
//!
//! So the processes elect one of themselves. The first to start owns a named pipe
//! and becomes the collector; every later one connects, hands its paths over and
//! exits immediately. The collector waits for a short gap in arrivals — that gap,
//! not a fixed sleep, is how it knows Explorer has finished launching — and then
//! converts the whole set, asking for a destination once.
//!
//! Election is the pipe itself rather than a separate mutex:
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` fails if an instance already exists, so
//! creating it is both the lock and the channel, and there is no window between
//! taking one and opening the other.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long to wait for another process to turn up before deciding the selection
/// is complete.
///
/// Explorer launches the verb in a tight loop, so arrivals are milliseconds apart;
/// this only has to outlast process startup. Too short and a large selection gets
/// split into two batches with two dialogs — the exact thing being avoided.
const STRAGGLER_WAIT: Duration = Duration::from_millis(500);

/// A ceiling on the whole gathering phase, so a wedged sibling cannot hold the
/// conversion open indefinitely.
const GATHER_LIMIT: Duration = Duration::from_secs(10);

/// What this process turned out to be.
pub enum Role {
    /// Nobody else was collecting, so this process is. Carries every path gathered,
    /// including the ones it started with.
    Collector(Vec<PathBuf>),
    /// Another process is collecting and has been given our paths. Nothing left to
    /// do but exit.
    HandedOver,
}

/// Join the batch identified by `key`, contributing `files`.
///
/// `key` must distinguish conversions that should not merge: two different formats
/// picked in quick succession are two batches, not one.
#[cfg(windows)]
pub fn join(key: &str, files: Vec<PathBuf>) -> Role {
    let name = pipe_name(key);

    match windows::create_first_instance(&name) {
        Some(pipe) => Role::Collector(gather(pipe, files)),
        None => {
            if windows::hand_over(&name, &files) {
                Role::HandedOver
            } else {
                // The collector went away between our failing to create the pipe and
                // our trying to reach it — it may have crashed, or simply finished
                // and closed up. Converting these files ourselves is better than
                // dropping them silently, and the worst case is two dialogs.
                Role::Collector(files)
            }
        }
    }
}

/// Elsewhere there is no Explorer to multi-launch us, so there is nothing to gather.
#[cfg(not(windows))]
pub fn join(_key: &str, files: Vec<PathBuf>) -> Role {
    Role::Collector(files)
}

/// Collect paths until arrivals stop, then return everything including our own.
#[cfg(windows)]
fn gather(pipe: windows::Pipe, mut files: Vec<PathBuf>) -> Vec<PathBuf> {
    let (tx, rx) = mpsc::channel();

    // Accepting blocks, so it happens on its own thread and the deadline lives here.
    // The thread is deliberately never joined: once the gap has elapsed there may
    // still be a client mid-connect, and waiting for a client that will never come
    // is what the deadline exists to avoid.
    std::thread::Builder::new()
        .name("convert-collector".to_owned())
        .spawn(move || pipe.accept_loop(&tx))
        .expect("failed to spawn the collector thread");

    let started = Instant::now();
    while started.elapsed() < GATHER_LIMIT {
        match rx.recv_timeout(STRAGGLER_WAIT) {
            Ok(handed_over) => files.extend(handed_over),
            // A gap this long means Explorer has finished launching.
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            // The accept thread gave up; nothing more is coming.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Explorer launches the verb in file-name order but the processes arrive in
    // whatever order the scheduler allows, so without this the progress output is
    // shuffled. Duplicates go too: the same file selected once cannot usefully be
    // converted twice, and Explorer has been known to double-invoke.
    files.sort();
    files.dedup();
    files
}

/// The pipe this batch talks over.
///
/// Named pipes are machine-global, so the key is all that separates two of our own
/// batches. It does not need to separate users: a pipe created with default security
/// grants write access to its creator only, so another account's process cannot hand
/// us paths even if it guesses the name.
fn pipe_name(key: &str) -> String {
    format!(r"\\.\pipe\imaginer-convert-{key}")
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::FromRawHandle;
    use std::path::PathBuf;
    use std::sync::mpsc::Sender;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    /// Buffer sizes for the pipe. Paths are short and the traffic is one burst, so
    /// this only needs to be large enough that a client's write never blocks.
    const BUFFER: u32 = 64 * 1024;

    /// The server end of the batch pipe.
    pub struct Pipe(HANDLE);

    // SAFETY: the handle is owned solely by this struct, and the only thing done
    // with it after construction is to move it to the accept thread.
    unsafe impl Send for Pipe {}

    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateNamedPipeW` and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Create the pipe, or `None` if another process already holds it.
    ///
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` is what makes this an election: exactly one
    /// caller can succeed, and it does not matter which.
    pub fn create_first_instance(name: &str) -> Option<Pipe> {
        let wide = wide(name);

        // SAFETY: `wide` is a NUL-terminated wide string that outlives the call, and
        // null security attributes ask for the default descriptor.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER,
                BUFFER,
                0,
                std::ptr::null(),
            )
        };

        (handle != INVALID_HANDLE_VALUE).then_some(Pipe(handle))
    }

    impl Pipe {
        /// Accept clients one after another, forwarding what each sends.
        ///
        /// Returns when the channel closes, which is the main thread saying the
        /// batch is settled and it no longer cares.
        pub fn accept_loop(self, tx: &Sender<Vec<PathBuf>>) {
            loop {
                // SAFETY: `self.0` is a valid server-end pipe handle, and a null
                // overlapped pointer is the documented blocking form.
                let connected = unsafe { ConnectNamedPipe(self.0, std::ptr::null_mut()) } != 0
                    // A client that connected between creating the pipe and this call
                    // is already there — reported as a failure, but the good kind.
                    || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;

                if !connected {
                    return;
                }

                let payload = self.read_to_end();

                // SAFETY: the handle is still ours and still connected.
                unsafe { DisconnectNamedPipe(self.0) };

                let paths = decode(&payload);
                if !paths.is_empty() && tx.send(paths).is_err() {
                    return;
                }
            }
        }

        /// Read until the client closes its end.
        fn read_to_end(&self) -> Vec<u8> {
            use std::io::Read;

            // SAFETY: the handle is a valid readable pipe. Wrapped rather than
            // duplicated, so it must not be closed when the `File` drops — hence
            // `ManuallyDrop`: the pipe has to survive for the next client.
            let mut file = std::mem::ManuallyDrop::new(unsafe {
                std::fs::File::from_raw_handle(self.0.cast())
            });

            let mut payload = Vec::new();
            let _ = file.read_to_end(&mut payload);
            payload
        }
    }

    /// Send `files` to the process holding the pipe. `false` if it could not be
    /// reached at all.
    pub fn hand_over(name: &str, files: &[PathBuf]) -> bool {
        use std::io::Write;

        // The collector is single-threaded between clients, so finding the pipe busy
        // is the normal case rather than an error — it means a sibling got there
        // first and is being read right now.
        let deadline = std::time::Instant::now() + super::STRAGGLER_WAIT * 4;
        loop {
            match std::fs::OpenOptions::new().write(true).open(name) {
                Ok(mut pipe) => {
                    return pipe.write_all(&encode(files)).is_ok() && pipe.flush().is_ok();
                }
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return false,
            }
        }
    }

    /// Paths as NUL-separated UTF-16.
    ///
    /// UTF-16 because that is what Windows paths natively are: going through UTF-8
    /// would mean `to_string_lossy` and a mangled path for anything that is not
    /// valid Unicode. NUL as the separator because it is the one byte a path cannot
    /// contain.
    fn encode(files: &[PathBuf]) -> Vec<u8> {
        let mut units: Vec<u16> = Vec::new();
        for (index, file) in files.iter().enumerate() {
            if index > 0 {
                units.push(0);
            }
            units.extend(file.as_os_str().encode_wide());
        }
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    }

    fn decode(payload: &[u8]) -> Vec<PathBuf> {
        let units: Vec<u16> = payload
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();

        units
            .split(|unit| *unit == 0)
            .filter(|part| !part.is_empty())
            .map(|part| PathBuf::from(std::ffi::OsString::from_wide(part)))
            .collect()
    }

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn paths_survive_the_round_trip() {
            let files = vec![
                PathBuf::from(r"C:\photos\holiday.png"),
                PathBuf::from(r"D:\a folder with spaces\logo.v2.jpg"),
            ];
            assert_eq!(decode(&encode(&files)), files);
        }

        #[test]
        fn non_ascii_paths_are_not_mangled() {
            // The reason the wire format is UTF-16 rather than lossy UTF-8.
            let files = vec![PathBuf::from(r"C:\gambar\Ünïcödé — 日本語.png")];
            assert_eq!(decode(&encode(&files)), files);
        }

        #[test]
        fn a_single_path_carries_no_separator() {
            let files = vec![PathBuf::from(r"C:\one.png")];
            let payload = encode(&files);
            assert_eq!(payload.len(), r"C:\one.png".len() * 2);
            assert_eq!(decode(&payload), files);
        }

        #[test]
        fn an_empty_payload_decodes_to_nothing_rather_than_a_blank_path() {
            assert!(decode(&[]).is_empty());
            // A trailing separator must not become an empty path either, which as a
            // `PathBuf` would later be treated as the current directory.
            assert!(decode(&[0, 0]).is_empty());
        }

        #[test]
        fn an_odd_trailing_byte_is_dropped_rather_than_panicking() {
            // A client killed mid-write leaves half a code unit behind.
            let mut payload = encode(&[PathBuf::from("a.png")]);
            payload.push(0x41);
            assert_eq!(decode(&payload), [PathBuf::from("a.png")]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_name_is_scoped_to_the_batch() {
        assert_eq!(pipe_name("webp-80"), r"\\.\pipe\imaginer-convert-webp-80");
        assert_ne!(pipe_name("webp-80"), pipe_name("webp-90"));
        assert_ne!(pipe_name("webp-80"), pipe_name("ico-80"));
    }
}
