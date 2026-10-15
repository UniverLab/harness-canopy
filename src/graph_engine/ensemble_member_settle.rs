//! Round-robin settlement helpers for [`super::ensemble_member`]. Split into a
//! sibling module to keep that file within the size gate.

use super::*;

/// The per-dispatch setup a round-robin walk starts from.
pub(crate) struct RoundRobinSetup<'a> {
    pub(crate) member_count: i64,
    pub(crate) start_index: i64,
    pub(crate) initial_resume_candidate: Option<String>,
    pub(crate) next_index: i64,
    pub(crate) available_members: HashSet<&'a str>,
    pub(crate) member_nodes: HashMap<String, GraphNode>,
    pub(crate) commit_watch: Option<CommitRightsWatch>,
}

/// The values a winning round-robin member's join needs to settle.
pub(crate) struct RoundRobinSettle<'a> {
    pub(crate) join_ctx: &'a EnsembleJoinCtx<'a>,
    pub(crate) ensemble: &'a crate::domain::graphs::Ensemble,
    pub(crate) details: &'a EnsembleDetails,
    pub(crate) member: &'a EnsembleMember,
    pub(crate) node: &'a GraphNode,
    pub(crate) execution: NodeExecution,
    pub(crate) members_ran: i64,
    pub(crate) skipped_unavailable: bool,
    pub(crate) next_after_winner: i64,
    pub(crate) next_index: i64,
    pub(crate) commit_watch: &'a Option<CommitRightsWatch>,
    pub(crate) workdir: &'a str,
}

/// The values the no-verdict tail of a round-robin walk needs to settle.
pub(crate) struct RoundRobinWalkCtx<'a> {
    pub(crate) join_ctx: &'a EnsembleJoinCtx<'a>,
    pub(crate) commit_watch: &'a Option<CommitRightsWatch>,
    pub(crate) workdir: &'a str,
    pub(crate) ensemble: &'a crate::domain::graphs::Ensemble,
}

impl GraphEngine {
    /// Resolve everything a round-robin walk needs before its first step:
    /// the member count, the rotation start cursor, the available-member set,
    /// the member nodes, and the commit watch.
    pub(crate) async fn prepare_round_robin<'a>(
        &self,
        details: &'a EnsembleDetails,
        available_members: &'a [EnsembleMember],
        enforce_commit_rights: bool,
        workdir: &str,
        round_robin_pins: &mut HashMap<String, i64>,
        resumable_sessions: &mut HashMap<String, String>,
    ) -> Result<RoundRobinSetup<'a>> {
        let ensemble = &details.ensemble;
        // CM48: the FULL member list is the rotation's index space. A member
        // that is unavailable now is stepped over inside the walk, so the
        // stored `round_robin_index` keeps naming the same member whatever
        // the roster says today.
        let member_count = details.members.len() as i64;
        if member_count == 0 {
            return Err(anyhow!(
                "Round-robin ensemble '{}' has no members to run.",
                ensemble.id
            ));
        }
        let available: HashSet<&str> = available_members
            .iter()
            .map(|member| member.node_id.as_str())
            .collect();
        let (start_index, initial_resume_candidate, next_index) = self.resolve_round_robin_start(
            details,
            member_count,
            round_robin_pins,
            resumable_sessions,
        )?;
        let member_nodes = load_ensemble_member_nodes(&self.db, details)?;
        let commit_watch = CommitRightsWatch::begin(
            enforce_commit_rights,
            details.ensemble.commit_rights,
            workdir,
        )
        .await;
        Ok(RoundRobinSetup {
            member_count,
            start_index,
            initial_resume_candidate,
            next_index,
            available_members: available,
            member_nodes,
            commit_watch,
        })
    }

    /// Settle the round-robin walk on the member that won: build the join
    /// execution, apply the commit watch, and record the join.
    pub(crate) async fn settle_round_robin_winner(
        &self,
        s: RoundRobinSettle<'_>,
    ) -> Result<NodeExecution> {
        let next_reported = self.store_round_robin_winner_index(
            s.ensemble,
            s.skipped_unavailable,
            s.next_after_winner,
            s.next_index,
        )?;
        let join_execution = round_robin_winner_execution(
            s.ensemble,
            s.details,
            s.member,
            s.node,
            &s.execution,
            // The members actually run so far, minus this one — a member
            // stepped over as unavailable is not "tried".
            s.members_ran - 1,
            next_reported,
        );
        let join_execution = apply_round_robin_commit_watch(
            join_execution,
            s.commit_watch,
            s.workdir,
            format!("Ensemble '{}' (round-robin member)", s.ensemble.name),
            &s.ensemble.join_node_id,
        )
        .await;
        self.record_ensemble_join(s.join_ctx, &join_execution)
            .await?;
        Ok(join_execution)
    }

    /// Store the rotation's next start index.
    ///
    /// CM48: `round_robin_index` is stored as the index actually used. The
    /// eager walk-start write (`start + 1`) predicts the member after the
    /// first one tried; once the walk has stepped over unavailable members
    /// that prediction no longer names the member after the one that won — it
    /// can point at the winner itself (dispatching it twice in a row) or at a
    /// member that never ran. Re-store it from the member that actually won,
    /// so the next dispatch's rotation continues after it and the stored index
    /// agrees with the join output. An empty roster skips nothing, so the
    /// stored index is exactly today's.
    pub(crate) fn store_round_robin_winner_index(
        &self,
        ensemble: &crate::domain::graphs::Ensemble,
        skipped_unavailable: bool,
        next_after_winner: i64,
        next_index: i64,
    ) -> Result<i64> {
        let next_reported = if skipped_unavailable {
            self.db
                .update_ensemble_kind(&ensemble.id, None, Some(Some(next_after_winner)))?;
            next_after_winner
        } else {
            next_index
        };
        Ok(next_reported)
    }

    /// Settle a round-robin walk that ran every member without a verdict.
    pub(crate) async fn settle_round_robin_no_verdict(
        &self,
        walk: &RoundRobinWalkCtx<'_>,
        details: &EnsembleDetails,
        tried_no_verdict: &[(String, String, Value, String)],
        next_index: i64,
    ) -> Result<NodeExecution> {
        let execution =
            round_robin_no_verdict_execution(walk.ensemble, details, tried_no_verdict, next_index);
        let execution = apply_round_robin_commit_watch(
            execution,
            walk.commit_watch,
            walk.workdir,
            format!("Ensemble '{}' (round-robin)", walk.ensemble.name),
            &walk.ensemble.join_node_id,
        )
        .await;
        self.record_ensemble_join(walk.join_ctx, &execution).await?;
        Ok(execution)
    }

    /// CB82: the interrupted execution for a round-robin member stepped over
    /// during daemon shutdown.
    pub(crate) fn shutdown_member_interruption(
        &self,
        ensemble: &crate::domain::graphs::Ensemble,
        member: &EnsembleMember,
    ) -> NodeExecution {
        self.shutdown_interrupted_member_execution(
            &member.node_id,
            format!(
                "Ensemble '{}' (round-robin member) interrupted: daemon shutdown.",
                ensemble.name
            ),
        )
    }
}
