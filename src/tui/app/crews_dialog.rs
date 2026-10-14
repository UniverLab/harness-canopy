//! CT30: dialog state for the Crews panel face — no I/O, no `App`.
//!
//! Three dialogs sit on top of the face: the `e`-dialog (mark a pair
//! unavailable with a reason/expiry), the `p`-dialog (mark a whole provider
//! prefix unavailable) and the `a`-dialog (add a member to a crew, picked
//! from the model catalog). Key handling lives in
//! `crate::tui::event::crews_keys`, rendering in
//! `crate::tui::ui::panel::crews`.

use crate::domain::models_db::{self, ModelCatalog, ModelEntry};

/// One of the Crews face's three dialogs. Every key is swallowed while one
/// is open (`dialog_keys`), and Esc closes it without writing anything.
#[derive(Debug, Clone)]
pub(crate) enum CrewsDialog {
    /// `e` — mark one pair unavailable. `field` 0 = Reason, 1 = Until.
    Unavailable {
        platform: String,
        model: String,
        reason: String,
        until: String,
        field: usize,
    },
    /// `p` — mark a whole provider prefix unavailable; `covers` is the
    /// precomputed pair count shown as `covers N pair(s)`.
    ProviderPrefix {
        platform: String,
        pattern: String,
        covers: usize,
    },
    /// `a` — append a member to a crew, picked from `suggestions`.
    AddMember {
        crew: String,
        platform: String,
        query: String,
        suggestions: Vec<ModelEntry>,
        idx: usize,
        catalog: Option<ModelCatalog>,
    },
}

impl CrewsDialog {
    /// Recompute the AddMember suggestion list from the current query and
    /// clamp the selection (mirrors `refresh_model_suggestions` in the
    /// new-agent dialog). The other two dialogs have no suggestions to
    /// refresh, so this is a no-op for them.
    pub(crate) fn refilter(&mut self) {
        let CrewsDialog::AddMember {
            platform,
            query,
            suggestions,
            idx,
            catalog,
            ..
        } = self
        else {
            return;
        };
        let Some(catalog) = catalog else {
            suggestions.clear();
            *idx = 0;
            return;
        };
        *suggestions = models_db::suggestions_for(catalog, platform, query);
        // Mirror `refresh_model_suggestions`: an out-of-range selection
        // restarts at the top instead of sticking to the list's end.
        if *idx >= suggestions.len() {
            *idx = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> ModelEntry {
        ModelEntry {
            id: id.to_string(),
            name: id.to_string(),
            provider: "p".to_string(),
            release_date: None,
            size_hint: None,
        }
    }

    #[test]
    fn add_member_suggestions_filter_by_query() {
        let catalog = ModelCatalog {
            models: vec![entry("a-1"), entry("b-1"), entry("a-2")],
            fetched_at: std::time::SystemTime::now(),
        };
        let mut dialog = CrewsDialog::AddMember {
            crew: "review".to_string(),
            platform: "opencode-go".to_string(),
            query: "a-".to_string(),
            suggestions: vec![entry("b-1")],
            idx: 5,
            catalog: Some(catalog),
        };
        dialog.refilter();
        let CrewsDialog::AddMember {
            suggestions, idx, ..
        } = &dialog
        else {
            panic!("still an AddMember dialog");
        };
        let ids: Vec<&str> = suggestions.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a-1", "a-2"], "query filters the catalog");
        assert_eq!(*idx, 0, "a stale index is clamped into range");
    }

    #[test]
    fn refilter_without_catalog_clears_suggestions() {
        let mut dialog = CrewsDialog::AddMember {
            crew: "review".to_string(),
            platform: "opencode-go".to_string(),
            query: String::new(),
            suggestions: vec![entry("a-1")],
            idx: 0,
            catalog: None,
        };
        dialog.refilter();
        let CrewsDialog::AddMember { suggestions, .. } = &dialog else {
            panic!("still an AddMember dialog");
        };
        assert!(suggestions.is_empty());
    }

    #[test]
    fn refilter_is_a_no_op_for_the_other_dialogs() {
        let mut dialog = CrewsDialog::Unavailable {
            platform: "opencode-go".to_string(),
            model: "kimi-k2".to_string(),
            reason: String::new(),
            until: String::new(),
            field: 0,
        };
        dialog.refilter();
        let CrewsDialog::Unavailable { reason, .. } = &dialog else {
            panic!("still an Unavailable dialog");
        };
        assert!(reason.is_empty());
    }
}
