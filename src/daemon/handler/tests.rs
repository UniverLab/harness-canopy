//! `tests` tests (split from `src/daemon/handler.rs`, pure move).
use super::*;

use super::{
    active_graph_spec_context, build_ensemble_unit, build_get_tools_response,
    build_graph_completion_hook, build_graph_summary_json, build_graph_trigger,
    build_graph_update_response, build_id_result, build_json_result, build_spec_update_response,
    effective_member_prompt, find_in_flight_run, graph_details_json, graph_run_status_guard,
    graph_trigger_json, handle_retry_current_node, handle_skip_next_spec, header_str,
    in_flight_run_error, json_value_kind_name, member_node_config, missing_sync_identity_error,
    node_copy_note, node_name_for_error, perform_graph_reset, plan_ensemble_copy, plan_node_copy,
    queue_running_spec, rag_result_json, resolve_graph_target, resolve_node_kind_and_config,
    resolve_reported_run, spec_summary_json, unknown_config_keys, validate_absolute_dir,
    validate_at_least_one_bool, validate_blueprint_exists, validate_edge_condition,
    validate_edge_condition_with_route, validate_ensemble_members, validate_node_config,
    validate_node_kind, validate_node_not_ensemble_owned, validate_non_empty,
    validate_not_join_kind, validate_queue_exists, validate_queue_member_removable,
    validate_queue_not_consumed, validate_queue_reorder, validate_queue_reorder_locking,
    validate_route_edge_target, validate_spec_deletable, validate_spec_exists,
    validate_spec_set_status_target, validate_spec_status, validate_spec_workdir, ActiveGraphSpec,
    BuiltEnsembleUnit, EnsembleMemberParams, EnsembleUnitSpec, HandlerDeps, TaskTriggerHandler,
    MISSING_SYNC_IDENTITY_MESSAGE,
};
use crate::daemon::params::{
    GraphCompletionHookParams, GraphCopyEnsembleParams, GraphCopyNodeParams,
    GraphScheduleAutorunParams, GraphScheduleContinueParams, GraphTriggerParams,
};
use crate::db::Database;
use crate::domain::blueprints::Blueprint;
use crate::domain::graphs::{
    Ensemble, EnsembleKind, EnsembleMember, Graph, GraphEdge, GraphEdgeCondition, GraphNode,
    GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
};
use crate::domain::models::Trigger;
use crate::domain::queues::Queue;
use crate::shared::sync_identity::CANOPY_AGENT_ID_HEADER;
use tempfile::tempdir;

