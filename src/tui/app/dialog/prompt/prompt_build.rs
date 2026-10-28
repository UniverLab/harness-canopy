use super::*;
use crate::domain::prompt_draft::{self, DraftSection, PromptCatalog, PromptContext, PromptDraft};

/// Test-only re-export of the assembly helpers that moved to
/// `domain::prompt_draft`: the unchanged dialog tests reach them through
/// this module's glob import in `prompt.rs`.
#[cfg(test)]
pub(crate) use crate::domain::prompt_draft::{
    append_tool_skill, build_xml_block, format_indexed_xml_section, push_xml_item,
    strip_resources_section,
};

impl SimplePromptDialog {
    /// Thin shim over the moved [`prompt_draft::format_project_block`],
    /// kept so the unchanged dialog tests still resolve this path.
    pub(super) fn format_project_block(project: &Project) -> String {
        prompt_draft::format_project_block(project)
    }

    pub(super) fn format_file_resource(path: &Path) -> String {
        format!("path: {}\nkind: file", path.display())
    }

    pub(super) fn format_rag_chunk(
        query: &str,
        chunk: &crate::rag::vector_store::SearchResult,
    ) -> String {
        let dist = chunk
            .distance
            .map_or("—".to_string(), |d| format!("{d:.4}"));
        format!(
            "kind: rag_chunk\nquery: {query}\npath: {}\ndistance: {}\ncontent:\n{}",
            chunk.file_path, dist, chunk.content
        )
    }

    pub(super) fn lookup_project_reference(db: &Database, entry: &str) -> Result<Option<Project>> {
        if let Some(project) = db.get_project(entry)? {
            return Ok(Some(project));
        }

        let path = Path::new(entry);
        if path.exists() {
            return db.get_project_by_path_or_ancestor(path);
        }

        Ok(None)
    }

    pub(super) fn resolve_resource_entry(db: &Database, raw: &str) -> String {
        let trimmed = raw.trim();

        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return format!("path: {trimmed}\nkind: url");
        }

        let path = Path::new(trimmed);
        if path.exists() {
            if path.is_dir() {
                if let Ok(Some(project)) = db.get_project_by_path(path) {
                    return Self::format_project_block(&project);
                }
                return format!("path: {}\nkind: directory", path.display());
            }
            return Self::format_file_resource(path);
        }

