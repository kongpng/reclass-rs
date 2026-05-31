//! `BookmarkNameProvider` — surfaces the active document's bookmarks.
//!
//! Port of `src/names/bookmark_name_provider.{h,cpp}`. Bookmarks are per-document,
//! so the provider takes a callback returning the active controller. The C++ type
//! is `std::function<RcxController*()>`.
//!
//! This is the only provider depending on the `controller`/`core`/`addr` ports.
//! Those modules are still skeletons in this workflow, so the controller surface
//! the provider needs is abstracted behind the [`BookmarkHost`] trait — the
//! integrator wires `RcxController` to it. There is no oracle test for this
//! provider in the `rtti` set (`PORTING_rtti-symbols.md §4.7`).

use crate::addr::{AddressParser, AddressParserCallbacks};
use crate::core::Bookmark;
use crate::provider::Provider;
use crate::rtti::names::{NameProvider, NamedAddress};
use crate::rtti::symbol_store::SymbolStore;

/// The slice of `RcxController`/`RcxDocument` the bookmark provider needs.
/// Implemented by the controller port; keeps the dependency edge without
/// requiring the (skeleton) controller to expose these inherent methods yet.
pub trait BookmarkHost {
    /// `ctrl->document()->tree.bookmarks` (`bookmark_name_provider.cpp:43`).
    fn bookmarks(&self) -> Vec<Bookmark>;
    /// `ctrl->document()->tree.pointerSize` (`bookmark_name_provider.cpp:44`).
    fn pointer_size(&self) -> i32;
    /// `ctrl->addBookmark(name, formula)` (`bookmark_name_provider.cpp:60`).
    fn add_bookmark(&self, name: &str, formula: &str);
    /// `ctrl->removeBookmark(i)` (`bookmark_name_provider.cpp:69`).
    fn remove_bookmark(&self, index: usize);
}

/// Callback returning the active controller (`BookmarkNameProvider::ActiveCtrlFn`,
/// `bookmark_name_provider.h:16`). May return `None`.
pub type ActiveHostFn = Box<dyn Fn() -> Option<Box<dyn BookmarkHost>> + Send + Sync>;

/// `class BookmarkNameProvider` (`bookmark_name_provider.h:13`).
pub struct BookmarkNameProvider {
    f: ActiveHostFn,
}

impl BookmarkNameProvider {
    /// `BookmarkNameProvider(fn)` (`bookmark_name_provider.h:18`).
    pub fn new(f: ActiveHostFn) -> Self {
        BookmarkNameProvider { f }
    }
}

/// `evaluateFormula(formula, prov, ptrSize)` (static, `bookmark_name_provider.cpp:16`).
fn evaluate_formula(formula: &str, prov: Option<&dyn Provider>, ptr_size: i32) -> u64 {
    let mut cbs = AddressParserCallbacks::default();
    if let Some(p) = prov {
        cbs.resolve_module = Some(Box::new(move |name: &str| {
            let base = p.symbol_to_address(name);
            (base, base != 0)
        }));
        cbs.read_pointer = Some(Box::new(move |addr: u64| {
            let n = ptr_size.clamp(0, 8) as usize;
            let mut b = [0u8; 8];
            let ok = p.read(addr, &mut b[..n]);
            (u64::from_le_bytes(b), ok)
        }));
        cbs.resolve_identifier = Some(Box::new(move |name: &str| {
            SymbolStore::global().lock().unwrap().resolve(name, Some(p))
        }));
    }
    let result = AddressParser::evaluate(
        formula,
        if ptr_size != 0 { ptr_size } else { 8 },
        Some(&cbs),
    );
    if result.ok {
        result.value
    } else {
        0
    }
}

impl NameProvider for BookmarkNameProvider {
    fn id(&self) -> String {
        "bookmark".to_owned()
    }
    fn display_name(&self) -> String {
        "Bookmarks".to_owned()
    }

