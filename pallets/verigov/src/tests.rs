use crate::{mock::*, *};
use frame_support::{
	assert_err, assert_noop, assert_ok,
	traits::fungible::{Inspect, InspectHold},
};
use sp_runtime::Perbill;

const STAKE: Balance = 1_000;
const BUILD_STAKE: Balance = 5_000;
const BOND: Balance = 1_000;
const VOTING_PERIOD: u64 = 100;
const ENACTMENT_DELAY: u64 = 10;

fn h(byte: u8) -> Hash32 {
	[byte; 32]
}

fn bv<S: frame_support::traits::Get<u32>>(s: &str) -> BoundedVec<u8, S> {
	s.as_bytes().to_vec().try_into().unwrap()
}

fn acme_policy() -> ReleasePolicy<u64> {
	ReleasePolicy {
		required_roles: vec![
			RoleQuorum { role: Role::CoreDeveloper, quorum: Perbill::from_rational(3u32, 5u32) },
			RoleQuorum { role: Role::SecurityAuditor, quorum: Perbill::from_rational(1u32, 2u32) },
		]
		.try_into()
		.unwrap(),
		voting_period: VOTING_PERIOD,
		enactment_delay: ENACTMENT_DELAY,
	}
}

fn acme_slashing() -> SlashingParams<u64> {
	SlashingParams {
		sigma_neg: Perbill::from_percent(50),
		sigma_mal: Perbill::from_percent(100),
		delta_max: 1_000,
	}
}

fn acme_stakeholders() -> Vec<StakeholderInit<AccountId, Balance>> {
	let si = |account, role, stake| StakeholderInit { account, role, stake };
	vec![
		si(ALICE, Role::CoreDeveloper, STAKE),
		si(BOB, Role::CoreDeveloper, STAKE),
		si(CAROL, Role::CoreDeveloper, STAKE),
		si(DAN, Role::CoreDeveloper, STAKE),
		si(EVE, Role::CoreDeveloper, STAKE),
		si(FOO_AUDIT, Role::SecurityAuditor, STAKE),
		si(FUZZ_IO, Role::SecurityAuditor, STAKE),
		si(BUILD_SERVICE, Role::BuildService, BUILD_STAKE),
	]
}

/// Register `acme-utils` exactly as in the paper's worked example. Returns the project id.
fn setup_acme() -> ProjectId {
	assert_ok!(Verigov::project_genesis(
		RuntimeOrigin::root(),
		bv("acme-utils"),
		acme_stakeholders().try_into().unwrap(),
		acme_policy(),
		acme_slashing(),
		BOND,
	));
	0
}

fn attestation(output_binary_hash: Hash32) -> BuildAttestation<AccountId> {
	BuildAttestation {
		source_commit_hash: h(0xc0),
		source_tree_hash: h(0x51),
		build_environment_hash: h(0xde),
		output_binary_hash,
		builder: BUILD_SERVICE,
	}
}

fn sign(att: &BuildAttestation<AccountId>) -> MockSignature {
	MockSignature(BUILD_SERVICE, att.signing_payload().to_vec())
}

fn propose(project: ProjectId, output_binary_hash: Hash32) -> ProposalId {
	let att = attestation(output_binary_hash);
	let sig = sign(&att);
	let id = NextProposalId::<Test>::get();
	assert_ok!(Verigov::new_release(
		RuntimeOrigin::signed(ALICE),
		project,
		bv("1.2.3"),
		att,
		h(0x42),
		sig,
	));
	id
}

fn status(id: ProposalId) -> ProposalStatus<u64> {
	Proposals::<Test>::get(id).unwrap().status
}

fn held(reason: HoldReason, who: AccountId) -> Balance {
	Balances::balance_on_hold(&reason.into(), &who)
}

// ---------------------------------------------------------------------------------------------
// Genesis
// ---------------------------------------------------------------------------------------------

