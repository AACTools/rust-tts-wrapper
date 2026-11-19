//! #131 acceptance: the pronunciation editor's ABI surface —
//! active-detection, dry-run preview, stats, and the BOM fix.
//! Exercises the C ABI directly (the surface frontends consume).

use std::ffi::{CStr, CString};

use rust_tts_wrapper::tts_apply_pronunciations;
use rust_tts_wrapper::tts_free_string;
use rust_tts_wrapper::tts_pronunciations_active;
use rust_tts_wrapper::tts_pronunciations_stats;
use rust_tts_wrapper::tts_set_pronunciations_file;

/// The pronunciation global is process-wide: serialize tests that
/// touch it (same discipline as the crate's env-mutation tests).
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn env_locked() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn set_file(path: Option<&str>) {
    let c = path.map(CString::new).transpose().unwrap();
    let p = c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    assert_eq!(tts_set_pronunciations_file(p), 1);
}

#[test]
fn bom_does_not_kill_the_first_entry() {
    let _env = env_locked();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pron.txt");
    // Notepad's default save: UTF-8 BOM glued to line 1
    std::fs::write(&file, "\u{feff}botox = bow tocks\nplain = usual\n").unwrap();
    set_file(file.to_str());
    assert_eq!(tts_pronunciations_active(), 1, "entry 1 survives the BOM");
    let mut entries = 0;
    let mut skipped = 0;
    assert_eq!(
        tts_pronunciations_stats(&raw mut entries, &raw mut skipped),
        1
    );
    assert_eq!((entries, skipped), (2, 0));
    set_file(None);
}

#[test]
fn dry_run_rewrites_respells_and_reports_identity_without_file() {
    let _env = env_locked();
    // no active file: input comes back unchanged
    let text = CString::new("um Botox end").unwrap();
    let out = tts_apply_pronunciations(text.as_ptr());
    assert!(!out.is_null());
    let s = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    tts_free_string(out);
    assert_eq!(s, "um Botox end");

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pron.txt");
    std::fs::write(&file, "botox = bow tocks\nnguyen = wen\n").unwrap();
    set_file(file.to_str());
    let out = tts_apply_pronunciations(text.as_ptr());
    let s = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    tts_free_string(out);
    assert_eq!(s, "um bow tocks end");
    set_file(None);
}

#[test]
fn stats_report_skipped_lines_for_editor_flagging() {
    let _env = env_locked();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pron.txt");
    std::fs::write(&file, "fine = good\nno equals sign\n=x\nk =\n").unwrap();
    set_file(file.to_str());
    let mut entries = -1;
    let mut skipped = -1;
    assert_eq!(
        tts_pronunciations_stats(&raw mut entries, &raw mut skipped),
        1
    );
    assert_eq!((entries, skipped), (1, 3), "3 malformed lines counted");
    // null outputs are skipped, not dereferenced
    assert_eq!(
        tts_pronunciations_stats(std::ptr::null_mut(), std::ptr::null_mut()),
        1
    );
    set_file(None);
}

#[test]
fn active_is_zero_without_a_file() {
    let _env = env_locked();
    set_file(Some("/nonexistent/pronunciations.txt"));
    assert_eq!(tts_pronunciations_active(), 0);
    let mut entries = 5;
    let mut skipped = 5;
    assert_eq!(
        tts_pronunciations_stats(&raw mut entries, &raw mut skipped),
        0
    );
    assert_eq!((entries, skipped), (0, 0), "outputs zeroed");
    set_file(None);
}
