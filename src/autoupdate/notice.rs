use std::sync::Mutex;

static UPDATE_NOTICE: Mutex<Option<String>> = Mutex::new(None);

pub(super) fn store_update_tag(tag: &str) {
    if let Ok(mut notice) = UPDATE_NOTICE.lock() {
        *notice = Some(tag.to_string());
    }
}

pub(crate) fn update_available_tag() -> Option<String> {
    UPDATE_NOTICE.lock().ok().and_then(|notice| notice.clone())
}

#[cfg(test)]
pub(super) fn clear_update_tag_for_tests() {
    if let Ok(mut notice) = UPDATE_NOTICE.lock() {
        *notice = None;
    }
}

/// Spawn the TUI's throttled, read-only release lookup.  The thread never
/// downloads or replaces anything, and all network failures are silent.
pub(crate) fn maybe_spawn_update_notice() {
    if !super::should_check() {
        return;
    }

    std::thread::spawn(|| {
        let latest = super::check_and_update_notice();
        // Record the attempt even when the network is unavailable so a
        // disconnected machine does not retry on every TUI startup.
        let _ = super::record_check();
        if let Some(tag) = latest {
            store_update_tag(&tag);
        }
    });
}