#[test]
fn genesis_registers_stakeholders_and_escrows_stake() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();

		let project = Projects::<Test>::get(p).unwrap();
		assert_eq!(project.name.to_vec(), b"acme-utils".to_vec());
		assert_eq!(project.proposal_bond, BOND);
		assert_eq!(project.slashing, acme_slashing());
		assert_eq!(NextProjectId::<Test>::get(), 1);

		assert_eq!(
			Stakeholders::<Test>::get(p, ALICE),
			Some(Stakeholder { role: Role::CoreDeveloper, stake: STAKE })
		);
		assert_eq!(
			Stakeholders::<Test>::get(p, BUILD_SERVICE),
			Some(Stakeholder { role: Role::BuildService, stake: BUILD_STAKE })
		);
		assert_eq!(RoleStake::<Test>::get(p, Role::CoreDeveloper), 5 * STAKE);
		assert_eq!(RoleStake::<Test>::get(p, Role::SecurityAuditor), 2 * STAKE);
		assert_eq!(RoleStake::<Test>::get(p, Role::BuildService), BUILD_STAKE);
		assert_eq!(RoleStake::<Test>::get(p, Role::CommunityTrustee), 0);

		// Stake is escrowed as a hold on VGOV (pallet-balances).
		assert_eq!(held(HoldReason::Stake, ALICE), STAKE);
		assert_eq!(held(HoldReason::Stake, BUILD_SERVICE), BUILD_STAKE);
		assert_eq!(Balances::balance(&ALICE), INITIAL_BALANCE - STAKE);

		System::assert_has_event(
			Event::ProjectCreated { project_id: p, name: bv("acme-utils") }.into(),
		);
		System::assert_has_event(
			Event::StakeholderRegistered {
				project_id: p,
				account: BUILD_SERVICE,
				role: Role::BuildService,
				stake: BUILD_STAKE,
			}
			.into(),
		);
	});
}

#[test]
fn genesis_requires_root() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			Verigov::project_genesis(
				RuntimeOrigin::signed(ALICE),
				bv("acme-utils"),
				acme_stakeholders().try_into().unwrap(),
				acme_policy(),
				acme_slashing(),
				BOND,
			),
			sp_runtime::DispatchError::BadOrigin
		);
	});
}

#[test]
fn genesis_rejects_unreachable_quorum_and_duplicates() {
	new_test_ext().execute_with(|| {
		// A required role with no staked members could never meet its quorum.
		let mut policy = acme_policy();
		policy
			.required_roles
			.try_push(RoleQuorum { role: Role::CommunityTrustee, quorum: Perbill::from_percent(50) })
			.unwrap();
		assert_err!(
			Verigov::project_genesis(
				RuntimeOrigin::root(),
				bv("acme-utils"),
				acme_stakeholders().try_into().unwrap(),
				policy,
				acme_slashing(),
				BOND,
			),
			Error::<Test>::RequiredRoleHasNoStake
		);

		let mut policy = acme_policy();
		policy.required_roles = Default::default();
		assert_noop!(
			Verigov::project_genesis(
				RuntimeOrigin::root(),
				bv("acme-utils"),
				acme_stakeholders().try_into().unwrap(),
				policy,
				acme_slashing(),
				BOND,
			),
			Error::<Test>::NoRequiredRoles
		);

		let mut dup = acme_stakeholders();
		dup.push(StakeholderInit { account: ALICE, role: Role::SecurityAuditor, stake: STAKE });
		assert_err!(
			Verigov::project_genesis(
				RuntimeOrigin::root(),
				bv("acme-utils"),
				dup.try_into().unwrap(),
				acme_policy(),
				acme_slashing(),
				BOND,
			),
			Error::<Test>::DuplicateStakeholder
		);
	});
}

#[test]
fn genesis_fails_when_a_stakeholder_cannot_cover_its_stake() {
	new_test_ext().execute_with(|| {
		let mut s = acme_stakeholders();
		s[0].stake = INITIAL_BALANCE + 1;
		assert!(Verigov::project_genesis(
			RuntimeOrigin::root(),
			bv("acme-utils"),
			s.try_into().unwrap(),
			acme_policy(),
			acme_slashing(),
			BOND,
		)
		.is_err());
	});
}

// ---------------------------------------------------------------------------------------------
// Proposal submission
// ---------------------------------------------------------------------------------------------