fn ensemble_member_params(platform: &str) -> EnsembleMemberParams {
    EnsembleMemberParams {
        platform: platform.to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

fn ensemble_member_params_with_prompt(
    platform: &str,
    prompt_override: &str,
) -> EnsembleMemberParams {
    EnsembleMemberParams {
        platform: platform.to_string(),
        model: None,
        prompt_override: Some(prompt_override.to_string()),
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

/// B7 A1 frozen inventory: sorted MCP tool names. Any added, removed, or
/// renamed tool fails `tool_inventory_is_byte_identical`.
const EXPECTED_TOOL_NAMES: &[&str] = &[
    "agent_add",
    "agent_disable",
    "agent_enable",
    "agent_get",
    "agent_list",
    "agent_logs",
    "agent_models",
    "agent_probe",
    "agent_probe_recent",
    "agent_remove",
    "agent_report",
    "agent_run",
    "agent_schedule_enable",
    "agent_status",
    "agent_update",
    "agent_watch",
    "blueprint_create",
    "blueprint_delete",
    "blueprint_list",
    "config_get",
    "config_list",
    "config_reset",
    "config_set",
    "create_seed",
    "crew_create",
    "crew_delete",
    "crew_get",
    "crew_list",
    "crew_update",
    "evolve_identity",
    "get_identity",
    "get_tools",
    "graph",
    "graph_complete_node",
    "graph_control",
    "graph_crew",
    "graph_edge",
    "graph_edit",
    "graph_ensemble",
    "graph_node",
    "graph_report_blocker",
    "intelligence_delete_node",
    "intelligence_delete_relation",
    "intelligence_get_context",
    "intelligence_graph_walk",
    "intelligence_link_projects",
    "intelligence_list_projects",
    "intelligence_search",
    "intelligence_upsert",
    "list_seeds",
    "loop_complete_node",
    "loop_report_blocker",
    "mutex",
    "node_install",
    "node_list",
    "node_remove",
    "project_register",
    "project_remap",
    "project_search",
    "project_update",
    "queue_add_spec",
    "queue_create",
    "queue_list",
    "queue_remove_spec",
    "queue_reorder",
    "queue_set_group",
    "rag_compact",
    "rag_search",
    "registry_refresh",
    "remove_seed",
    "roster_check",
    "roster_clear",
    "roster_list",
    "roster_set",
    "scheduled_send_cancel",
    "scheduled_send_create",
    "scheduled_send_list",
    "session_list",
    "skill_get",
    "skill_list",
    "spec_convert",
    "spec_create",
    "spec_delete",
    "spec_list",
    "spec_note_write",
    "spec_notes_get",
    "spec_section_get",
    "spec_set_status",
    "spec_update",
    "subagent_collect",
    "subagent_spawn",
    "sync_broadcast",
    "sync_declare_intent",
    "sync_get_context",
    "sync_report_status",
];

/// B7 A1 frozen snapshot: per-tool `name|sha256(description)|sha256(input_schema JSON)`.
/// Any byte drift in a description or schema fails `tool_inventory_is_byte_identical`.
/// The schema hashes were re-pinned for rmcp 2.x, whose `validate_and_strip` drops the
/// root `title`/`description` of each input schema (rmcp 1.6 kept them). Properties,
/// required fields and tool descriptions are unchanged by that.
const EXPECTED_TOOL_ROWS: &[&str] = &[
    "agent_add|dce812557e165d8d06b92ab956bafaf4c4eccd29acd4216074adbce66d116570|dbbb12a5bbe342e026592882ae703d056cf0804f60271060da1195e4a167acd9",
    "agent_disable|a5d3befdd0358cdb34f4cbceba2ad730b8cb7968bea1f4d5af15095cfc3e5d16|aaaa4591bf27eb54bc4052fa58a07a3b5af8a671b8595c525bfe323462c90387",
    "agent_enable|812c3569335836df22c0021c4591ab1642a739c0be32ab921aa3a9dad35040f3|aaaa4591bf27eb54bc4052fa58a07a3b5af8a671b8595c525bfe323462c90387",
    "agent_get|efc57a83aaf2b2b93b249749d2af313ad14b7d5f78bc63dfeb4a8067069d81fb|aaaa4591bf27eb54bc4052fa58a07a3b5af8a671b8595c525bfe323462c90387",
    "agent_list|f12441b7b162701cc3feeead67ecc6adf610b4e28b3d2a2ba2eedd22d17b6b45|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "agent_logs|313a92b2669f743f4352fff989b30a666510fef1a7f6be65b4b17e8862d04acb|f89711d2401e85da80adc2b687e731c2cdff0fee8a6883ac3b36b52cd0ab2a61",
    "agent_models|e26acac42beb6e5abc68fd28473196c549864a9796e020a4ebf9d62f81606090|aec8a95e3baea87069ac61ae2a4c64f2f61261ba9e5bd7f409ffac07b7858c51",
    "agent_probe|b1c612dc5b845b72e90f81458538c0fb18cb07768105adb87b06590875904577|5464fa5580132a97b0a7e2129a2384de8337dae47e6009095aa8a03eb5982d94",
    "agent_probe_recent|665aea90717bcf46854edda2c7627d61c2242f960ae586d53185923e27f76e2e|8e49086ca9899f371ee52f4bebb5bfcdc745540d0bf78e3080d5f9ba2b4bde2c",
    "agent_remove|304a93b8e0d8f9957d7ff6e134154d1f67bc0e8920b5d7ff86874a93361d5de2|aaaa4591bf27eb54bc4052fa58a07a3b5af8a671b8595c525bfe323462c90387",
    "agent_report|998eb6d649fe53809656f0a4db81248aa0feb0b045f957686d917fbcfe289224|f40f986d373435cf862cf0e260812d92f78eaf4bfcfbe98ffce5d724a1685d54",
    "agent_run|39ae4bbed141b7635a135111a7cf767c2569bf36d7d2d64da8089091534706f1|aaaa4591bf27eb54bc4052fa58a07a3b5af8a671b8595c525bfe323462c90387",
    "agent_schedule_enable|ed852e08dc893d9cd6ae715374b0e695ee4516030ebe02b2e5ecb09aa0d98f1c|3a3c50b04564580555f0ab8dbf04f391912190d0fd86dfada5936f08f89bf5b3",
    "agent_status|d1054486453a2080fe05eb9ccbcd4a86aefe4894a2a93870b6b2bb9e8781b194|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "agent_update|5f6ad5332501fc94b77510ae0093dcf92d451b051a90b3843e3e134c2d029415|56e670152acde083334d2f34d983971b86174a67b3d47d2388036efe865d4bff",
    "agent_watch|b7aeeaf163d575665d5e2f9699687dede83db72e17a55d0b1aa0becd998aea87|29c1b840934a7296c55fa378446e948aa6e3479192ec924ef0bbf65f9ad2318d",
    "blueprint_create|6d5cbb1faea26fc441fadaf71dd8bb4e7eeba1681242a0c877606a91a42b0756|0b9d1b9ae8540677838ac428cdd03984d9ce3faab1a4e753bf4097068e7fd912",
    "blueprint_delete|1eaa169fb5798e6d4095b44d1784bfb6a390925ec2e8513ca4dbe542119b69fb|f1e32f55c6e9420ef2484a0af9a8aab3080b5c9df3d1b0e99e48be1a60bdee58",
    "blueprint_list|af473eda02aca08ea5d8f980c9fe0558057c2821a2afe3ff5f72780290643a80|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "config_get|f1ddfd06dae3dc171e677fcb02ff75eb7ceca0aaffbb868a91ee11a8c271fa9e|d7d40b7d47032b9ca22bfcdd95cf4a6b6ce70e92ead8ee20a5d22aae577214b8",
    "config_list|72678d9d9c372128e56333712c1fc402a70318f3ed63909ff91c629fdab0f683|87452c7ab16b470feb183acedadff7f2eab93fd98dbf1f710b49631254c32dcf",
    "config_reset|3a0f24b627f69af16eeefd214b2f0c92df61f4e60ac982704c58457f24708c54|d90d4323bbec97685cda52f36bc28c0a66d45f10ee58725b389c46d8dba06158",
    "config_set|d3c69dd3454c45c952ee10f97731f2dd9e5e419a3492a3aa52e8c703634ab479|4c7761d6b13cfd0bf93e31b449ed2b002a3ad598cc67f635d7ee492e89ddc4d4",
    "create_seed|ec16a67a00abe30d8410e1720468a039cd4b4c1a85820894e9b766a6ba868701|c79d95c1377cbed5559441027de27bbaace22d9ee8945fdd8c3d47c0333f0d7e",
    "crew_create|731ac03f7474a59d939a6acd2e6cb97344805db5dea7dc185c287468f39a6ce3|5fbc0737c82ee925cc6b708ce63fe4ea11d2238c905f9974587b1cf021ab9fb4",
    "crew_delete|6ac73732aec72b5596cf240decbc0da64ea177ae39be4e40f77629371bb81f48|21f065b99a46c1d795525e2f941ed4b7fe8f69e6593bc79ef81182897d4faa57",
    "crew_get|d9471965dee083abac1ffe1f0b61cff31f46f3c339f42cd1b7cd646ac62181d5|5dd7b6e4bb6740844ab949d730a74a3035678090f0f52e329fec09ff93cc3c7f",
    "crew_list|de5d37159a9000510178aa3147800fdb7166d2feab4fc55b7a66622d0a0c0cec|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "crew_update|ba98471566591039ce23de7339c7b0355f9715bf8ff743e7ede1715319479689|4cd37a027dac954b477faae56e13cc566ca5b620929feee69f100b6d842a0c93",
    "evolve_identity|cbae994f70163b304935cc28f006ffac2eec8e1bea905dea2c0fb59bd1c66cdc|5ec9c4410613adf3d140071d76d032723c8112c15228ccd582212ec76839a681",
    "get_identity|21d80d3a69d9719487bc498958b5600e86bf36519e229d270d2c2ae338383f72|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "get_tools|ea4c08804bf3a96c7e2e9f9be825feedb18ce038998f953065423576f6783beb|8d1085667f9f7b280bc979a52911ce87934bee573c8dd9a35175afad22365c8f",
    "graph|d430e133b8f3fb2f69fa901969a339a14191f3cd2da796bfccd5887a44709d82|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_complete_node|dd79817c0bd15c7fb324ee65bbf0b920c4d36f7958595932ab728a1d4418c76c|e4d69bbb70ef2131b2283c6695389cea76e8bd3a3af90ef7c139fb7772513bce",
    "graph_control|6ea48b9f5a7efdcb8091015b809f91686f69870f05e7f83b077744334a521603|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_crew|21b4da02c1240d4ca5cb9609c412b60f6b89d07a31ce2b4f89e39669fce69180|0034468e8fee6ccb105e52327ac46b4ea0842965c6da921f19977031e15ee86a",
    "graph_edge|dc837b1ca327d911a1f60b27d4546747dcb70411b35c7a51d4954256e73987de|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_edit|d5c0c9ad02ee3f28ccea179274e9a3410f8458a9ddb43f87123119ce9e92f71f|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_ensemble|bf20db1b2cf999ce57db1956975fe7399a5554d3b527a217512258684bb3e9ea|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_node|4abaead01a23923bfd19dac5d8630ca0c008b99a8b7a0d5f689f8cd0d649e235|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_report_blocker|e8b03ebc2341ed1dcec92b824650173db7d164d076ff501b3c460682c81f072c|8659fd458ed655692eb594522433fe42ab4a7c69f8164862571038fa7b254192",
    "intelligence_delete_node|b70ce24d59d2619e275cb40397037d7748f3ebca64ac5ffdc85eae6ee65bb1af|28ea20eaac05de60c91d7d9f3f7af2d48bab582aae5ee9dd04b5bc04ba88f1ca",
    "intelligence_delete_relation|14ac75ee8ed351ec58f6bec7bbb0a9ca8267f01931e936d287cc0aaf494612a8|811a9f24eda2e40c4d023042194be57946b8da97789fe52c5855eb4e6d258752",
    "intelligence_get_context|669fe1f581e27a2bdf49f09765a42bc8f1f0879bf49c0466ff072ed315e73e33|21815b7ab3af1404c86b99df1bd52c8d73a656d5ffa2229c6ba6e7b95402b58f",
    "intelligence_graph_walk|2f5efed1b23f5b4400575ed31c19a02c454bbeaa364f7036ae14049663c0e0cb|0c72c72222c0cda81d69c26df97f41217d836bc7360a555346e460653e25b4d9",
    "intelligence_link_projects|e0b46f79afb49768ec33f101e8210ea7818dd8418aad92df4a8be69eebadbe2e|13d886b67b4f5e77eb671cce41a6b7423cc24e6867c99ee1e51125e07844376a",
    "intelligence_list_projects|68da4fb4e810e0f41612e0f56052ba26bbe029553038b43b7f4aa9aa6259f3aa|893537157a2d2c8ab5753a889bc25ff91b60e2e95f23f1af34fc418e9ec34224",
    "intelligence_search|a91c69fe51924face0f4a2a6c4777b9dde708606f68cc44c7c46d16719d3f67c|fa7481fcecc2528cae994223e58076d4818ac3f55cb99577e71a45e5d85c395e",
    "intelligence_upsert|164de5427fff75a57508a5da581435374ebd7176ced490cf2e21577294d1aa01|7f55a980873729de4e6ae987d684496da34ac296de5d6d75603bf72dc039db1d",
    "list_seeds|94657a2e7927a4f05350461cdef7c05569145bc39c512444039f38077c689cde|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "loop_complete_node|a3a48975f0dafdc50569ac00a150cb091479281e7339f112708580a1764f95e8|e4d69bbb70ef2131b2283c6695389cea76e8bd3a3af90ef7c139fb7772513bce",
    "loop_report_blocker|9fb9038c5d224015aa25b157730b99339dd91372deb3c1460abb1e5e4fad2081|8659fd458ed655692eb594522433fe42ab4a7c69f8164862571038fa7b254192",
    "mutex|8451e497d2312801bd69e0c0d17a9977ef0a450c948592e7d749c1a52519faf4|05fa698c38dd86f99b62665294f300134ba08d32cac693e66826a47b8166a117",
    "node_install|38e9f70a418ce7bc81c03aa7d091e8509f5f1f15acb966eb91ff07e6a42f931c|f4c5a0612af1e6fd4735aead2dabac62829d5f54bcc64c1e70143782959b4d93",
    "node_list|49e8c0c65572041d3500b485f2e648777a0ae0be6706f54f9380801b1b169429|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "node_remove|d8b9b2d56ff664fa3560cf511b6e55a11dfe59037ad865029e3af721762a5f59|413fb6bc5f33f458ea1d94114797d81de2738d90e0185e3412682145d37f8e5d",
    "project_register|95d40d7773adb80b430d84f928d60d6a48d369505e8bf6c0d9f54aaa64b1d535|2fe9ec9ceb19c66e3a5f34a073529e6088506baa2e77dcca8d0d8f31081bcadd",
    "project_remap|ee326ec962be9943ccf911a190168d71163903ab4370775a0a433d32e7721f25|9fb44ff73d1e90118188516f4eb03a9741b9ad7cc2a0309a81a1919179158c35",
    "project_search|c0abdc269d287d6cf3465702f699e8464f18e0d6952f6870d2d1b4e37401b8da|4b6603ec8691bbbfa1813460965630dcdbc2613d655f1b3047e215a2a5693259",
    "project_update|33152dd73661dc1c449e9dc09e4b4bd72bdead18268244c1279c4e2dd3afb6b4|f82719dddba8f813c95d438e67ebb669b9414a60489aae2ac46de3adc377dde0",
    "queue_add_spec|71fed63c512e34c3bb79ae3438fd29bb76321b919c42fef162603a6d0f458e3d|7ebca2cc7b5207dc54b7089e3b2492f9812dbc1fb3752cce3978e9e80dbb4f7e",
    "queue_create|ad96a6520c976c9acdcf08610dc74a7d0728ae1cc81d47b678aed41ca63f1ac1|dda4afff54546d6abf4068d69851810d4e9eae9e40eebd0d11a3312500d0d538",
    "queue_list|1f3e846f9b91428f0a705e8e2232e40ed93bd57e45eeb2960fee25b912bd06dc|9896c1d2e6097e411c53d81f1b34541d5b7a0848b4f4cd439eb39deae4fcd655",
    "queue_remove_spec|d607661fcaed39c344ae545f5a1f2246ed530af3ce9cc13615e2a62b933aafa8|6399dab2f77e4bacd80b88aff1f4530000cff7157384c10e252844d5ce85c53e",
    "queue_reorder|c2553e02e72d85646d644b7aa94c6096ffcea3ffb819194875bc9e7d421ffb26|503be7944f0afe9758aa3d587bd223892125df857f671f6efbed7ffab5fc447c",
    "queue_set_group|3eb98961df3f175e2b84f14beecc1629cf21c635e2bef749fe79919470056082|65cad91c82f116e5b68100ab9268eded30c0612d2d0bbfacb27d1bd55776e302",
    "rag_compact|5d48c58fffa0c215e2b2fdcfafeb530f3fa62584e05f76bb5d6d99bd59897e84|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "rag_search|8a1f6386074319d1c99700410f884ba9ed3a3f0f91276a50766cf677bf1b26cd|cad73c0b5b678b2f49d622a364b008d729b24da064fffa4fa9bb0543c6bc2563",
    "registry_refresh|abfca8f63e35db90f993908eff7e4f13ba94d0b133ce72830fd5ec86df6c7908|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "remove_seed|b24cd25a52f13d1714cd1a02c3156317166b33f19df049dc99326685750fc374|b892848b7943f3e267ee87a61dc0600b279b87f2e291c69fa7878c9c71dc4678",
    "roster_check|80d72258ccfc8cbb6509168ff6197f0c073adc5600e7a50e9815172a65313387|289fb4a620ecfc6f1ffb6a4f702621f09fec4b818a97f94c2d86f075f7812abd",
    "roster_clear|31c4ac456165d42a5b18c6f8ca8f37e05aafeba98ee44488bf6cfa1fbecf1720|dbf92e8f2bf597121d5a9484dcd66ce0d0797733186803ac36aa963821094bbf",
    "roster_list|99f9cd3cdad9f30fe335f61cb51bdeb618ee0a743d3c0decdce04c8a9572af68|1fe958cb823e3ec1da7395e8033dccb055b2dcdc456019ecffc8da3c2a7e2b37",
    "roster_set|4a5445a5af19460e4fcc5a4bff5c9d32174fa64373b922d74392bfc7178a8e0c|8c757aa7220d7226152ef854113c682393ece93695cf62844c0c6dcb3a2bccf3",
    "scheduled_send_cancel|e97ad927d1c1d8778e0e7862e7ac4df954098bd847329a5cac398d19177d47e8|4fcc384db51c3909ff831c4c888499597bbcdd4e95af1a16c5a7b851fbbdc778",
    "scheduled_send_create|e6defc41d6d01fe133056562efc3aba3b50b6d1e2a06fe278894cbbbcb65a165|959aa8850eee97391ca1529377085f07115c926742f575ceac2d01e143b80880",
    "scheduled_send_list|946cdf95ebf54570fe9566aa69f8f5e7a18f08ea7b29a6be390bd9ce66a4df63|db1ee418b59faad9b3fae002b3f21eb04fa7f25097f3a595c26e7bc54f700d0f",
    "session_list|f983db33c96519c95584055f90c807dda350a17e9b12d402c0551590e772532b|adae59573f649012ccb3856be07ea7740581587cfeb6e9caba07ab66ca88ec84",
    "skill_get|11bb4551bb0cf314b47867061fdb6ecc35812780d98808cd2f1e134d098bc8b9|ffa1d0d446d446ba220972aac074b9d9d386caa3a92df3b710eadbc7cdfa0b59",
    "skill_list|970ee8acf69615b521eaca0b1b27b8555a7fdc96b5655ebf435f4667418d290b|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "spec_convert|dcabb9d52877782e47ac4fed3582e63e4c1cc6324c25b3079d6c25ef06552b16|efddc7bd8bbcef73a14eb1ace1ffdaec81e518ef1e13c1e9271d0b8acb694a49",
    "spec_create|7dd9a69253452c11b113743c3c5118f35cf4bf029c3380bcb9777254e41fd2eb|9a7716e491160f3935cb57a19fffa132eaa261ef7ea9455c208811fe4244cfab",
    "spec_delete|cd48e914d536c1e203436b9694cc93ba0b107b488b0383537716107c02537df8|5b662f4985d9d6880743467a057fa666350855e935d09b710e04fe7c958a701f",
    "spec_list|0675cb8e001153bcf3691cf3d6e276c91f3e1f3be36beff51938b4b01afdd22c|adb821d596c9cb76c6b557a59ab6afb4fa0dba9eea1d4f3c009fc9881835312f",
    "spec_note_write|5864f496a2db355ac435a64b0fb172d0162b75eb50b84aba7913f43d89a0cecc|d48e9814cc0265adbd0d55121b2537649d71afdae08221b567bde38aff7b2c33",
    "spec_notes_get|c0a4e526752166f5544538fcbb039c03b03cfbaa2f57aa3aa1fbb8abf0c7bccb|4a09a00d0f82bc519a07b0f23d1bcc28ddcbc9f7cdb830a54ba30fbd6e6ab494",
    "spec_section_get|f2e7ae92e46d096fb4c5f1a3ce635e171dcd884e3f746ae936dfcb649e0aca22|8ba9af8b72d50548d76ed5795411302e403b639c6fb0c1f2ad085fcc8a1c2a69",
    "spec_set_status|1a8612df16827c55d054df32ed3d585f33a14068c8c6737db584b918727ff5ab|432f735505cfff0df84802fa35c02800341b240c276037bfb6772e2a0fb589cd",
    "spec_update|4b63bd53115202d8c35c0e347ecc43491b5080564306c1fa643657d40790ba14|2fb17064e36c1ef3e25b4a7acba170162686e939bc4ddf22a7c730e7340767ed",
    "subagent_collect|c950e7193036445e86b6ffd6587304e0e71cf00302734d0ed6e83c549a2e95dd|5c379cd2fe57942f2460934525f4f8645ea490dcdd7130a4d49035cd8354fa21",
    "subagent_spawn|062b2ac16b1f04f1ada1607d2cb04fe4840b9a09809382eed9c09878ab038fd1|8ea96b320e0b3bf02c54fa0bad48a60e941089abeb0af2b7ff2e592103abaeff",
    "sync_broadcast|c5904706ddfd0877686466deeff33f21d70075bbed6c8ae6eba5794fe3ea1043|f5584c95194a960c79e39b84a115e439c1b0e5f7b91d39385e5788cc344e83af",
    "sync_declare_intent|621ecbc55446f78d4050026a7554487c33116b0ca218f359fc4355207d92abfe|ec40954a22bc0559f68b428ff8071f9ffa02c9e5702620ad4432f33b5e384eba",
    "sync_get_context|5bf5707dc316b32fa427141fab68b709dc07af8a52676da646958774a432a1ff|4bd69d426fd995f95a4bc3d546c79b60f85a0a282552cd0d9523052e2467216a",
    "sync_report_status|ee93a882c476cc2792fa68bce24c1f187de5947c08b72bb55400dd114bdc07a4|ca10e912be130d9d76d33ccb1ced7bfce5f119e46d3d222fda29af25b4fd95a1",
            ];

fn spec_with_status(graph_id: &str, id: &str, position: i64, status: GraphSpecStatus) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: Some(graph_id.to_string()),
        name: id.to_string(),
        description: None,
        position,
        parallelizable: false,
        status,
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    }
}

fn graph_reset_fixture(status: GraphStatus) -> (tempfile::TempDir, Database, String) {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let graph_id = "graph-reset-test".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();
    (dir, db, graph_id)
}

fn router_config(routes: &serde_json::Value, fallback: &str) -> serde_json::Value {
    serde_json::json!({ "routes": routes, "fallback": fallback })
}

fn two_routes_json() -> serde_json::Value {
    serde_json::json!([
        { "label": "retry", "description": "Retry the current step." },
        { "label": "escalate", "description": "Hand off to a human." },
    ])
}

fn standalone_spec(id: &str) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    }
}

