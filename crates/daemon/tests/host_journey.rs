//! The host, proven through the two synthetic adapters.
//!
//! Each journey runs the conformance suite's host scenario against this
//! crate's `companion-lobby` binary, which the test build compiles with the
//! test kit's passbook and board adapters; a release build has neither.

use std::path::Path;

use lobby_conformance::host;

const EXE: &str = env!("CARGO_BIN_EXE_companion-lobby");

#[test]
fn a_companion_with_two_boards_sees_reach_and_board_tools_whose_at_names_both() {
    host::two_boards_are_reached_through_one_set_of_tools(Path::new(EXE));
}

#[test]
fn a_second_companion_sees_none_of_the_first_ones_tools() {
    host::another_companion_sees_none_of_the_first_ones_tools(Path::new(EXE));
}

#[test]
fn a_call_is_journaled_pending_then_settled_with_its_connection() {
    host::a_call_is_journaled_pending_then_settled_through_its_connection(Path::new(EXE));
}

#[test]
fn a_connection_revoked_mid_call_never_delivers_a_success() {
    host::a_late_result_after_a_revocation_is_never_a_success(Path::new(EXE));
}

#[test]
fn revoking_a_credential_cascades_and_the_host_hears_the_catalog_shrink() {
    host::revoking_the_credential_cascades_to_its_connections(Path::new(EXE));
}

#[test]
fn an_ask_the_credential_cannot_answer_is_refused_before_the_provider_is_reached() {
    host::an_unsupported_ask_is_refused_before_the_provider_is_reached(Path::new(EXE));
}
