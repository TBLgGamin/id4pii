use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, anyhow};
use enigo::{Direction, Enigo, Key, Keyboard, Mouse, Settings};
use uiautomation::UIAutomation;
use uiautomation::patterns::{UITextPattern, UIValuePattern};

pub(crate) fn read_focused() -> Result<String> {
    if let Some(text) = read_via_uia()
        && !text.trim().is_empty()
    {
        return Ok(text);
    }
    read_via_clipboard()
}

fn read_via_uia() -> Option<String> {
    let automation = UIAutomation::new().ok()?;
    let element = automation.get_focused_element().ok()?;

    if let Ok(value) = element.get_pattern::<UIValuePattern>()
        && let Ok(text) = value.get_value()
        && !text.is_empty()
    {
        return Some(text);
    }
    if let Ok(text_pattern) = element.get_pattern::<UITextPattern>()
        && let Ok(range) = text_pattern.get_document_range()
        && let Ok(text) = range.get_text(-1)
    {
        return Some(text);
    }
    None
}

fn read_via_clipboard() -> Result<String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow!("input init: {e}"))?;
    ctrl_combo(&mut enigo, 'a')?;
    ctrl_combo(&mut enigo, 'c')?;
    sleep(Duration::from_millis(150));
    let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard: {e}"))?;
    let text = clipboard
        .get_text()
        .map_err(|e| anyhow!("clipboard read: {e}"))?;
    if text.trim().is_empty() {
        return Err(anyhow!("the focused field has no readable text"));
    }
    Ok(text)
}

/// Try to apply each `(find, replace)` pair as a targeted UIA text replacement, preserving
/// surrounding formatting (bold/italic/lists/newlines). Returns `Ok(true)` if every requested
/// substitution was applied via TextPattern; `Ok(false)` if the focused element doesn't support
/// TextPattern (caller should fall back). Errors only on hard failures.
pub(crate) fn apply_substitutions(substitutions: &[(String, String)]) -> Result<bool> {
    if substitutions.is_empty() {
        return Ok(true);
    }
    let automation = UIAutomation::new().map_err(|e| anyhow!("UI Automation init: {e}"))?;
    let element = automation
        .get_focused_element()
        .map_err(|e| anyhow!("no focused element: {e}"))?;

    let Ok(text_pattern) = element.get_pattern::<UITextPattern>() else {
        return Ok(false);
    };

    let _ = element.set_focus();
    let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard init: {e}"))?;
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow!("input init: {e}"))?;

    for (find, replace) in substitutions {
        let doc = match text_pattern.get_document_range() {
            Ok(r) => r,
            Err(_) => return Ok(false),
        };
        let range = match doc.find_text(find, false, false) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if range.select().is_err() {
            continue;
        }
        clipboard
            .set_text(replace.clone())
            .map_err(|e| anyhow!("clipboard set: {e}"))?;
        ctrl_combo(&mut enigo, 'v')?;
        sleep(Duration::from_millis(40));
    }
    Ok(true)
}

pub(crate) fn write_focused(text: &str) -> Result<()> {
    let automation = UIAutomation::new().map_err(|e| anyhow!("UI Automation init failed: {e}"))?;
    let element = automation
        .get_focused_element()
        .map_err(|e| anyhow!("no focused element: {e}"))?;

    if let Ok(value) = element.get_pattern::<UIValuePattern>()
        && value.set_value(text).is_ok()
    {
        return Ok(());
    }

    let _ = element.set_focus();
    let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard: {e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| anyhow!("clipboard write: {e}"))?;
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow!("input init: {e}"))?;
    ctrl_combo(&mut enigo, 'a')?;
    ctrl_combo(&mut enigo, 'v')?;
    Ok(())
}

pub(crate) fn cursor_position() -> (i32, i32) {
    Enigo::new(&Settings::default())
        .ok()
        .and_then(|enigo| enigo.location().ok())
        .unwrap_or((240, 240))
}

fn ctrl_combo(enigo: &mut Enigo, letter: char) -> Result<()> {
    if let Err(err) = enigo.key(Key::Control, Direction::Press) {
        return Err(anyhow!("input ctrl-press: {err}"));
    }
    let click_result = enigo.key(Key::Unicode(letter), Direction::Click);
    let release_result = enigo.key(Key::Control, Direction::Release);
    click_result.map_err(|e| anyhow!("input letter-click: {e}"))?;
    release_result.map_err(|e| anyhow!("input ctrl-release: {e}"))?;
    Ok(())
}