fn queue_test_db() -> (tempfile::TempDir, Database) {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    (dir, db)
}

fn insert_queue(db: &Database, id: &str) {
    db.insert_queue(&Queue {
        id: id.to_string(),
        name: format!("{id}-name"),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

/// Build a fully-wired `TaskTriggerHandler` over an in-memory-ish temp DB
/// so the queue `#[tool]` methods (and their shared `do_queue_*` helpers)
/// can be exercised end-to-end.
fn queue_test_handler() -> (
    tempfile::TempDir,
    std::sync::Arc<Database>,
    TaskTriggerHandler,
) {
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::sync_manager::SyncManager;
    use crate::watchers::WatcherEngine;
    use std::sync::Arc;
    use tokio::sync::Notify;

    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
    let sync_manager = Arc::new(SyncManager::new(Arc::clone(&db)));
    let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
    let watcher_engine = Arc::new(WatcherEngine::new(
        Arc::clone(&db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));
    let ingestion = Arc::new(IngestionManager::new(
        Arc::clone(&db),
        dir.path().to_path_buf(),
    ));
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
        dir.path().join("skills"),
        Vec::new(),
        15,
    ));
    let handler = TaskTriggerHandler::with_deps(HandlerDeps {
        db: Arc::clone(&db),
        executor,
        watcher_engine,
        scheduler_notify: Arc::new(Notify::new()),
        graph_engine,
        notification_service: notif,
        sync_manager,
        ingestion,
        dynamic_skills,
        start_time: std::time::Instant::now(),
        port: 0,
    });
    (dir, db, handler)
}

fn result_text(result: &rmcp::model::CallToolResult) -> String {
    format!("{:?}", result.content)
}

fn queue_raw_text(result: &rmcp::model::CallToolResult) -> String {
    result.content[0].as_text().unwrap().text.clone()
}

fn queue_is_err(result: &rmcp::model::CallToolResult) -> bool {
    result.is_error == Some(true)
}

fn running_spec(id: &str) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.status = GraphSpecStatus::Running;
    spec
}

fn interrupted_spec(id: &str) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.status = GraphSpecStatus::Interrupted;
    spec
}

