//! Microsoft symbol-server PDB downloader.
//!
//! Port of `src/symbol_downloader.{h,cpp}`. The C++ class is a `QObject` with
//! async Qt signals over `QNetworkAccessManager`; the port uses `reqwest`
//! blocking on a worker thread and a [`DownloadEvent`] callback in place of the
//! signals. **The cache-path layout and the MS server URL are exact behavioral
//! contracts — replicated verbatim** (`symbol_downloader.cpp:19-113`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// `SymbolDownloader::DownloadRequest` (`symbol_downloader.h:17-22`).
#[derive(Clone, Debug, Default)]
pub struct DownloadRequest {
    /// display name (e.g. "ntoskrnl.exe").
    pub module_name: String,
    /// PDB filename (e.g. "ntoskrnl.pdb").
    pub pdb_name: String,
    /// 32 hex chars, no dashes.
    pub guid_string: String,
    pub age: u32,
}

/// Replaces the Qt `progress`/`finished` signals (`symbol_downloader.h:40-43`).
#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Progress {
        module_name: String,
        received: i64,
        total: i64,
    },
    Finished {
        module_name: String,
        local_path: String,
        success: bool,
        error: String,
    },
}

/// Qualifier/org/app identity for the symbol cache directory. Mirrors the C++
/// `QStandardPaths::AppLocalDataLocation` org/app the rest of the app uses.
const APP_QUALIFIER: &str = "";
const APP_ORG: &str = "Reclass";
const APP_NAME: &str = "Reclass";

/// `SymbolDownloader::cacheDir()` (`symbol_downloader.cpp:19`).
/// `AppLocalDataLocation + "/SymbolCache"`.
pub fn cache_dir() -> PathBuf {
    let base = directories::ProjectDirs::from(APP_QUALIFIER, APP_ORG, APP_NAME)
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("SymbolCache")
}

/// Pure helper: the cache path for a request —
/// `cacheDir/{pdbName}/{guid}{age:x}/{pdbName}` (`symbol_downloader.cpp:24-31`).
/// `age` is lowercase hex, no `0x` (== `QString::number(age,16)`).
pub fn cached_path(req: &DownloadRequest) -> PathBuf {
    cache_dir()
        .join(&req.pdb_name)
        .join(format!("{}{:x}", req.guid_string, req.age))
        .join(&req.pdb_name)
}

/// Pure helper: the MS symbol-server download URL
/// (`symbol_downloader.cpp:46`).
pub fn download_url(req: &DownloadRequest) -> String {
    format!(
        "https://msdl.microsoft.com/download/symbols/{}/{}{:x}/{}",
        req.pdb_name, req.guid_string, req.age, req.pdb_name
    )
}

/// `class SymbolDownloader` (`symbol_downloader.h:12`). Holds the cancel flag for
/// the single active download.
#[derive(Default)]
pub struct SymbolDownloader {
    cancel_flag: Arc<AtomicBool>,
}

impl SymbolDownloader {
    pub fn new() -> Self {
        SymbolDownloader::default()
    }

    /// `findCached(req)` (`symbol_downloader.cpp:24`).
    pub fn find_cached(&self, req: &DownloadRequest) -> Option<PathBuf> {
        let path = cached_path(req);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    /// `findLocal(moduleFullPath, pdbName)` (static, `symbol_downloader.cpp:33`).
    /// Candidate = `<dir-of-module>/<pdbName>`.
    pub fn find_local(module_full_path: &str, pdb_name: &str) -> Option<PathBuf> {
        if module_full_path.is_empty() || pdb_name.is_empty() {
            return None;
        }
        let dir = Path::new(module_full_path).parent()?;
        let candidate = dir.join(pdb_name);
        if candidate.exists() {
            Some(candidate)
        } else {
            None
        }
    }

    /// `cancel()` (`symbol_downloader.cpp:115`) — abort the active download.
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    /// `download(req)` (`symbol_downloader.cpp:44`). Blocking; runs the HTTP GET
    /// with `reqwest` and emits [`DownloadEvent`]s through `on_event`. The
    /// streaming chunk loop honors [`cancel`](Self::cancel) like Qt's `abort()`.
    #[cfg(feature = "symbols")]
    pub fn download<F: Fn(DownloadEvent)>(&self, req: &DownloadRequest, on_event: F) {
        // cancel any previous (matches C++ cancel()-first).
        self.cancel();
        self.cancel_flag.store(false, Ordering::SeqCst);
        let cancel = self.cancel_flag.clone();

        let url = download_url(req);
        let module_name = req.module_name.clone();
        let pdb_name = req.pdb_name.clone();
        let guid = req.guid_string.clone();
        let age = req.age;

        let client = match reqwest::blocking::Client::builder()
            .user_agent("Microsoft-Symbol-Server/10.0.0.0")
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                on_event(DownloadEvent::Finished {
                    module_name,
                    local_path: String::new(),
                    success: false,
                    error: format!("Download failed: {e}"),
                });
                return;
            }
        };

