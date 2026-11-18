use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use tokio::sync::Notify;

use crate::db::scheduled_prompts::ScheduledPromptProvenance;
use crate::db::Database;
use crate::domain::graphs::{GraphSpec, SpecReviewState, SpecReviewTier};

const MAX_MESSAGE_CHARS: usize = 6000;
const MORE_SUFFIX_PREFIX: &str = "(+";
const MORE_SUFFIX: &str = " more findings: read them with the spec tool)";

/// Try to drain finished spec reviews for `session_id` and enqueue one
/// batched scheduled send. No-op when the session is pending, the finished
/// set is empty, or the session id is `None`.
pub fn drain_if_ready(
    db: &Database,
    session_id: Option<&str>,
    scheduled_prompt_notify: Option<&Arc<Notify>>,
) -> Result<()> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let Some(specs) = db.claim_spec_review_drain(session_id)? else {
        return Ok(());
    };
    if specs.is_empty() {
        return Ok(());
    }
    let prompt = format_review_message(&specs);
    let Some(workdir) = db.get_session_workdir(session_id)? else {
        // Session row gone before we could resolve workdir: preserve as
        // dead-target delivery by inserting with no workdir? insert_failed
        // path is handled by delivery loop when target not alive; inserting a
        // scheduled prompt with unknown workdir still lets delivery preserve
        // the prompt text. But get_session_workdir returning None means we
        // can't target delivery meaningfully; keep it recallable by storing a
        // failed prompt record would require different handling. However the
        // send must go through scheduled-send path; insert with no workdir.
        // Delivery will see target not alive and preserve with workdir None.
        enqueue_review_send(db, session_id, None, &prompt, scheduled_prompt_notify)?;
        return Ok(());
    };
    enqueue_review_send(
        db,
        session_id,
        Some(&workdir),
        &prompt,
        scheduled_prompt_notify,
    )?;
    Ok(())
}

fn enqueue_review_send(
    db: &Database,
    session_id: &str,
    workdir: Option<&str>,
    prompt: &str,
    scheduled_prompt_notify: Option<&Arc<Notify>>,
) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    db.insert_scheduled_prompt(
        &id,
        prompt,
        session_id,
        workdir,
        Utc::now(),
        None,
        Some(&ScheduledPromptProvenance::spec_review()),
    )?;
    if let Some(notify) = scheduled_prompt_notify {
        notify.notify_one();
    }
    Ok(())
}

pub fn format_review_message(specs: &[GraphSpec]) -> String {
    let n = specs.len();
    let mut approved_trivial = 0usize;
    let mut approved_normal = 0usize;
    let mut rejected = 0usize;
    let mut errors = 0usize;
    for spec in specs {
        match spec.review_state {
            SpecReviewState::Approved => match spec.review_tier {
                Some(SpecReviewTier::Trivial) => approved_trivial += 1,
                _ => approved_normal += 1,
            },
            SpecReviewState::Rejected => rejected += 1,
            SpecReviewState::ReviewError => errors += 1,
            _ => {}
        }
    }
    let approved = approved_trivial + approved_normal;
    let header = format!(
        "Spec review finished: {n} reviewed, {approved} approved ({approved_trivial} trivial, {approved_normal} normal), {rejected} rejected, {errors} errors."
    );

    let mut ordered: Vec<&GraphSpec> = specs.iter().collect();
    ordered.sort_by_key(|s| match s.review_state {
        SpecReviewState::Rejected => 0,
        SpecReviewState::ReviewError => 1,
        SpecReviewState::Approved if s.review_findings.as_ref().is_some_and(|f| !f.is_empty()) => 2,
        _ => 3,
    });

    // Render blocks with full findings, then trim to cap.
    let mut blocks: Vec<SpecBlock> = ordered.into_iter().map(SpecBlock::from_spec).collect();
    let mut msg = render_message(&header, &blocks);
    if msg.chars().count() <= MAX_MESSAGE_CHARS {
        return msg;
    }

    // Drop low-severity findings first, then medium.
    let mut dropped = 0usize;
    for sev in ["low", "medium"] {
        for block in &mut blocks {
            dropped += block.drop_severity(sev);
        }
        msg = render_message_with_more(&header, &blocks, dropped);
        if msg.chars().count() <= MAX_MESSAGE_CHARS {
            return msg;
        }
    }

    // Still too long: drop remaining findings and keep suffix.
    for block in &mut blocks {
        dropped += block.drop_all();
    }
    let mut msg = render_message_with_more(&header, &blocks, dropped);
    if msg.chars().count() > MAX_MESSAGE_CHARS {
        // Last resort: truncate hard but keep suffix intent.
        msg = truncate_chars(&msg, MAX_MESSAGE_CHARS);
    }
    msg
}

#[derive(Clone)]
struct Finding {
    severity: String,
    text: String,
}

#[derive(Clone)]
struct SpecBlock {
    name: String,
    state: SpecReviewState,
    tier: Option<SpecReviewTier>,
    findings: Vec<Finding>,
}

impl SpecBlock {
    fn from_spec(spec: &GraphSpec) -> Self {
        let findings = spec
            .review_findings
            .as_ref()
            .map(|fs| {
                fs.iter()
                    .map(|f| Finding {
                        severity: f.severity.clone(),
                        text: f.text.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            name: spec.name.clone(),
            state: spec.review_state,
            tier: spec.review_tier,
            findings,
        }
    }

    fn drop_severity(&mut self, sev: &str) -> usize {
        let before = self.findings.len();
        self.findings.retain(|f| f.severity != sev);
        before - self.findings.len()
    }

    fn drop_all(&mut self) -> usize {
        let n = self.findings.len();
        self.findings.clear();
        n
    }

    fn render(&self) -> String {
        let tier_part = match (self.state, self.tier) {
            (SpecReviewState::Rejected, _) | (SpecReviewState::ReviewError, _) => String::new(),
            (_, Some(t)) => format!(" [{}]", t.as_str()),
            _ => String::new(),
        };
        let mut out = format!("{} -> {}{tier_part}", self.name, self.state.as_str());
        for f in &self.findings {
            out.push_str(&format!("\n  [{}] {}", f.severity, f.text));
        }
        out
    }
}

fn render_message(header: &str, blocks: &[SpecBlock]) -> String {
    let mut parts = vec![header.to_string()];
    for b in blocks {
        parts.push(b.render());
    }
    parts.join("\n")
}

fn render_message_with_more(header: &str, blocks: &[SpecBlock], dropped: usize) -> String {
    let mut msg = render_message(header, blocks);
    if dropped > 0 {
        msg.push('\n');
        msg.push_str(&format!("{MORE_SUFFIX_PREFIX}{dropped}{MORE_SUFFIX}"));
    }
    msg
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