/// A minimal graph row, needed only to satisfy `graph_runs.graph_id`'s FK.
fn insert_test_graph(db: &Database, id: &str) {
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();
}

/// A minimal node row, needed only to satisfy `graph_runs.node_id`'s FK.
fn insert_test_node(db: &Database, id: &str, spec_id: &str) {
    db.insert_graph_node(&GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

fn graph_run_row(id: &str, graph_id: &str, spec_id: &str, status: GraphRunStatus) -> GraphNodeRun {
    GraphNodeRun {
        id: id.to_string(),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: "node-1".to_string(),
        status,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: (status != GraphRunStatus::Running).then(chrono::Utc::now),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    }
}

// ── B12: stale graph_complete_node/graph_report_blocker reports ─────

fn reported_run_fixture() -> (tempfile::TempDir, Database) {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
    insert_test_node(&db, "node-1", "spec-a");
    (dir, db)
}

/// CB30 test helpers: a blank-name placeholder bound to a graph (engine
/// bookkeeping for spec-less dispatches) and a named spec bound to a
/// graph or standing alone as a queue member.
fn cb30_test_graph(id: &str, status: GraphStatus) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    }
}

fn placeholder_spec(id: &str, graph_id: &str, status: GraphSpecStatus) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.graph_id = Some(graph_id.to_string());
    spec.name = String::new();
    spec.status = status;
    spec
}

