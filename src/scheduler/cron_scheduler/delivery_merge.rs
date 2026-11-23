//! Rendering and packing merged scheduled-send deliveries.
//!
//! Pure text logic only: ordering parts, origin lines, separators, and the
//! size caps. The delivery task in `scheduled_prompts` resolves display
//! names and performs the single paste-and-submit write.

use crate::db::scheduled_prompts::ScheduledPrompt;

/// Most parts one merged message may hold (FR5).
pub(crate) const MAX_PARTS_PER_MESSAGE: usize = 20;
/// Most bytes one merged message may hold, separators and origin lines
/// included (FR5).
pub(crate) const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Registry default for `scheduler.delivery_merge_window_s`, read here so
/// the delivery entry point and the scheduler agree on one number.
pub(crate) fn default_merge_window_s() -> u64 {
    u64::try_from(crate::domain::settings::default_int(
        "scheduler.delivery_merge_window_s",
    ))
    .unwrap_or(60)
}

/// One due send plus the display names the delivery task resolved for it.
pub(crate) struct ResolvedPart<'a> {
    pub send: &'a ScheduledPrompt,
    pub graph_name: Option<String>,
    pub session_name: Option<String>,
    pub agent_name: Option<String>,
}

/// Order parts `fire_at`-first, ties by `created_at`, then by id so sends
/// sharing one truncated second still come out deterministically.
pub(crate) fn sort_parts(parts: &mut [ResolvedPart<'_>]) {
    parts.sort_by(|a, b| {
        (a.send.fire_at, a.send.created_at, &a.send.id).cmp(&(
            b.send.fire_at,
            b.send.created_at,
            &b.send.id,
        ))
    });
}

/// The origin line before a part's prompt, by provenance kind (FR3).
/// Display names fall back to their ids; a send without provenance is a
/// prompt-builder send and names its own `fire_at`.
pub(crate) fn origin_line(part: &ResolvedPart<'_>) -> String {
    let send = part.send;
    match part.send.provenance.as_ref().map(|p| p.kind.as_str()) {
        Some("hook") => {
            let event = &send.provenance.as_ref().expect("matched Some").event;
            let graph = graph_display(part.graph_name.as_deref(), &send_graph_id(send));
            format!("from: graph hook {graph} ({event})")
        }
        Some("agent") => {
            let session = part
                .session_name
                .as_deref()
                .or(session_id_of(send))
                .unwrap_or(&send.target_session_id);
            format!("from: session {session}")
        }
        Some("agent_trigger") => {
            let prov = send.provenance.as_ref().expect("matched Some");
            let agent = part
                .agent_name
                .as_deref()
                .or(prov.agent_id.as_deref())
                .unwrap_or("");
            let trigger = prov.trigger.as_deref().unwrap_or("trigger");
            format!("from: background agent {agent} ({trigger})")
        }
        Some("spec_review") => {
            let graph = graph_display(part.graph_name.as_deref(), &send_graph_id(send));
            format!("from: spec review (graph {graph})")
        }
        Some("deliver") => {
            let graph = graph_display(part.graph_name.as_deref(), &send_graph_id(send));
            format!("from: deliver (graph {graph})")
        }
        _ => {
            let fire_at = send
                .fire_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            format!("from: you (scheduled {fire_at})")
        }
    }
}

fn send_graph_id(send: &ScheduledPrompt) -> String {
    send.provenance
        .as_ref()
        .map(|p| p.graph_id.clone())
        .unwrap_or_default()
}

fn session_id_of(send: &ScheduledPrompt) -> Option<&str> {
    send.provenance
        .as_ref()
        .and_then(|p| p.session_id.as_deref())
}

/// A resolved name, falling back to the id when unresolvable (FR3).
fn graph_display(resolved: Option<&str>, id: &str) -> String {
    resolved
        .filter(|name| !name.is_empty())
        .unwrap_or(id)
        .to_string()
}

/// One part's block: separator, origin, then the prompt text unchanged.
pub(crate) fn render_block(index: usize, total: usize, part: &ResolvedPart<'_>) -> String {
    format!(
        "--- delivery {} of {} ---\n{}\n{}",
        index + 1,
        total,
        origin_line(part),
        part.send.prompt
    )
}

/// Join blocks into one message.
pub(crate) fn render_message(blocks: &[String]) -> String {
    blocks.join("\n")
}

/// Split block indices into consecutive messages holding at most
/// `MAX_PARTS_PER_MESSAGE` parts and `MAX_MESSAGE_BYTES` bytes each,
/// preserving order. A single part larger than the byte cap goes alone in
/// its own message, never split and never truncated.
pub(crate) fn pack_blocks(blocks: &[String]) -> Vec<Vec<usize>> {
    let mut messages: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_len: usize = 0;
    for (index, block) in blocks.iter().enumerate() {
        if !current.is_empty() && !block_fits(&current, current_len, block.len()) {
            messages.push(std::mem::take(&mut current));
            current_len = 0;
        }
        current.push(index);
        current_len += block.len() + usize::from(current.len() > 1);
    }
    if !current.is_empty() {
        messages.push(current);
    }
    messages
}

/// Whether `block_len` still fits the open message: room for one more part
/// and the joined bytes (including this block's joining newline) within the
/// byte cap.
fn block_fits(current: &[usize], current_len: usize, block_len: usize) -> bool {
    current.len() < MAX_PARTS_PER_MESSAGE && current_len + 1 + block_len <= MAX_MESSAGE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(id: &str, prompt: &str) -> ScheduledPrompt {
        let at = chrono::Utc::now();
        ScheduledPrompt {
            id: id.to_string(),
            prompt: prompt.to_string(),
            target_session_id: "sess".to_string(),
            workdir: None,
            fire_at: at,
            created_at: at,
            builder_state: None,
            provenance: None,
        }
    }

    fn part<'a>(send: &'a ScheduledPrompt) -> ResolvedPart<'a> {
        ResolvedPart {
            send,
            graph_name: None,
            session_name: None,
            agent_name: None,
        }
    }

    #[test]
    fn hook_origin_names_graph_and_event() {
        use crate::db::scheduled_prompts::ScheduledPromptProvenance;
        let mut owned = send("a", "hi");
        owned.provenance = Some(ScheduledPromptProvenance::hook("g-1", "on_failed"));
        let resolved = ResolvedPart {
            send: &owned,
            graph_name: Some("canopy-next".to_string()),
            session_name: None,
            agent_name: None,
        };
        assert_eq!(
            origin_line(&resolved),
            "from: graph hook canopy-next (on_failed)"
        );
    }

    #[test]
    fn graph_origin_falls_back_to_the_full_id() {
        use crate::db::scheduled_prompts::ScheduledPromptProvenance;
        let mut owned = send("a", "hi");
        owned.provenance = Some(ScheduledPromptProvenance::hook(
            "969ee102-0138-4054-95fd-2946ef237431",
            "on_completed",
        ));
        let unresolved = part(&owned);
        assert_eq!(
            origin_line(&unresolved),
            "from: graph hook 969ee102-0138-4054-95fd-2946ef237431 (on_completed)"
        );
    }

    #[test]
    fn agent_origin_names_the_calling_session() {
        use crate::db::scheduled_prompts::ScheduledPromptProvenance;
        let mut owned = send("a", "hi");
        owned.provenance = Some(ScheduledPromptProvenance::agent("caller-id"));
        let resolved = ResolvedPart {
            send: &owned,
            graph_name: None,
            session_name: Some("work".to_string()),
            agent_name: None,
        };
        assert_eq!(origin_line(&resolved), "from: session work");
    }

    #[test]
    fn session_origin_falls_back_to_the_session_id() {
        use crate::db::scheduled_prompts::ScheduledPromptProvenance;
        let mut owned = send("a", "hi");
        owned.provenance = Some(ScheduledPromptProvenance::agent("caller-id"));
        let unresolved = part(&owned);
        assert_eq!(origin_line(&unresolved), "from: session caller-id");
    }

    #[test]
    fn trigger_origin_names_agent_and_trigger() {
        use crate::db::scheduled_prompts::ScheduledPromptProvenance;
        let mut owned = send("a", "hi");
        owned.provenance = Some(ScheduledPromptProvenance::agent_trigger("nudger", "cron"));
        let resolved = ResolvedPart {
            send: &owned,
            graph_name: None,
            session_name: None,
            agent_name: Some("nudger".to_string()),
        };
        assert_eq!(
            origin_line(&resolved),
            "from: background agent nudger (cron)"
        );
    }

    #[test]
    fn spec_review_and_deliver_origins_name_the_graph() {
        use crate::db::scheduled_prompts::ScheduledPromptProvenance;
        let mut review = send("a", "hi");
        review.provenance = Some(ScheduledPromptProvenance {
            kind: "spec_review".to_string(),
            graph_id: "g-9".to_string(),
            event: String::new(),
            session_id: None,
            agent_id: None,
            trigger: None,
            target_session_name: None,
            node_id: None,
            run_id: None,
        });
        let resolved = ResolvedPart {
            send: &review,
            graph_name: Some("review-graph".to_string()),
            session_name: None,
            agent_name: None,
        };
        assert_eq!(
            origin_line(&resolved),
            "from: spec review (graph review-graph)"
        );

        let mut deliver = send("b", "hi");
        deliver.provenance = Some(ScheduledPromptProvenance::deliver("g-9", "n-1", "r-1"));
        let resolved = ResolvedPart {
            send: &deliver,
            graph_name: Some("review-graph".to_string()),
            session_name: None,
            agent_name: None,
        };
        assert_eq!(origin_line(&resolved), "from: deliver (graph review-graph)");
    }

    #[test]
    fn prompt_builder_origin_names_the_rows_fire_at() {
        let mut owned = send("a", "hi");
        let fire_at = chrono::DateTime::from_timestamp(1_800_000_000, 0).expect("valid");
        owned.fire_at = fire_at;
        let unresolved = part(&owned);
        assert_eq!(
            origin_line(&unresolved),
            "from: you (scheduled 2027-01-15T08:00:00Z)"
        );
    }

    #[test]
    fn parts_sort_by_fire_at_then_created_at() {
        let mut first = send("id-b", "first");
        first.fire_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let mut second = send("id-a", "second");
        second.fire_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        let mut parts = [part(&second), part(&first)];
        sort_parts(&mut parts);
        assert_eq!(parts[0].send.prompt, "first");
        assert_eq!(parts[1].send.prompt, "second");
    }

    #[test]
    fn blocks_carry_separators_and_unchanged_prompts() {
        let a = send("a", "AAA");
        let b = send("b", "BBB");
        let parts = [part(&a), part(&b)];
        let blocks: Vec<String> = parts
            .iter()
            .enumerate()
            .map(|(i, p)| render_block(i, parts.len(), p))
            .collect();
        let message = render_message(&blocks);
        assert!(message.contains("--- delivery 1 of 2 ---"));
        assert!(message.contains("--- delivery 2 of 2 ---"));
        assert!(message.contains("AAA"));
        assert!(message.contains("BBB"));
        assert!(message.find("AAA").expect("first") < message.find("BBB").expect("second"));
    }

    #[test]
    fn twenty_five_parts_pack_into_twenty_plus_five() {
        let blocks: Vec<String> = (0..25).map(|i| format!("block-{i}")).collect();
        let packed = pack_blocks(&blocks);
        assert_eq!(packed.len(), 2);
        assert_eq!(packed[0].len(), 20);
        assert_eq!(packed[1].len(), 5);
        assert_eq!(packed[0][0], 0);
        assert_eq!(packed[1][0], 20);
    }

    #[test]
    fn message_over_sixty_four_kib_starts_a_new_message() {
        let big = "x".repeat(40 * 1024);
        let blocks = vec![big.clone(), big];
        let packed = pack_blocks(&blocks);
        assert_eq!(packed.len(), 2, "two 40 KiB parts never share a message");
    }

    #[test]
    fn oversize_single_part_goes_alone_and_intact() {
        let big = "y".repeat(100 * 1024);
        let blocks = vec!["small".to_string(), big.clone(), "tail".to_string()];
        let packed = pack_blocks(&blocks);
        assert_eq!(packed.len(), 3);
        assert_eq!(packed[1], vec![1]);
        let message = render_message(
            &packed[1]
                .iter()
                .map(|i| blocks[*i].clone())
                .collect::<Vec<_>>(),
        );
        assert_eq!(message, big);
    }
}