#[test]
fn new_release_holds_bond_and_records_proposer_aye() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));

		let prop = Proposals::<Test>::get(id).unwrap();
		assert_eq!(prop.proposer, ALICE);
		assert_eq!(prop.version.to_vec(), b"1.2.3".to_vec());
		assert_eq!(prop.attestation.output_binary_hash, h(0xa1));
		assert_eq!(prop.attestation_cid, h(0x42));
		assert_eq!(prop.bond, BOND);
		assert_eq!(prop.voting_ends, 1 + VOTING_PERIOD);
		assert_eq!(prop.status, ProposalStatus::Voting);

		assert_eq!(held(HoldReason::ProposalBond, ALICE), BOND);
		assert_eq!(Balances::balance(&ALICE), INITIAL_BALANCE - STAKE - BOND);

		assert_eq!(Votes::<Test>::get(id, ALICE), Some(true));
		assert_eq!(Tallies::<Test>::get(id, Role::CoreDeveloper).ayes, STAKE);
		assert_eq!(NextProposalId::<Test>::get(), 1);
	});
}

#[test]
fn only_core_developers_of_the_project_can_propose() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let att = attestation(h(0xa1));
		let sig = sign(&att);

		assert_noop!(
			Verigov::new_release(
				RuntimeOrigin::signed(FOO_AUDIT),
				p,
				bv("1.2.3"),
				att.clone(),
				h(0x42),
				sig.clone()
			),
			Error::<Test>::NotCoreDeveloper
		);
		assert_noop!(
			Verigov::new_release(
				RuntimeOrigin::signed(OUTSIDER),
				p,
				bv("1.2.3"),
				att.clone(),
				h(0x42),
				sig.clone()
			),
			Error::<Test>::NotStakeholder
		);
		assert_noop!(
			Verigov::new_release(RuntimeOrigin::signed(ALICE), 7, bv("1.2.3"), att, h(0x42), sig),
			Error::<Test>::ProjectNotFound
		);
	});
}

#[test]
fn attestation_must_be_signed_by_a_registered_build_service() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let att = attestation(h(0xa1));

		// Signed by the wrong key.
		let forged = MockSignature(BOB, att.signing_payload().to_vec());
		assert_noop!(
			Verigov::new_release(RuntimeOrigin::signed(ALICE), p, bv("1.2.3"), att.clone(), h(0x42), forged),
			Error::<Test>::InvalidBuilderSignature
		);

		// Signed over a different attestation (e.g. the hash was swapped after signing).
		let other = attestation(h(0xbb));
		let stale = sign(&other);
		assert_noop!(
			Verigov::new_release(RuntimeOrigin::signed(ALICE), p, bv("1.2.3"), att.clone(), h(0x42), stale),
			Error::<Test>::InvalidBuilderSignature
		);

		// `builder` names a stakeholder that is not a Build Service.
		let mut bad_builder = att.clone();
		bad_builder.builder = BOB;
		let sig = MockSignature(BOB, bad_builder.signing_payload().to_vec());
		assert_noop!(
			Verigov::new_release(RuntimeOrigin::signed(ALICE), p, bv("1.2.3"), bad_builder, h(0x42), sig),
			Error::<Test>::NotBuildService
		);
	});
}

// ---------------------------------------------------------------------------------------------
// Voting and enactment (paper, Section 5.6, Steps 4-6)
// ---------------------------------------------------------------------------------------------