fn named_spec(
    id: &str,
    graph_id: Option<&str>,
    name: &str,
    position: i64,
    status: GraphSpecStatus,
) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.graph_id = graph_id.map(str::to_string);
    spec.name = name.to_string();
    spec.position = position;
    spec.status = status;
    spec
}

fn queue_driven_test_db() -> (tempfile::TempDir, Database) {
    // A running graph drawing from a queue with two named members, plus
    // the blank-name placeholder the engine binds to the graph itself.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let mut lp = cb30_test_graph("graph-q", GraphStatus::Running);
    lp.active_run_queue_id = Some("q1".to_string());
    db.insert_graph(&lp).unwrap();
    insert_queue(&db, "q1");
    db.insert_graph_spec(&placeholder_spec("ph", "graph-q", GraphSpecStatus::Running))
        .unwrap();
    db.insert_graph_spec(&named_spec(
        "spec-a",
        None,
        "CM4 — queue work",
        0,
        GraphSpecStatus::Running,
    ))
    .unwrap();
    db.append_queue_member("q1", "spec-a", None).unwrap();
    db.insert_graph_spec(&named_spec(
        "spec-b",
        None,
        "CM6 — next work",
        0,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    db.append_queue_member("q1", "spec-b", None).unwrap();
    (dir, db)
}

// ── B41: cancel a scheduled autorun via graph_control(at=None) ──

fn autorun_test_graph(graph_id: &str, workdir: &str, status: GraphStatus) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: graph_id.to_string(),
        name: graph_id.to_string(),
        description: None,
        workdir: workdir.to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    }
}

