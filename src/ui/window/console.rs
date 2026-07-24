//! Windows GUI-subsystem console management.
//!
//! The application starts without a console window and creates one only when
//! View > Show Console is enabled. Redirected stdout/stderr handles are restored
//! after `AllocConsole`, so logs captured by a shell or test harness are never
//! stolen by the temporary console. Only a console allocated by this process is
//! released again.

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{HANDLE, HWND};
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_DISK, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Console::{
        AllocConsole, FreeConsole, GetConsoleWindow, GetStdHandle, SetStdHandle, STD_ERROR_HANDLE,
        STD_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DeleteMenu, GetSystemMenu, ShowWindow, MF_BYCOMMAND, SC_CLOSE, SW_SHOW,
    };

    static ALLOCATED_BY_RECLASS: AtomicBool = AtomicBool::new(false);

    fn redirected_standard_handle(which: STD_HANDLE) -> Option<HANDLE> {
        // SAFETY: both APIs only inspect the process standard-handle table and
        // the returned kernel handle. No ownership is transferred.
        let handle = unsafe { GetStdHandle(which) };
        if handle.is_null() || handle == (-1isize as HANDLE) {
            return None;
        }
        let kind = unsafe { GetFileType(handle) };
        (kind == FILE_TYPE_PIPE || kind == FILE_TYPE_DISK).then_some(handle)
    }

    fn disable_close_button(hwnd: HWND) {
        // SAFETY: `hwnd` came from GetConsoleWindow. Removing SC_CLOSE prevents
        // the console close button from terminating the GUI and losing edits.
        let menu = unsafe { GetSystemMenu(hwnd, 0) };
        if !menu.is_null() {
            unsafe {
                DeleteMenu(menu, SC_CLOSE, MF_BYCOMMAND);
            }
        }
    }

    pub(super) fn set_visible(visible: bool) -> bool {
        if visible {
            // Save redirected handles before AllocConsole: Windows may install
            // fresh CONOUT$ handles, but a pipe/file supplied by the caller must
            // remain authoritative.
            let redirected_stdout = redirected_standard_handle(STD_OUTPUT_HANDLE);
            let redirected_stderr = redirected_standard_handle(STD_ERROR_HANDLE);

            // SAFETY: these calls operate on the current process console only.
            let mut hwnd = unsafe { GetConsoleWindow() };
            if hwnd.is_null() && unsafe { AllocConsole() } != 0 {
                ALLOCATED_BY_RECLASS.store(true, Ordering::Release);
                if let Some(handle) = redirected_stdout {
                    unsafe {
                        SetStdHandle(STD_OUTPUT_HANDLE, handle);
                    }
                }
                if let Some(handle) = redirected_stderr {
                    unsafe {
                        SetStdHandle(STD_ERROR_HANDLE, handle);
                    }
                }
                hwnd = unsafe { GetConsoleWindow() };
                if !hwnd.is_null() {
                    disable_close_button(hwnd);
                }
            }
            if !hwnd.is_null() {
                unsafe {
                    ShowWindow(hwnd, SW_SHOW);
                }
                true
            } else {
                false
            }
        } else {
            if ALLOCATED_BY_RECLASS.swap(false, Ordering::AcqRel) {
                // SAFETY: the flag is set only after this process successfully
                // called AllocConsole; inherited consoles are never detached.
                unsafe {
                    FreeConsole();
                }
            }
            false
        }
    }
}

#[cfg(windows)]
pub(super) fn set_visible(visible: bool) -> bool {
    imp::set_visible(visible)
}

#[cfg(not(windows))]
pub(super) fn set_visible(_visible: bool) -> bool {
    false
}

#[cfg(windows)]
impl super::MainWindow {
    /// Toggle View ▸ Show Console, persist the preference, and keep the menu
    /// checkmark synchronized with the requested state. `set_visible(false)`
    /// deliberately leaves an inherited console attached; only a console this
    /// process allocated is ever released.
    pub(super) fn toggle_console(&mut self, cx: &mut gpui::Context<Self>) {
        self.show_console = !self.show_console;
        set_visible(self.show_console);
        crate::theme::SettingsStore::set(
            &mut *self.settings.borrow_mut(),
            super::settings_keys::SHOW_CONSOLE,
            if self.show_console { "true" } else { "false" },
        );
        self.sync_console_menu_checked(cx);
    }

    pub(super) fn sync_console_menu_checked(&mut self, cx: &mut gpui::Context<Self>) {
        let checked = self.show_console;
        self.menubar.update(cx, |menubar, cx| {
            menubar.set_command_checked("view.show_console", checked, cx);
        });
    }
}