    /// `entries(active)` (`bookmark_name_provider.cpp:38`). Bookmark addresses are
    /// formulas re-evaluated each call.
    fn entries(&self, active: Option<&dyn Provider>) -> Vec<NamedAddress> {
        let host = match (self.f)() {
            Some(h) => h,
            None => return Vec::new(),
        };
        let ptr_size = host.pointer_size();
        host.bookmarks()
            .into_iter()
            .map(|b| NamedAddress {
                address: evaluate_formula(&b.address_formula, active, ptr_size),
                name: b.name,
                kind: "bookmark".to_owned(),
                ..Default::default()
            })
            .collect()
    }

    fn supports_add(&self) -> bool {
        true
    }

    /// `add(name, address)` (`bookmark_name_provider.cpp:54`).
    fn add(&self, name: &str, address: u64) -> bool {
        let host = match (self.f)() {
            Some(h) => h,
            None => return false,
        };
        let formula = format!("0x{address:x}");
        host.add_bookmark(name, &formula);
        true
    }

    fn supports_remove(&self) -> bool {
        true
    }

    /// `remove(name)` (`bookmark_name_provider.cpp:62`).
    fn remove(&self, name: &str) -> bool {
        let host = match (self.f)() {
            Some(h) => h,
            None => return false,
        };
        let bms = host.bookmarks();
        for (i, b) in bms.iter().enumerate() {
            if b.name == name {
                host.remove_bookmark(i);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    // A test host that does NOT exercise evaluate_formula (which would call the
    // skeleton AddressParser::evaluate todo!()). entries() with no bookmarks, and
    // add/remove round-trip, are covered without touching the parser.
    struct TestHost {
        bms: Rc<RefCell<Vec<Bookmark>>>,
    }
    impl BookmarkHost for TestHost {
        fn bookmarks(&self) -> Vec<Bookmark> {
            self.bms.borrow().clone()
        }
        fn pointer_size(&self) -> i32 {
            8
        }
        fn add_bookmark(&self, name: &str, formula: &str) {
            self.bms.borrow_mut().push(Bookmark {
                name: name.to_owned(),
                address_formula: formula.to_owned(),
            });
        }
        fn remove_bookmark(&self, index: usize) {
            self.bms.borrow_mut().remove(index);
        }
    }

    #[test]
    fn no_host_yields_empty_and_false() {
        let p = BookmarkNameProvider::new(Box::new(|| None));
        assert!(p.entries(None).is_empty());
        assert!(!p.add("x", 0x10));
        assert!(!p.remove("x"));
        assert!(p.supports_add());
        assert!(p.supports_remove());
        assert_eq!(p.id(), "bookmark");
        assert_eq!(p.display_name(), "Bookmarks");
    }

    #[test]
    fn add_remove_round_trip() {
        // Thread-local-ish: the closure must be Send+Sync, so use a leaked static
        // store keyed per-test via a fresh Box each call is impossible; instead we
        // build a host that captures a process-local store guarded for the test.
        use std::sync::Mutex;
        static STORE: Mutex<Vec<Bookmark>> = Mutex::new(Vec::new());
        STORE.lock().unwrap().clear();

        struct SyncHost;
        impl BookmarkHost for SyncHost {
            fn bookmarks(&self) -> Vec<Bookmark> {
                STORE.lock().unwrap().clone()
            }
            fn pointer_size(&self) -> i32 {
                8
            }
            fn add_bookmark(&self, name: &str, formula: &str) {
                STORE.lock().unwrap().push(Bookmark {
                    name: name.to_owned(),
                    address_formula: formula.to_owned(),
                });
            }
            fn remove_bookmark(&self, index: usize) {
                STORE.lock().unwrap().remove(index);
            }
        }

        let p = BookmarkNameProvider::new(Box::new(|| Some(Box::new(SyncHost))));
        assert!(p.add("here", 0x1234));
        // The bookmark formula round-trips as "0x1234".
        assert_eq!(STORE.lock().unwrap()[0].address_formula, "0x1234");
        assert!(p.remove("here"));
        assert!(STORE.lock().unwrap().is_empty());
        // remove unknown -> false.
        assert!(!p.remove("nope"));
        // silence unused warning on the Rc TestHost helper.
        let _ = TestHost {
            bms: Rc::new(RefCell::new(Vec::new())),
        };
    }
}