#[test]
fn worked_example_release_is_accepted_and_enacted() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));

		// Bob, Carol, Dan vote yes; Eve abstains. Core Dev approval = 4/5 >= 3/5.
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(BOB), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(CAROL), id, true));
		// Core Dev quorum met (3/5 incl. Alice) but no auditor has voted yet.
		assert_eq!(status(id), ProposalStatus::Voting);
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(DAN), id, true));
		assert_eq!(status(id), ProposalStatus::Voting);

		// Foo Audit votes yes: Sec Auditor approval = 1/2 >= 1/2. Accepted.
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FOO_AUDIT), id, true));
		let enact_at = 1 + ENACTMENT_DELAY;
		assert_eq!(status(id), ProposalStatus::Approved { enact_at });
		assert_eq!(PendingEnactments::<Test>::get(enact_at).to_vec(), vec![id]);
		System::assert_has_event(Event::ProposalApproved { proposal_id: id, enact_at }.into());

		// fuzz.io may still vote during the enactment delay: 2/2.
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FUZZ_IO), id, true));
		assert_eq!(Tallies::<Test>::get(id, Role::CoreDeveloper).ayes, 4 * STAKE);
		assert_eq!(Tallies::<Test>::get(id, Role::SecurityAuditor).ayes, 2 * STAKE);
		assert_eq!(status(id), ProposalStatus::Approved { enact_at });

		// Nothing is in the log before the enactment block.
		run_to_block(enact_at - 1);
		assert_eq!(ReleaseCount::<Test>::get(p), 0);
		assert_eq!(held(HoldReason::ProposalBond, ALICE), BOND);

		run_to_block(enact_at);
		assert_eq!(status(id), ProposalStatus::Enacted);
		assert_eq!(ReleaseCount::<Test>::get(p), 1);
		let entry = ReleaseLog::<Test>::get(p, 0).unwrap();
		assert_eq!(entry.proposal_id, id);
		assert_eq!(entry.version.to_vec(), b"1.2.3".to_vec());
		assert_eq!(entry.output_binary_hash, h(0xa1));
		assert_eq!(entry.attestation_cid, h(0x42));
		assert_eq!(entry.source_commit_hash, h(0xc0));
		assert_eq!(entry.enacted_at, enact_at);
		assert!(PendingEnactments::<Test>::get(enact_at).is_empty());

		// Bond returned; stake still escrowed.
		assert_eq!(held(HoldReason::ProposalBond, ALICE), 0);
		assert_eq!(held(HoldReason::Stake, ALICE), STAKE);
		assert_eq!(Balances::balance(&ALICE), INITIAL_BALANCE - STAKE);

		System::assert_has_event(
			Event::ReleaseEnacted {
				project_id: p,
				proposal_id: id,
				index: 0,
				version: bv("1.2.3"),
				output_binary_hash: h(0xa1),
			}
			.into(),
		);
	});
}

#[test]
fn every_required_role_must_meet_its_quorum() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));

		// Both auditors approve (2/2) but only Alice+Bob among core devs (2/5 < 3/5).
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FOO_AUDIT), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FUZZ_IO), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(BOB), id, true));
		assert_eq!(status(id), ProposalStatus::Voting);

		// Nays never contribute to approval.
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(EVE), id, false));
		assert_eq!(Tallies::<Test>::get(id, Role::CoreDeveloper).nays, STAKE);
		assert_eq!(status(id), ProposalStatus::Voting);

		// Third core dev tips it to exactly 3/5.
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(CAROL), id, true));
		assert_eq!(status(id), ProposalStatus::Approved { enact_at: 1 + ENACTMENT_DELAY });
	});
}

#[test]
fn revoting_can_break_quorum_before_enactment() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(BOB), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(CAROL), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FOO_AUDIT), id, true));
		let enact_at = 1 + ENACTMENT_DELAY;
		assert_eq!(status(id), ProposalStatus::Approved { enact_at });

		// Carol flips to nay: 2/5 core devs -> back to voting, enactment cancelled.
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(CAROL), id, false));
		assert_eq!(Tallies::<Test>::get(id, Role::CoreDeveloper).ayes, 2 * STAKE);
		assert_eq!(Tallies::<Test>::get(id, Role::CoreDeveloper).nays, STAKE);
		assert_eq!(status(id), ProposalStatus::Voting);
		assert!(PendingEnactments::<Test>::get(enact_at).is_empty());
		System::assert_has_event(Event::ProposalApprovalRevoked { proposal_id: id }.into());

		run_to_block(enact_at + 1);
		assert_eq!(ReleaseCount::<Test>::get(p), 0);
		assert_eq!(status(id), ProposalStatus::Voting);
	});
}

