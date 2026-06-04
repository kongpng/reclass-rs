//! Integration tests porting `tests/test_theme.cpp` (the C++ fidelity contract).
//!
//! Run headless: `cargo test --no-default-features --test theme_oracle`.
//!
//! Every manager test builds an isolated `ThemeManager` over the shipped golden
//! JSON in `_oracle/fixtures/themes/` + a fresh temp `user_dir` — never the
//! real user data dir. Where the upstream `test_theme` drifted from the shipped
//! JSON (3 recorded FAILs in `_oracle/logs/test_theme.txt`), these tests assert
//! the CORRECT current behaviour (shipped JSON + current `theme.cpp` win), as
//! the oracle documents.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use reclass::theme::color::Color;
use reclass::theme::manager::{MemSettings, ThemeManager};
use reclass::theme::model::Theme;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("_oracle/fixtures/themes")
}

fn fresh_user_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rcx-theme-it-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn manager() -> ThemeManager {
    ThemeManager::new(
        Box::new(MemSettings::new()),
        fixtures_dir(),
        fresh_user_dir(),
    )
}

#[test]
fn built_in_themes() {
    // test_theme.cpp:13-40 (builtInThemes).
    let m = manager();
    let all = m.themes();
    assert!(all.len() >= 2);

    let dark = all.iter().find(|t| t.name == "Reclass Dark").unwrap();
    assert!(dark.background.is_some());
    assert!(dark.text.is_some());
    assert!(dark.syntax_keyword.is_some());
    assert!(dark.marker_error.is_some());

    let warm = all.iter().find(|t| t.name == "Warm").unwrap();
    assert!(warm.background.is_some());
    assert!(warm.text.is_some());
    assert_eq!(warm.background, Color::parse("#212121"));
    // Shipped warm.json selection matches C++ test_theme.cpp:37.
    assert_eq!(warm.selection, Color::parse("#21213A"));
    assert_eq!(warm.syntax_keyword, Color::parse("#AA9565"));
    assert_eq!(warm.syntax_type, Color::parse("#6B959F"));
}

#[test]
fn json_round_trip() {
    // test_theme.cpp:42-60 (jsonRoundTrip).
    let m = manager();
    let orig = m.themes()[0].clone();
    let loaded = Theme::from_json(&orig.to_json());
    assert_eq!(loaded.name, orig.name);
    assert_eq!(loaded.background, orig.background);
    assert_eq!(loaded.text, orig.text);
    assert_eq!(loaded.selection, orig.selection);
    assert_eq!(loaded.syntax_keyword, orig.syntax_keyword);
    assert_eq!(loaded.syntax_number, orig.syntax_number);
    assert_eq!(loaded.syntax_string, orig.syntax_string);
    assert_eq!(loaded.syntax_comment, orig.syntax_comment);
    assert_eq!(loaded.syntax_type, orig.syntax_type);
    assert_eq!(loaded.marker_ptr, orig.marker_ptr);
    assert_eq!(loaded.marker_error, orig.marker_error);
    assert_eq!(loaded.ind_hover_span, orig.ind_hover_span);
}

#[test]
fn json_round_trip_warm() {
    // test_theme.cpp:62-76 (jsonRoundTripWarm).
    let m = manager();
    let all = m.themes();
    let orig = all.iter().find(|t| t.name == "Warm").unwrap().clone();
    let loaded = Theme::from_json(&orig.to_json());
    assert_eq!(loaded.name, orig.name);
    assert_eq!(loaded.background, orig.background);
    assert_eq!(loaded.selection, orig.selection);
    assert_eq!(loaded.syntax_keyword, orig.syntax_keyword);
}

#[test]
fn from_json_missing_fields() {
    // test_theme.cpp:78-90 (fromJsonMissingFields).
    let sparse = serde_json::json!({ "name": "Sparse", "background": "#ff0000" });
    let t = Theme::from_json(&sparse);
    assert_eq!(t.name, "Sparse");
    assert_eq!(t.background, Color::parse("#ff0000"));
    assert!(t.text.is_none());
    assert!(t.syntax_keyword.is_none());
    // C++ has no marker fallbacks: a sparse theme leaves all three markers
    // invalid (test_theme.cpp:89 `QVERIFY(!t.markerError.isValid())`).
    assert!(t.marker_error.is_none());
    assert!(t.marker_ptr.is_none());
    assert!(t.marker_cycle.is_none());
}

#[test]
fn theme_manager_has_built_ins() {
    // test_theme.cpp:92-105 (themeManagerHasBuiltIns).
    let m = manager();
    let all = m.themes();
    assert!(all.len() >= 3);
    // ORACLE FIX (test_theme FAIL line 96): filename-sort-first is "Long Night".
    assert_eq!(all[0].name, "Long Night");
    assert!(all.iter().any(|t| t.name == "VS2022 Dark"));
    assert!(all.iter().any(|t| t.name == "Warm"));
}

#[test]
fn theme_manager_switch() {
    // test_theme.cpp:107-121 (themeManagerSwitch).
    use std::cell::Cell;
    use std::rc::Rc;
    let mut m = manager();
    let count = Rc::new(Cell::new(0usize));
    let c2 = count.clone();
    m.subscribe(Box::new(move |_t| c2.set(c2.get() + 1)));

    let start = m.current_index();
    let target = if start == 0 { 1 } else { 0 };
    m.set_current(target);
    assert_eq!(count.get(), 1);
    assert_eq!(m.current_index(), target);
    assert_eq!(m.current().name, m.themes()[target].name);
    m.set_current(start);
}

#[test]
fn theme_manager_crud() {
    // test_theme.cpp:123-145 (themeManagerCRUD).
    let mut m = manager();
    let initial = m.themes().len();

    let mut custom = m.themes()[0].clone();
    custom.name = "Test Custom".to_string();
    custom.background = Color::parse("#ff0000");
    m.add_theme(custom.clone());
    assert_eq!(m.themes().len(), initial + 1);
    assert_eq!(m.themes().last().unwrap().name, "Test Custom");

    let idx = m.themes().len() - 1;
    let mut updated = custom;
    updated.background = Color::parse("#00ff00");
    m.update_theme(idx, updated);
    assert_eq!(m.themes()[idx].background, Color::parse("#00ff00"));

    m.remove_theme(idx);
    assert_eq!(m.themes().len(), initial);
}

#[test]
fn defaults_match_oracle_fixtures() {
    // Guard against drift of the shipped defaults: embedded bytes must equal
    // the oracle fixtures byte-for-byte.
    let fx = fixtures_dir();
    for (name, json) in reclass::theme::defaults::DEFAULT_THEMES.iter() {
        let disk = std::fs::read_to_string(fx.join(name))
            .unwrap_or_else(|e| panic!("read fixture {name}: {e}"));
        assert_eq!(*json, disk, "embedded default drifted from fixture: {name}");
    }
}