        format!("content: {trimmed}\nkind: raw")
    }

    pub(super) fn resolve_rag_scope<'a>(
        query: &'a str,
        default_project_hash: Option<&'a str>,
    ) -> (RagScope<'a>, &'a str) {
        if let Some(rest) = query.strip_prefix("global:") {
            return (RagScope::Global, rest.trim());
        }
        if let Some(rest) = query.strip_prefix("project:") {
            if let Some((project_hash, query)) = rest.split_once(':') {
                return (RagScope::Project(project_hash.trim()), query.trim());
            }
        }

        default_project_hash.map_or((RagScope::Global, query.trim()), |project_hash| {
            (RagScope::Project(project_hash), query.trim())
        })
    }

    pub(super) fn default_project_hash(
        &self,
        db: &Database,
        current_workdir: &Path,
    ) -> Option<String> {
        db.get_project_by_path_or_ancestor(current_workdir)
            .ok()
            .flatten()
            .map(|project| project.hash)
    }

    pub(super) fn resolve_project_contexts(&self, db: &Database) -> Vec<String> {
        let mut seen_hashes = HashSet::new();
        let mut projects = self
            .section_lines("project_context")
            .into_iter()
            .filter_map(|entry| Self::lookup_project_reference(db, &entry).ok().flatten())
            .filter(|project| seen_hashes.insert(project.hash.clone()))
            .collect::<Vec<_>>();

        for project in self.derived_project_contexts_from_resources(db) {
            if seen_hashes.insert(project.hash.clone()) {
                projects.push(project);
            }
        }

        projects
            .into_iter()
            .map(|project| Self::format_project_block(&project))
            .collect()
    }

    pub(super) fn derived_project_contexts_from_resources(&self, db: &Database) -> Vec<Project> {
        self.section_lines("resources")
            .into_iter()
            .filter_map(|entry| {
                let path = Path::new(&entry);
                path.exists()
                    .then(|| db.get_project_by_path_or_ancestor(path).ok().flatten())
                    .flatten()
            })
            .collect()
    }

    pub(super) fn resolve_resource_entries(&self, db: &Database) -> Vec<String> {
        self.section_lines("resources")
            .into_iter()
            .map(|entry| Self::resolve_resource_entry(db, &entry))
            .collect()
    }

    pub(super) fn search_rag_resources<'a>(
        _db: &Database,
        query: &'a str,
        _default_project_hash: Option<&'a str>,
    ) -> Vec<String> {
        let (scope, resolved_query) = Self::resolve_rag_scope(query, _default_project_hash);
        let _ = scope;
        if resolved_query.is_empty() {
            return Vec::new();
        }

        let canopy_dir = match dirs::home_dir() {
            Some(h) => h.join(".canopy"),
            None => return Vec::new(),
        };
        let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
        let model = config.embeddings_model.trim();
        if model.is_empty() {
            return Vec::new();
        }
        let Ok(dimensions) = crate::rag::embedding_client::model_dimensions(model) else {
            return Vec::new();
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return Vec::new();
        };

        rt.block_on(async {
            let Ok(store) = crate::rag::vector_store::VectorStore::new(
                dimensions,
                Some(config.rag_vector_cache_entries),
            )
            .await
            else {
                return Vec::new();
            };
            let Ok(embedder) = crate::rag::embedding_client::client_from_config(&config) else {
                return Vec::new();
            };
            let Ok(query_vec) = embedder.embed(resolved_query) else {
                return Vec::new();
            };
            let Ok(results) = store.search_similar(&query_vec, 5).await else {
                return Vec::new();
            };
            results
                .iter()
                .map(|chunk| Self::format_rag_chunk(resolved_query, chunk))
                .collect()
        })
    }

    pub(super) fn resolve_rag_resources(
        &self,
        db: &Database,
        default_project_hash: Option<&str>,
    ) -> Vec<String> {
        self.section_lines("rag_search")
            .into_iter()
            .flat_map(|query| Self::search_rag_resources(db, &query, default_project_hash))
            .collect()
    }

    /// Test-only shim over the moved [`prompt_draft::tool_lines`]: the
    /// unchanged dialog tests still call this, while production builds the
    /// tools group through [`prompt_draft::build_prompt_body`].
    #[cfg(test)]
    pub(super) fn collect_tool_lines(&self) -> Vec<String> {
        prompt_draft::tool_lines(&self.draft_sections())
    }

    /// This dialog's state as a domain draft: every enabled section by id,
    /// with no separate named choices — a picked preset/skills/project
    /// already lives in the sections as instruction/tools/project_context
    /// content by the time anything builds.
    fn to_prompt_draft(&self) -> PromptDraft {
        PromptDraft {
            sections: self.draft_sections(),
            preset: None,
            skills: Vec::new(),
            project: None,
        }
    }

    fn draft_sections(&self) -> Vec<DraftSection> {
        self.enabled_sections
            .iter()
            .map(|id| DraftSection {
                title: id.clone(),
                content: self
                    .section_content_for_build(id)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect()
    }

    pub fn build_prompt_with_resolved_resources(
        &self,
        db: &Database,
        current_workdir: &Path,
    ) -> Result<String> {
        let default_project_hash = self.default_project_hash(db, current_workdir);
        let mut resources = self.resolve_resource_entries(db);
        resources.extend(self.resolve_rag_resources(db, default_project_hash.as_deref()));

        let ctx = PromptContext {
            catalog: PromptCatalog::default(),
            project_blocks: self.resolve_project_contexts(db),
            resources,
            turn_context: self.system_content.clone(),
            protocol_included: self.protocol_in_prompt(),
        };
        prompt_draft::build_prompt_text(&self.to_prompt_draft(), &ctx).map_err(anyhow::Error::from)
    }

    /// Build the prompt body (every section except the system block).
    /// Test-only now: production sends build through
    /// [`prompt_draft::build_prompt_text`], and this stays so the unchanged
    /// dialog tests still resolve the helper.
    #[cfg(test)]
    pub(super) fn build_body(&self) -> String {
        prompt_draft::build_prompt_body(&self.to_prompt_draft(), &PromptCatalog::default())
            .expect("dialog drafts carry no preset or skill lookups")
    }
}

// ── Prompt builder helpers ────────────────────────────────────────
// Skill discovery helpers

pub(super) fn add_skills_from_dir(
    dir: &std::path::Path,
    prefix: &str,
    out: &mut Vec<(String, String, String)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(raw_name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if crate::skills_module::find_skill_instructions(&path).is_none() {
            continue;
        }
        out.push((format!("skill:{raw_name}"), raw_name, prefix.to_string()));
    }
}