#[test]
fn stake_weighting_within_a_role() {
	new_test_ext().execute_with(|| {
		// One whale core dev holds 6/10 of the role's stake: her aye alone meets 3/5.
		let si = |account, role, stake| StakeholderInit { account, role, stake };
		assert_ok!(Verigov::project_genesis(
			RuntimeOrigin::root(),
			bv("whale-utils"),
			vec![
				si(ALICE, Role::CoreDeveloper, 6_000),
				si(BOB, Role::CoreDeveloper, 2_000),
				si(CAROL, Role::CoreDeveloper, 2_000),
				si(FOO_AUDIT, Role::SecurityAuditor, 1_000),
				si(BUILD_SERVICE, Role::BuildService, BUILD_STAKE),
			]
			.try_into()
			.unwrap(),
			acme_policy(),
			acme_slashing(),
			BOND,
		));
		let id = propose(0, h(0xa1));
		assert_eq!(Tallies::<Test>::get(id, Role::CoreDeveloper).ayes, 6_000);
		assert_eq!(status(id), ProposalStatus::Voting);
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FOO_AUDIT), id, true));
		assert_eq!(status(id), ProposalStatus::Approved { enact_at: 1 + ENACTMENT_DELAY });
	});
}

#[test]
fn only_project_stakeholders_can_vote() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));
		assert_noop!(
			Verigov::vote(RuntimeOrigin::signed(OUTSIDER), id, true),
			Error::<Test>::NotStakeholder
		);
		assert_noop!(
			Verigov::vote(RuntimeOrigin::signed(BOB), 99, true),
			Error::<Test>::ProposalNotFound
		);
	});
}

#[test]
fn voting_closes_after_period_and_expired_proposal_returns_bond() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));
		let ends = 1 + VOTING_PERIOD;

		assert_noop!(
			Verigov::close_expired(RuntimeOrigin::signed(BOB), id),
			Error::<Test>::VotingPeriodNotOver
		);

		run_to_block(ends);
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(BOB), id, true));
		run_to_block(ends + 1);
		assert_noop!(
			Verigov::vote(RuntimeOrigin::signed(CAROL), id, true),
			Error::<Test>::VotingPeriodOver
		);

		assert_ok!(Verigov::close_expired(RuntimeOrigin::signed(BOB), id));
		assert_eq!(status(id), ProposalStatus::Expired);
		assert_eq!(held(HoldReason::ProposalBond, ALICE), 0);
		assert_eq!(ReleaseCount::<Test>::get(p), 0);
		assert_noop!(
			Verigov::close_expired(RuntimeOrigin::signed(BOB), id),
			Error::<Test>::ProposalNotVoting
		);
	});
}

// ---------------------------------------------------------------------------------------------
// Failure case: counter-attestation and negligence slashing (paper, Section 5.6)
// ---------------------------------------------------------------------------------------------

#[test]
fn counter_attestation_slashes_build_service_and_rejects_proposal() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		// Compromised Build Service attests to a tampered binary.
		let id = propose(p, h(0xbd));
		let issuance_before = Balances::total_issuance();

		// Bob's independent rebuild yields the honest hash.
		assert_ok!(Verigov::submit_counter_attestation(RuntimeOrigin::signed(BOB), id, h(0xa1)));

		// sigma_neg = 50% of the Build Service's 5,000 stake is burned.
		let slashed = BUILD_STAKE / 2;
		assert_eq!(
			Stakeholders::<Test>::get(p, BUILD_SERVICE).unwrap().stake,
			BUILD_STAKE - slashed
		);
		assert_eq!(RoleStake::<Test>::get(p, Role::BuildService), BUILD_STAKE - slashed);
		assert_eq!(held(HoldReason::Stake, BUILD_SERVICE), BUILD_STAKE - slashed);
		assert_eq!(Balances::total_balance(&BUILD_SERVICE), INITIAL_BALANCE - slashed);
		assert_eq!(Balances::total_issuance(), issuance_before - slashed);

		// Proposal rejected; the good-faith proposer's bond is returned.
		assert_eq!(status(id), ProposalStatus::Rejected);
		assert_eq!(held(HoldReason::ProposalBond, ALICE), 0);
		assert_eq!(
			CounterAttestations::<Test>::get(id),
			Some(CounterAttestation {
				challenger: BOB,
				attested_binary_hash: h(0xbd),
				rebuilt_binary_hash: h(0xa1),
			})
		);

		System::assert_has_event(
			Event::CounterAttestationAccepted {
				proposal_id: id,
				challenger: BOB,
				attested_binary_hash: h(0xbd),
				rebuilt_binary_hash: h(0xa1),
			}
			.into(),
		);
		System::assert_has_event(
			Event::BuildServiceSlashed {
				project_id: p,
				builder: BUILD_SERVICE,
				amount: slashed,
				remaining_stake: BUILD_STAKE - slashed,
			}
			.into(),
		);
		System::assert_has_event(Event::ProposalRejected { proposal_id: id }.into());

		// No further votes, no enactment, nothing in the log.
		assert_noop!(
			Verigov::vote(RuntimeOrigin::signed(CAROL), id, true),
			Error::<Test>::ProposalNotVoting
		);
		run_to_block(50);
		assert_eq!(ReleaseCount::<Test>::get(p), 0);
	});
}