// ── U10: copy nodes and ensembles ────────────────────────────────

fn u10_check(id: &str, spec_id: Option<&str>, graph_id: Option<&str>, position: i64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: spec_id.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position,
        created_at: chrono::Utc::now(),
    }
}

fn u10_agent(
    id: &str,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    position: i64,
    config: serde_json::Value,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: spec_id.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        name: id.to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position,
        created_at: chrono::Utc::now(),
    }
}

/// A persisted source ensemble ("proposers"): two members (claude, codex/o1),
/// wired `from` → members → join → `to`.
fn u10_source_ensemble(
    db: &Database,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    from: &str,
    to: &str,
) -> BuiltEnsembleUnit {
    let members = [
        ("claude".to_string(), None, None, None, None, None, None),
        (
            "codex".to_string(),
            Some("o1".to_string()),
            None,
            None,
            None,
            None,
            None,
        ),
    ];
    let built = build_ensemble_unit(&EnsembleUnitSpec {
        mutex: Vec::new(),
        spec_id: spec_id.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        name: "proposers",
        prompt_template: "propose a solution",
        members: &members,
        entry_from_node: from,
        entry_condition: GraphEdgeCondition::Always,
        on_pass_to: to,
        on_pass_fan_out: None,
        on_fail_to: None,
        on_fail_fan_out: None,
        min_pass: 2,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        start_position: 10,
        kind: EnsembleKind::Parallel,
        commit_rights: false,
    });
    db.insert_ensemble_unit(
        &built.ensemble,
        &built.members,
        &built.member_nodes,
        &built.join_node,
        &built.edges,
    )
    .unwrap();
    built
}

/// Insert a standalone spec (no owning graph) so a test can hang nodes
/// off it without needing a full `Graph` row too.
fn insert_standalone_spec(db: &Database, id: &str) {
    db.insert_graph_spec(&GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    })
    .unwrap();
}

// ── graph_trigger_json ───────────────────────────────────────────

fn make_graph_with_trigger(trigger: Option<Trigger>) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: "graph-1".to_string(),
        name: "Test Graph".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Draft,
        trigger,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    }
}

mod part01_tests;
mod part02_tests;
mod part03_tests;
mod part04_tests;
