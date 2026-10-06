use super::*;

impl IngestionManager {
    pub(super) async fn index_file(&self, source_path: &str) -> anyhow::Result<()> {
        let path = std::path::Path::new(source_path);

        if !path.exists() {
            purge_vector_chunks(&self.data_dir, source_path, Some(&self.db)).await;
            return Ok(());
        }

        // Loaded fresh (not cached on `self`) so a `rag_max_file_mb` edit in
        // config.toml takes effect on this file's very next indexing pass —
        // no daemon restart needed.
        let config = crate::domain::canopy_config::CanopyConfig::load(&self.data_dir);
        let max_bytes = config.rag_max_file_bytes();

        let meta = std::fs::metadata(path)?;
        if meta.len() > max_bytes {
            warn_and_record_oversize(&self.db, source_path, meta.len(), max_bytes);
            return Ok(());
        }

        let Some(lang) = detect_lang(source_path) else {
            return Ok(());
        };

        let Some(content) = read_indexable_content(source_path, path, lang)? else {
            return Ok(());
        };
        let now = chrono::Utc::now().timestamp();

        let Some(vector_chunks) = self
            .embed_file_chunks(&config, source_path, lang, content, now)
            .await?
        else {
            return Ok(());
        };

        tracing::info!(
            "RAG index_file: {source_path} — {} chunk(s) embedded, pushing to vector store",
            vector_chunks.len()
        );
        sync_vector_store(
            &self.db,
            &self.data_dir,
            &config,
            source_path,
            &vector_chunks,
        )
        .await?;
        Ok(())
    }

    /// Semantic-chunk `content`, obtain the embedding client, and embed every
    /// chunk. Returns `Ok(None)` when chunking produced nothing to embed or
    /// embedding produced no vectors (the file simply isn't indexed), and
    /// `Err` when the embedding client itself is unavailable.
    pub(super) async fn embed_file_chunks(
        &self,
        config: &crate::domain::canopy_config::CanopyConfig,
        source_path: &str,
        lang: &'static str,
        content: String,
        now: i64,
    ) -> anyhow::Result<Option<Vec<VectorChunk>>> {
        let threshold = config.similarity_threshold;
        let Some(semantic_chunks) =
            build_semantic_chunks(content.clone(), lang.to_owned(), threshold, source_path).await
        else {
            return Ok(None);
        };
        tracing::debug!(
            "RAG index_file: {source_path} — {} semantic chunk(s) produced",
            semantic_chunks.len()
        );
        let embedding_client = self.embedding_client_for_file(config, source_path).await?;

        let vector_chunks =
            embed_semantic_chunks(embedding_client, semantic_chunks, source_path, now).await;

        if vector_chunks.is_empty() {
            tracing::error!(
                "RAG: no chunks were embedded for {source_path} — file will not be indexed"
            );
            return Ok(None);
        }

        Ok(Some(vector_chunks))
    }

    /// Obtain the embedding client for one indexing pass, logging the same
    /// readiness or failure line the indexing path always emitted; surfaces
    /// the client error unchanged so `index_file`'s `?` reports it.
    pub(super) async fn embedding_client_for_file(
        &self,
        config: &crate::domain::canopy_config::CanopyConfig,
        source_path: &str,
    ) -> anyhow::Result<Arc<dyn EmbeddingClient>> {
        match self.get_embedding_client(config).await {
            Ok(client) => {
                tracing::debug!("RAG index_file: embedding client ready for {source_path}");
                Ok(client)
            }
            Err(error) => {
                tracing::error!(
                    "RAG: cannot index {source_path} — embedding client unavailable: {error:#}"
                );
                Err(error)
            }
        }
    }
}

pub(super) async fn build_semantic_chunks(
    content: String,
    lang: String,
    threshold: f32,
    source_path: &str,
) -> Option<Vec<SemanticChunk>> {
    let chunk_result = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            chunk_semantic(&content, &lang, threshold)
        }))
    })
    .await;

    match chunk_result {
        Ok(Ok(chunks)) => Some(chunks),
        Ok(Err(panic)) => {
            tracing::error!("RAG chunker panic for {source_path}: {panic:?}");
            None
        }
        Err(join_err) => {
            tracing::error!("RAG chunker task failed for {source_path}: {join_err}");
            None
        }
    }
}

pub(super) async fn embed_semantic_chunks(
    embedding_client: Arc<dyn EmbeddingClient>,
    semantic_chunks: Vec<SemanticChunk>,
    source_path: &str,
    created_at: i64,
) -> Vec<VectorChunk> {
    let mut vector_chunks = Vec::with_capacity(semantic_chunks.len());

    for chunk in semantic_chunks {
        let content = chunk.content;
        let chunk_id = Uuid::new_v4().to_string();
        let client = Arc::clone(&embedding_client);
        let content_for_embedding = content.clone();
        let embedding_result =
            tokio::task::spawn_blocking(move || client.embed(&content_for_embedding)).await;

        let embedding = match embedding_result {
            Ok(Ok(values)) => {
                tracing::debug!(
                    "RAG: embedded chunk {} of {source_path} → {} dims",
                    chunk.index,
                    values.len()
                );
                values
            }
            Ok(Err(error)) => {
                tracing::error!(
                    "RAG embedding failed for {source_path} chunk {}: {error}",
                    chunk.index
                );
                continue;
            }
            Err(error) => {
                tracing::error!(
                    "RAG embedding task panicked for {source_path} chunk {}: {error}",
                    chunk.index
                );
                continue;
            }
        };

        vector_chunks.push(VectorChunk {
            id: chunk_id,
            file_path: source_path.to_owned(),
            content,
            embedding,
            created_at,
        });
    }

    vector_chunks
}