        let mut resp = match client.get(&url).send() {
            Ok(r) => r,
            Err(e) => {
                on_event(DownloadEvent::Finished {
                    module_name,
                    local_path: String::new(),
                    success: false,
                    error: format!("Download failed: {e}"),
                });
                return;
            }
        };

        let status = resp.status();
        if status.as_u16() != 200 {
            on_event(DownloadEvent::Finished {
                module_name,
                local_path: String::new(),
                success: false,
                error: format!("HTTP {}", status.as_u16()),
            });
            return;
        }

        let total = resp.content_length().map(|t| t as i64).unwrap_or(-1);
        let mut data: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        use std::io::Read;
        loop {
            if cancel.load(Ordering::SeqCst) {
                on_event(DownloadEvent::Finished {
                    module_name,
                    local_path: String::new(),
                    success: false,
                    error: "Download failed: cancelled".to_owned(),
                });
                return;
            }
            match resp.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    data.extend_from_slice(&chunk[..n]);
                    on_event(DownloadEvent::Progress {
                        module_name: module_name.clone(),
                        received: data.len() as i64,
                        total,
                    });
                }
                Err(e) => {
                    on_event(DownloadEvent::Finished {
                        module_name,
                        local_path: String::new(),
                        success: false,
                        error: format!("Download failed: {e}"),
                    });
                    return;
                }
            }
        }

        if data.is_empty() {
            on_event(DownloadEvent::Finished {
                module_name,
                local_path: String::new(),
                success: false,
                error: "Empty response".to_owned(),
            });
            return;
        }

        let dir = cache_dir()
            .join(&pdb_name)
            .join(format!("{guid}{age:x}"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            on_event(DownloadEvent::Finished {
                module_name,
                local_path: String::new(),
                success: false,
                error: format!("Cannot write: {e}"),
            });
            return;
        }
        let path = dir.join(&pdb_name);
        if let Err(e) = std::fs::write(&path, &data) {
            on_event(DownloadEvent::Finished {
                module_name,
                local_path: String::new(),
                success: false,
                error: format!("Cannot write: {e}"),
            });
            return;
        }
        on_event(DownloadEvent::Finished {
            module_name,
            local_path: path.to_string_lossy().into_owned(),
            success: true,
            error: String::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── §TEST 3 — URL + cache-path construction (no network) ──
    #[test]
    fn url_and_cache_path_construction() {
        let req = DownloadRequest {
            module_name: "ntoskrnl.exe".to_owned(),
            pdb_name: "ntoskrnl.pdb".to_owned(),
            guid_string: "ABCDEF0123456789ABCDEF0123456789".to_owned(),
            age: 1,
        };
        assert_eq!(
            download_url(&req),
            "https://msdl.microsoft.com/download/symbols/ntoskrnl.pdb/ABCDEF0123456789ABCDEF01234567891/ntoskrnl.pdb"
        );
        // cache path ends with .../ntoskrnl.pdb/<GUID><age:x>/ntoskrnl.pdb
        let p = cached_path(&req);
        let s = p.to_string_lossy();
        assert!(
            s.ends_with("SymbolCache/ntoskrnl.pdb/ABCDEF0123456789ABCDEF01234567891/ntoskrnl.pdb")
                || s.ends_with(
                    "SymbolCache\\ntoskrnl.pdb\\ABCDEF0123456789ABCDEF01234567891\\ntoskrnl.pdb"
                ),
            "{s}"
        );
    }

    #[test]
    fn age_hex_formatting() {
        let req = DownloadRequest {
            module_name: "m.exe".to_owned(),
            pdb_name: "m.pdb".to_owned(),
            guid_string: "GUID".to_owned(),
            age: 0x1a,
        };
        // age 0x1a -> "1a" (lowercase hex, no 0x).
        assert!(download_url(&req).contains("/GUID1a/"));
    }

    #[test]
    fn find_local_empty_inputs() {
        assert!(SymbolDownloader::find_local("", "x.pdb").is_none());
        assert!(SymbolDownloader::find_local("/a/b/m.dll", "").is_none());
        // nonexistent file -> None.
        assert!(SymbolDownloader::find_local("/nonexistent/dir/m.dll", "m.pdb").is_none());
    }
}
