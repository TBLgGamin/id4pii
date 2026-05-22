use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, anyhow};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use uiautomation::UIAutomation;
use uiautomation::patterns::{UITextPattern, UIValuePattern};

pub(crate) fn read_focused() -> Result<String> {
    let automation = UIAutomation::new().map_err(|e| anyhow!("UI Automation init failed: {e}"))?;
    let element = automation
        .get_focused_element()
        .map_err(|e| anyhow!("no focused element: {e}"))?;

    if let Ok(value) = element.get_pattern::<UIValuePattern>()
        && let Ok(text) = value.get_value()
        && !text.is_empty()
    {
        return Ok(text);
    }
    if let Ok(text_pattern) = element.get_pattern::<UITextPattern>()
        && let Ok(range) = text_pattern.get_document_range()
        && let Ok(text) = range.get_text(-1)
    {
        return Ok(text);
    }
    Err(anyhow!("the focused control does not expose readable text"))
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

pub(crate) fn read_selection() -> Result<String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow!("input init: {e}"))?;
    ctrl_combo(&mut enigo, 'c')?;
    sleep(Duration::from_millis(140));
    let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard: {e}"))?;
    clipboard
        .get_text()
        .map_err(|e| anyhow!("clipboard read: {e}"))
}

pub(crate) fn show_popup(title: &str, body: &str) {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(body)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn ctrl_combo(enigo: &mut Enigo, letter: char) -> Result<()> {
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| anyhow!("input: {e}"))?;
    enigo
        .key(Key::Unicode(letter), Direction::Click)
        .map_err(|e| anyhow!("input: {e}"))?;
    enigo
        .key(Key::Control, Direction::Release)
        .map_err(|e| anyhow!("input: {e}"))?;
    Ok(())
}