#[test]
fn counter_attestation_cancels_a_scheduled_enactment() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xbd));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(CAROL), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(DAN), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FUZZ_IO), id, true));
		let enact_at = 1 + ENACTMENT_DELAY;
		assert_eq!(status(id), ProposalStatus::Approved { enact_at });

		// Foo Audit's rebuild disagrees during the enactment delay.
		assert_ok!(Verigov::submit_counter_attestation(RuntimeOrigin::signed(FOO_AUDIT), id, h(0xa1)));
		assert_eq!(status(id), ProposalStatus::Rejected);
		assert!(PendingEnactments::<Test>::get(enact_at).is_empty());

		run_to_block(enact_at + 1);
		assert_eq!(ReleaseCount::<Test>::get(p), 0);
		assert_eq!(status(id), ProposalStatus::Rejected);
	});
}

#[test]
fn counter_attestation_requires_a_real_discrepancy_from_another_stakeholder() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));

		assert_noop!(
			Verigov::submit_counter_attestation(RuntimeOrigin::signed(BOB), id, h(0xa1)),
			Error::<Test>::NoDiscrepancy
		);
		assert_noop!(
			Verigov::submit_counter_attestation(RuntimeOrigin::signed(BUILD_SERVICE), id, h(0xbb)),
			Error::<Test>::BuilderCannotChallenge
		);
		assert_noop!(
			Verigov::submit_counter_attestation(RuntimeOrigin::signed(OUTSIDER), id, h(0xbb)),
			Error::<Test>::NotStakeholder
		);
		assert_noop!(
			Verigov::submit_counter_attestation(RuntimeOrigin::signed(BOB), 99, h(0xbb)),
			Error::<Test>::ProposalNotFound
		);
		// Build Service untouched.
		assert_eq!(held(HoldReason::Stake, BUILD_SERVICE), BUILD_STAKE);
	});
}

#[test]
fn enacted_releases_are_outside_the_negligence_track() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id = propose(p, h(0xa1));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(BOB), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(CAROL), id, true));
		assert_ok!(Verigov::vote(RuntimeOrigin::signed(FOO_AUDIT), id, true));
		run_to_block(1 + ENACTMENT_DELAY);
		assert_eq!(status(id), ProposalStatus::Enacted);

		// Post-release malice is the dispute track (not implemented here).
		assert_noop!(
			Verigov::submit_counter_attestation(RuntimeOrigin::signed(BOB), id, h(0xbb)),
			Error::<Test>::ProposalNotChallengeable
		);
		assert_eq!(ReleaseCount::<Test>::get(p), 1);
	});
}

#[test]
fn repeated_negligence_keeps_halving_the_stake() {
	new_test_ext().execute_with(|| {
		let p = setup_acme();
		let id1 = propose(p, h(0xb1));
		assert_ok!(Verigov::submit_counter_attestation(RuntimeOrigin::signed(BOB), id1, h(0xa1)));
		let id2 = propose(p, h(0xb2));
		assert_ok!(Verigov::submit_counter_attestation(RuntimeOrigin::signed(CAROL), id2, h(0xa1)));
		assert_eq!(Stakeholders::<Test>::get(p, BUILD_SERVICE).unwrap().stake, BUILD_STAKE / 4);
		assert_eq!(held(HoldReason::Stake, BUILD_SERVICE), BUILD_STAKE / 4);
	});
}
