//! Getting stdout onto the terminal that launched us.
//!
//! Release builds are `windows_subsystem = "windows"` so that Explorer and file
//! associations do not flash a console window. The cost is that a GUI-subsystem
//! process gets no console at all: run it from PowerShell and every `println!`
//! goes nowhere. That is fine for the viewer and useless for `--convert`, which
//! reports what it wrote.
//!
//! So the conversion path borrows the parent's console on the way in. Nothing here
//! runs for a normal viewer launch.

/// Attach to the console of whatever launched this process, if there is one.
///
/// Silent when there isn't — that is the Explorer case, where the caller falls
/// back to a dialog for anything the user must see.
#[cfg(windows)]
pub fn attach_to_parent() {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };

    // SAFETY: no arguments, no state of ours involved. Failure means there was no
    // parent console to attach to, which is an ordinary outcome.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }

    // Attaching does not reliably populate the standard handles, so point them at
    // the console explicitly. Only when they are empty: a handle that is already
    // valid belongs to a redirection the user asked for (`--convert ... > log.txt`)
    // and overwriting it would send their output to the screen instead of the file.
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: `which` is one of the documented constants.
        let existing = unsafe { GetStdHandle(which) };
        if !existing.is_null() && existing != INVALID_HANDLE_VALUE {
            continue;
        }
        redirect_to_console(which);
    }

    /// Open `CONOUT$` — the console's own output device — and install it.
    ///
    /// Through `OpenOptions` rather than `CreateFileW`, which needs two more
    /// `windows-sys` features and a wide string to say the same thing.
    fn redirect_to_console(which: STD_HANDLE) {
        use std::os::windows::io::IntoRawHandle;

        let Ok(console) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONOUT$")
        else {
            return;
        };

        // Deliberately leaked: the standard handle has to outlive this function,
        // and closing it on drop would shut stdout again the moment we returned.
        let handle = console.into_raw_handle();

        // SAFETY: the handle was just opened and is not owned by anything else, and
        // `which` is one of the documented constants.
        unsafe { SetStdHandle(which, handle.cast()) };
    }
}

#[cfg(not(windows))]
pub fn attach_to_parent() {}
