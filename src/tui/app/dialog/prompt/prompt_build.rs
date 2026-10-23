use super::*;

impl SimplePromptDialog {
    pub(super) fn format_project_block(project: &Project) -> String {
        let mut lines = vec![
            format!("name: {}", project.name),
            format!("workdir_hash: {}", project.hash),
            format!("path: {}", project.path),
        ];
        if let Some(description) = project.description.as_deref() {
            lines.push(format!("description: {}", description));
        }
        if let Some(tags) = project.tags.as_deref() {
            lines.push(format!("tags: {}", tags));
        }
        if let Some(indexed_at) = project.indexed_at {
            lines.push(format!("indexed_at: {}", indexed_at));
        }
        lines.join("\n")
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

    pub(super) fn append_prompt_section(
        &self,
        result: &mut String,
        prefix: &str,
        header: &str,
        outer_tag: &str,
        item_tag: &str,
    ) {
        build_xml_block(
            result,
            &self.enabled_sections,
            |section_id| Self::section_matches_prefix(section_id, prefix),
            |section_id| self.section_content_for_build(section_id),
            header,
            outer_tag,
            item_tag,
        );
    }

    pub(super) fn append_instruction_section(&self, result: &mut String) {
        result.push_str("# [INSTRUCTIONS]: Execution Logic\n");
        result.push_str("<instruction_set>\n");
        for content in self.section_entries("instruction") {
            push_xml_item(result, "instruction", content);
        }
        result.push_str("</instruction_set>\n\n");
    }

    pub(super) fn append_tools_section(&self, result: &mut String) {
        let tool_lines = self.collect_tool_lines();
        if tool_lines.is_empty() {
            return;
        }

        result.push_str("# [TOOLS]: Skills & Capabilities\n");
        result.push_str("<tools>\n");
        for tool_line in tool_lines {
            append_tool_skill(result, &tool_line);
        }
        result.push_str("</tools>\n\n");
    }

    pub(super) fn collect_tool_lines(&self) -> Vec<String> {
        self.section_entries("tools")
            .into_iter()
            .flat_map(|content| {
                content
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn build_prompt_with_resolved_resources(
        &self,
        db: &Database,
        current_workdir: &Path,
    ) -> Result<String> {
        let system = self.render_system_block();
        let mut body = self.build_body();
        let project_contexts = self.resolve_project_contexts(db);
        let default_project_hash = self.default_project_hash(db, current_workdir);
        let mut resources = self.resolve_resource_entries(db);
        resources.extend(self.resolve_rag_resources(db, default_project_hash.as_deref()));

        // PROJECT CONTEXT leads the body, right below the system block.
        if let Some(section) = format_indexed_xml_section(
            "# [PROJECT CONTEXT]: Registered Project Metadata\n",
            "project_context",
            "project",
            &project_contexts,
        ) {
            body = format!("{section}{body}");
        }

        // Resolved resources replace the raw resources section and close the body.
        if let Some(section) = format_indexed_xml_section(
            "# [RESOURCES]: Knowledge Base & Data\n",
            "resources",
            "resource",
            &resources,
        ) {
            body = strip_resources_section(&body);
            body.push_str(&section);
        }

        // System block stays first so its protocol is read before the task.
        Ok(format!("{system}{body}"))
    }

    /// The invisible system block, rendered so its operating protocol is the
    /// first thing the agent reads — the session-start protocol once at the
    /// top when it rides along, then the per-turn context (see
    /// `system_text`). Empty when there is neither to send.
    pub(super) fn render_system_block(&self) -> String {
        let system = self.system_text();
        if system.is_empty() {
            return String::new();
        }
        format!("<system>\n{system}\n</system>\n\n")
    }

    /// Build the prompt body (every section except the system block).
    pub(super) fn build_body(&self) -> String {
        let mut result = String::new();
        self.append_prompt_section(
            &mut result,
            "goal",
            "# [GOAL]: Desired Outcome\n",
            "goal",
            "goal_item",
        );
        self.append_prompt_section(
            &mut result,
            "context",
            "# [CONTEXT]: Project Background\n",
            "context",
            "context",
        );
        self.append_instruction_section(&mut result);
        self.append_prompt_section(
            &mut result,
            "resources",
            "# [RESOURCES]: Knowledge Base & Data\n",
            "resources",
            "resource",
        );
        self.append_prompt_section(
            &mut result,
            "constraints",
            "# [CONSTRAINTS]: Behavioral Boundaries\n",
            "constraints",
            "constraint",
        );
        self.append_tools_section(&mut result);
        result
    }
}

pub(super) fn append_tool_skill(result: &mut String, tool_line: &str) {
    if let Some(name) = tool_line.strip_prefix("canopy:") {
        result.push_str("  <skill>\n");
        result.push_str(&format!(
            "    canopy skill {name} — load it with skill_get(name=\"{name}\")\n"
        ));
        result.push_str("  </skill>\n\n");
    } else {
        result.push_str("  <skill>\n");
        result.push_str(&format!("    {tool_line}\n"));
        result.push_str("  </skill>\n\n");
    }
}

pub(super) fn strip_resources_section(prompt: &str) -> String {
    let header = "# [RESOURCES]: Knowledge Base & Data\n<resources>\n";
    let Some(start) = prompt.find(header) else {
        return prompt.to_string();
    };
    let Some(end_rel) = prompt[start..].find("</resources>\n\n") else {
        return prompt.to_string();
    };

    let end = start + end_rel + "</resources>\n\n".len();
    let mut stripped = String::with_capacity(prompt.len().saturating_sub(end - start));
    stripped.push_str(&prompt[..start]);
    stripped.push_str(&prompt[end..]);
    stripped
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

// XML formatting helpers

pub(super) fn push_xml_item(result: &mut String, tag: &str, content: &str) {
    result.push_str(&format!("  <{tag}>\n"));
    for line in content.lines() {
        result.push_str(&format!("    {line}\n"));
    }
    result.push_str(&format!("  </{tag}>\n\n"));
}

pub(super) fn format_indexed_xml_section(
    header: &str,
    outer_tag: &str,
    item_tag: &str,
    items: &[String],
) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let mut result = String::new();
    result.push_str(header);
    result.push_str(&format!("<{outer_tag}>\n"));
    for item in items.iter() {
        push_xml_item(&mut result, item_tag, item);
    }
    result.push_str(&format!("</{outer_tag}>\n\n"));
    Some(result)
}

/// Build a wrapped XML section (header + outer tag + items) from matching section IDs.
pub(super) fn build_xml_block<'a>(
    result: &mut String,
    sections: &'a [String],
    matches: impl Fn(&str) -> bool,
    content_for: impl Fn(&'a str) -> Option<&'a str>,
    header: &str,
    outer_tag: &str,
    item_tag: &str,
) {
    let mut count = 0;
    for id in sections {
        if !matches(id) {
            continue;
        }
        let Some(content) = content_for(id) else {
            continue;
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        if count == 0 {
            result.push_str(header);
            result.push_str(&format!("<{outer_tag}>\n"));
        }
        count += 1;
        push_xml_item(result, item_tag, trimmed);
    }
    if count > 0 {
        result.push_str(&format!("</{outer_tag}>\n\n"));
    }
}
