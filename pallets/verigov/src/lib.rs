//! # VeriGov Pallet
//!
//! On-chain implementation of the VeriGov release-governance protocol:
//!
//! * **Project genesis** registers a project's stakeholders (role + escrowed `VGOV` stake), the
//!   release policy (required roles and their stake-weighted quorums `tau_r`), and the slashing
//!   parameters `(sigma_neg, sigma_mal, delta_max)`.
//! * **Release proposals** (`new_release`) are submitted by a Core Developer and carry the build
//!   attestation `A_build` (source commit hash, source tree hash, build environment hash, output
//!   binary hash) together with the Build Service's signature over it and the content address of
//!   the attestation in the artifact store.
//! * **Voting** is stake-weighted inside each role. A proposal is accepted iff, for every required
//!   role `r`, `sum(stake of aye voters in r) / sum(stake in r) >= tau_r` (paper, Eq. 1).
//! * **Enactment** appends the blessed `output_binary_hash` to the project's append-only Official
//!   Release Log after the configured delay.
//! * **Counter-attestations** (`submit_counter_attestation`) let any other stakeholder report an
//!   independent rebuild whose hash differs from the attested one. The discrepancy is
//!   self-evident on-chain, so the Build Service is negligence-slashed by `sigma_neg` and the
//!   proposal is rejected.
//!
//! `VGOV` is `pallet-balances`; stake and proposal bonds are held via `fungible::MutateHold`.

#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use frame_support::{
	pallet_prelude::*,
	traits::{
		fungible::{Inspect, InspectHold, Mutate, MutateHold},
		tokens::{Fortitude, Precision},
	},
};
use frame_system::pallet_prelude::*;
use scale_info::TypeInfo;
use sp_runtime::{
	traits::{IdentifyAccount, SaturatedConversion, Saturating, Verify, Zero},
	PerThing, Perbill, RuntimeDebug,
};

/// Identifier of a registered project.
pub type ProjectId = u32;
/// Identifier of a release proposal.
pub type ProposalId = u32;
/// A 32-byte digest (sha256 / blake2-256 / padded git hash).
pub type Hash32 = [u8; 32];

pub type NameLimit = ConstU32<64>;
pub type VersionLimit = ConstU32<32>;
pub type MaxStakeholdersPerGenesis = ConstU32<64>;
pub type MaxRequiredRoles = ConstU32<4>;
pub type MaxEnactmentsPerBlock = ConstU32<32>;

pub type BalanceOf<T> =
	<<T as Config>::Currency as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

/// Stakeholder roles (paper, Section 5.1).
#[derive(
	Clone,
	Copy,
	Encode,
	Decode,
	DecodeWithMemTracking,
	Eq,
	PartialEq,
	Ord,
	PartialOrd,
	RuntimeDebug,
	TypeInfo,
	MaxEncodedLen,
)]
pub enum Role {
	CoreDeveloper,
	SecurityAuditor,
	CommunityTrustee,
	BuildService,
}

/// A required role together with its stake-weighted approval quorum `tau_r`.
#[derive(
	Clone, Copy, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct RoleQuorum {
	pub role: Role,
	pub quorum: Perbill,
}

/// The release policy for the `New Release` track.
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct ReleasePolicy<BlockNumber> {
	/// `R_req` with the quorum for each required role.
	pub required_roles: BoundedVec<RoleQuorum, MaxRequiredRoles>,
	/// Number of blocks a proposal stays open for voting.
	pub voting_period: BlockNumber,
	/// Blocks between acceptance and appending to the release log.
	pub enactment_delay: BlockNumber,
}

/// Slashing parameters `(sigma_neg, sigma_mal, delta_max)`.
#[derive(
	Clone, Copy, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct SlashingParams<BlockNumber> {
	/// Fraction of stake slashed for a negligence offence (chain-adjudicated).
	pub sigma_neg: Perbill,
	/// Fraction of stake slashed for a malice offence (dispute track; recorded, not enforced here).
	pub sigma_mal: Perbill,
	/// Statute of limitations for disputes (recorded, not enforced here).
	pub delta_max: BlockNumber,
}

/// Stakeholder entry supplied at project genesis.
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct StakeholderInit<AccountId, Balance> {
	pub account: AccountId,
	pub role: Role,
	pub stake: Balance,
}

/// Registered stakeholder.
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct Stakeholder<Balance> {
	pub role: Role,
	pub stake: Balance,
}

/// Registered project.
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct ProjectInfo<Balance, BlockNumber> {
	pub name: BoundedVec<u8, NameLimit>,
	pub policy: ReleasePolicy<BlockNumber>,
	pub slashing: SlashingParams<BlockNumber>,
	pub proposal_bond: Balance,
}

/// The build attestation `A_build` (paper, Section 4.3) minus the detached signature.
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct BuildAttestation<AccountId> {
	pub source_commit_hash: Hash32,
	pub source_tree_hash: Hash32,
	pub build_environment_hash: Hash32,
	pub output_binary_hash: Hash32,
	/// The Build Service stakeholder that produced and signed this attestation.
	pub builder: AccountId,
}

impl<AccountId> BuildAttestation<AccountId> {
	/// The exact bytes the Build Service signs: the four digests concatenated (128 bytes).
	pub fn signing_payload(&self) -> [u8; 128] {
		let mut out = [0u8; 128];
		out[0..32].copy_from_slice(&self.source_commit_hash);
		out[32..64].copy_from_slice(&self.source_tree_hash);
		out[64..96].copy_from_slice(&self.build_environment_hash);
		out[96..128].copy_from_slice(&self.output_binary_hash);
		out
	}
}

#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub enum ProposalStatus<BlockNumber> {
	/// Open for votes.
	Voting,
	/// Quorums met; will be appended to the release log at `enact_at`.
	Approved { enact_at: BlockNumber },
	/// Appended to the Official Release Log.
	Enacted,
	/// Rejected by an accepted counter-attestation.
	Rejected,
	/// Voting period elapsed without meeting the quorums.
	Expired,
}

#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct Proposal<AccountId, Balance, BlockNumber> {
	pub project_id: ProjectId,
	pub proposer: AccountId,
	pub version: BoundedVec<u8, VersionLimit>,
	pub attestation: BuildAttestation<AccountId>,
	/// Content address of the full signed attestation JSON in the artifact store.
	pub attestation_cid: Hash32,
	pub bond: Balance,
	pub submitted_at: BlockNumber,
	pub voting_ends: BlockNumber,
	pub status: ProposalStatus<BlockNumber>,
}

#[derive(
	Clone, Default, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct RoleTally<Balance> {
	pub ayes: Balance,
	pub nays: Balance,
}

/// One entry of a project's Official Release Log.
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct ReleaseEntry<BlockNumber> {
	pub proposal_id: ProposalId,
	pub version: BoundedVec<u8, VersionLimit>,
	pub source_commit_hash: Hash32,
	pub output_binary_hash: Hash32,
	pub attestation_cid: Hash32,
	pub enacted_at: BlockNumber,
}

#[derive(Clone, Encode, Decode, DecodeWithMemTracking, Eq, PartialEq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct CounterAttestation<AccountId> {
	pub challenger: AccountId,
	pub attested_binary_hash: Hash32,
	pub rebuilt_binary_hash: Hash32,
}

#[frame_support::pallet]
pub mod pallet {
	use super::*;

	#[pallet::pallet]
	pub struct Pallet<T>(_);

	#[pallet::config]
	pub trait Config: frame_system::Config {
		/// The overarching runtime event type.
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

		/// The `VGOV` token (`pallet-balances` in the runtime).
		type Currency: Inspect<Self::AccountId>
			+ Mutate<Self::AccountId>
			+ InspectHold<Self::AccountId>
			+ MutateHold<Self::AccountId, Reason = Self::RuntimeHoldReason>;

		/// The overarching hold reason.
		type RuntimeHoldReason: From<HoldReason>;

		/// Public key type of a Build Service; must identify an `AccountId`.
		type Public: IdentifyAccount<AccountId = Self::AccountId> + Parameter;

		/// Signature type used for `A_build` (`MultiSignature` in the runtime).
		type Signature: Verify<Signer = Self::Public> + Parameter + Member + DecodeWithMemTracking;
	}

	/// Reasons for holding `VGOV`.
	#[pallet::composite_enum]
	pub enum HoldReason {
		/// Stakeholder stake escrowed at project genesis.
		#[codec(index = 0)]
		Stake,
		/// Proposal bond auto-staked by the proposer of a release.
		#[codec(index = 1)]
		ProposalBond,
	}

	#[pallet::storage]
	pub type NextProjectId<T> = StorageValue<_, ProjectId, ValueQuery>;

	#[pallet::storage]
	pub type NextProposalId<T> = StorageValue<_, ProposalId, ValueQuery>;

	#[pallet::storage]
	pub type Projects<T: Config> =
		StorageMap<_, Twox64Concat, ProjectId, ProjectInfo<BalanceOf<T>, BlockNumberFor<T>>>;

	/// The Stakeholder Registry: `(project, account) -> (role, stake)`.
	#[pallet::storage]
	pub type Stakeholders<T: Config> = StorageDoubleMap<
		_,
		Twox64Concat,
		ProjectId,
		Blake2_128Concat,
		T::AccountId,
		Stakeholder<BalanceOf<T>>,
	>;

	/// Total stake held by all stakeholders of a role in a project (denominator of Eq. 1).
	#[pallet::storage]
	pub type RoleStake<T: Config> =
		StorageDoubleMap<_, Twox64Concat, ProjectId, Twox64Concat, Role, BalanceOf<T>, ValueQuery>;

	#[pallet::storage]
	pub type Proposals<T: Config> = StorageMap<
		_,
		Twox64Concat,
		ProposalId,
		Proposal<T::AccountId, BalanceOf<T>, BlockNumberFor<T>>,
	>;

	/// Recorded votes: `(proposal, voter) -> aye?`.
	#[pallet::storage]
	pub type Votes<T: Config> =
		StorageDoubleMap<_, Twox64Concat, ProposalId, Blake2_128Concat, T::AccountId, bool>;

	/// Stake-weighted tally per role.
	#[pallet::storage]
	pub type Tallies<T: Config> = StorageDoubleMap<
		_,
		Twox64Concat,
		ProposalId,
		Twox64Concat,
		Role,
		RoleTally<BalanceOf<T>>,
		ValueQuery,
	>;

	/// Number of entries in a project's Official Release Log.
	#[pallet::storage]
	pub type ReleaseCount<T> = StorageMap<_, Twox64Concat, ProjectId, u32, ValueQuery>;

	/// The Official Release Log: `(project, index) -> entry`. Append-only.
	#[pallet::storage]
	pub type ReleaseLog<T: Config> =
		StorageDoubleMap<_, Twox64Concat, ProjectId, Twox64Concat, u32, ReleaseEntry<BlockNumberFor<T>>>;

	/// Accepted counter-attestations.
	#[pallet::storage]
	pub type CounterAttestations<T: Config> =
		StorageMap<_, Twox64Concat, ProposalId, CounterAttestation<T::AccountId>>;

	/// Proposals scheduled for enactment at a given block.
	#[pallet::storage]
	pub type PendingEnactments<T: Config> = StorageMap<
		_,
		Twox64Concat,
		BlockNumberFor<T>,
		BoundedVec<ProposalId, MaxEnactmentsPerBlock>,
		ValueQuery,
	>;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		ProjectCreated { project_id: ProjectId, name: BoundedVec<u8, NameLimit> },
		StakeholderRegistered {
			project_id: ProjectId,
			account: T::AccountId,
			role: Role,
			stake: BalanceOf<T>,
		},
		ReleaseProposed {
			proposal_id: ProposalId,
			project_id: ProjectId,
			proposer: T::AccountId,
			version: BoundedVec<u8, VersionLimit>,
			source_commit_hash: Hash32,
			output_binary_hash: Hash32,
			attestation_cid: Hash32,
			voting_ends: BlockNumberFor<T>,
		},
		Voted {
			proposal_id: ProposalId,
			voter: T::AccountId,
			role: Role,
			aye: bool,
			weight: BalanceOf<T>,
		},
		/// All required-role quorums are met; enactment is scheduled.
		ProposalApproved { proposal_id: ProposalId, enact_at: BlockNumberFor<T> },
		/// A changed vote broke a quorum before enactment; the proposal is back to voting.
		ProposalApprovalRevoked { proposal_id: ProposalId },
		/// The blessed binary hash was appended to the Official Release Log.
		ReleaseEnacted {
			project_id: ProjectId,
			proposal_id: ProposalId,
			index: u32,
			version: BoundedVec<u8, VersionLimit>,
			output_binary_hash: Hash32,
		},
		CounterAttestationAccepted {
			proposal_id: ProposalId,
			challenger: T::AccountId,
			attested_binary_hash: Hash32,
			rebuilt_binary_hash: Hash32,
		},
		BuildServiceSlashed {
			project_id: ProjectId,
			builder: T::AccountId,
			amount: BalanceOf<T>,
			remaining_stake: BalanceOf<T>,
		},
		ProposalRejected { proposal_id: ProposalId },
		ProposalExpired { proposal_id: ProposalId },
	}

	#[pallet::error]
	pub enum Error<T> {
		ProjectNotFound,
		ProposalNotFound,
		/// The caller is not a registered stakeholder of the project.
		NotStakeholder,
		/// Only Core Developers may submit release proposals.
		NotCoreDeveloper,
		/// The attestation's `builder` is not a registered Build Service of the project.
		NotBuildService,
		DuplicateStakeholder,
		/// The release policy must name at least one required role.
		NoRequiredRoles,
		/// A required role has no staked stakeholders, so its quorum could never be met.
		RequiredRoleHasNoStake,
		/// The Build Service signature over `A_build` does not verify.
		InvalidBuilderSignature,
		/// The proposal is not open for voting.
		ProposalNotVoting,
		VotingPeriodOver,
		VotingPeriodNotOver,
		/// Only proposals that are voting or awaiting enactment can be counter-attested.
		ProposalNotChallengeable,
		/// The rebuilt hash equals the attested hash: there is no discrepancy to slash for.
		NoDiscrepancy,
		/// The Build Service cannot counter-attest its own attestation.
		BuilderCannotChallenge,
		TooManyEnactments,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn on_initialize(now: BlockNumberFor<T>) -> Weight {
			let ids = PendingEnactments::<T>::take(now);
			let mut weight = T::DbWeight::get().reads_writes(1, 1);
			for id in ids {
				Self::enact(id, now);
				weight = weight.saturating_add(T::DbWeight::get().reads_writes(3, 4));
			}
			weight
		}
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// `ProjectGenesis`: register a project, its stakeholders (escrowing each one's initial
		/// stake), its release policy and its slashing parameters. Root-only in this prototype;
		/// root stands in for the founding maintainer set's collective signature.
		#[pallet::call_index(0)]
		#[pallet::weight(Weight::from_parts(80_000_000, 0)
			.saturating_add(T::DbWeight::get().reads_writes(
				2 + 2 * stakeholders.len() as u64,
				3 + 3 * stakeholders.len() as u64,
			)))]
		pub fn project_genesis(
			origin: OriginFor<T>,
			name: BoundedVec<u8, NameLimit>,
			stakeholders: BoundedVec<StakeholderInit<T::AccountId, BalanceOf<T>>, MaxStakeholdersPerGenesis>,
			policy: ReleasePolicy<BlockNumberFor<T>>,
			slashing: SlashingParams<BlockNumberFor<T>>,
			proposal_bond: BalanceOf<T>,
		) -> DispatchResult {
			ensure_root(origin)?;
			ensure!(!policy.required_roles.is_empty(), Error::<T>::NoRequiredRoles);

			let project_id = NextProjectId::<T>::get();

			for s in stakeholders.iter() {
				ensure!(
					!Stakeholders::<T>::contains_key(project_id, &s.account),
					Error::<T>::DuplicateStakeholder
				);
				T::Currency::hold(&HoldReason::Stake.into(), &s.account, s.stake)?;
				Stakeholders::<T>::insert(
					project_id,
					&s.account,
					Stakeholder { role: s.role, stake: s.stake },
				);
				RoleStake::<T>::mutate(project_id, s.role, |t| *t = t.saturating_add(s.stake));
				Self::deposit_event(Event::StakeholderRegistered {
					project_id,
					account: s.account.clone(),
					role: s.role,
					stake: s.stake,
				});
			}

			for rq in policy.required_roles.iter() {
				ensure!(
					!RoleStake::<T>::get(project_id, rq.role).is_zero(),
					Error::<T>::RequiredRoleHasNoStake
				);
			}

			Projects::<T>::insert(
				project_id,
				ProjectInfo { name: name.clone(), policy, slashing, proposal_bond },
			);
			NextProjectId::<T>::put(project_id.saturating_add(1));
			Self::deposit_event(Event::ProjectCreated { project_id, name });
			Ok(())
		}

		/// `NewRelease`: a Core Developer proposes a release. The attestation must be signed by a
		/// registered Build Service of the project. A proposal bond is auto-held from the
		/// proposer, who is also recorded as an aye vote.
		#[pallet::call_index(1)]
		#[pallet::weight(Weight::from_parts(120_000_000, 0)
			.saturating_add(T::DbWeight::get().reads_writes(6, 6)))]
		pub fn new_release(
			origin: OriginFor<T>,
			project_id: ProjectId,
			version: BoundedVec<u8, VersionLimit>,
			attestation: BuildAttestation<T::AccountId>,
			attestation_cid: Hash32,
			builder_signature: T::Signature,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let project = Projects::<T>::get(project_id).ok_or(Error::<T>::ProjectNotFound)?;

			let proposer =
				Stakeholders::<T>::get(project_id, &who).ok_or(Error::<T>::NotStakeholder)?;
			ensure!(proposer.role == Role::CoreDeveloper, Error::<T>::NotCoreDeveloper);

			let builder = Stakeholders::<T>::get(project_id, &attestation.builder)
				.ok_or(Error::<T>::NotBuildService)?;
			ensure!(builder.role == Role::BuildService, Error::<T>::NotBuildService);
			ensure!(
				builder_signature.verify(&attestation.signing_payload()[..], &attestation.builder),
				Error::<T>::InvalidBuilderSignature
			);

			T::Currency::hold(&HoldReason::ProposalBond.into(), &who, project.proposal_bond)?;

			let now = frame_system::Pallet::<T>::block_number();
			let voting_ends = now.saturating_add(project.policy.voting_period);
			let proposal_id = NextProposalId::<T>::get();
			NextProposalId::<T>::put(proposal_id.saturating_add(1));

			Proposals::<T>::insert(
				proposal_id,
				Proposal {
					project_id,
					proposer: who.clone(),
					version: version.clone(),
					attestation: attestation.clone(),
					attestation_cid,
					bond: project.proposal_bond,
					submitted_at: now,
					voting_ends,
					status: ProposalStatus::Voting,
				},
			);

			Self::deposit_event(Event::ReleaseProposed {
				proposal_id,
				project_id,
				proposer: who.clone(),
				version,
				source_commit_hash: attestation.source_commit_hash,
				output_binary_hash: attestation.output_binary_hash,
				attestation_cid,
				voting_ends,
			});

			// The proposer's submission counts as their aye.
			Self::do_vote(proposal_id, project_id, &project, &who, &proposer, true, now)?;
			Ok(())
		}

		/// `Vote`: a stakeholder votes on a proposal with weight equal to their stake. Re-voting
		/// replaces the previous vote. Votes stay open until enactment, so the tally that is
		/// enacted is the one standing at the enactment block.
		#[pallet::call_index(2)]
		#[pallet::weight(Weight::from_parts(80_000_000, 0)
			.saturating_add(T::DbWeight::get().reads_writes(6, 4)))]
		pub fn vote(origin: OriginFor<T>, proposal_id: ProposalId, aye: bool) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let proposal = Proposals::<T>::get(proposal_id).ok_or(Error::<T>::ProposalNotFound)?;
			ensure!(
				matches!(proposal.status, ProposalStatus::Voting | ProposalStatus::Approved { .. }),
				Error::<T>::ProposalNotVoting
			);
			let now = frame_system::Pallet::<T>::block_number();
			ensure!(now <= proposal.voting_ends, Error::<T>::VotingPeriodOver);

			let project =
				Projects::<T>::get(proposal.project_id).ok_or(Error::<T>::ProjectNotFound)?;
			let voter = Stakeholders::<T>::get(proposal.project_id, &who)
				.ok_or(Error::<T>::NotStakeholder)?;

			Self::do_vote(proposal_id, proposal.project_id, &project, &who, &voter, aye, now)
		}

		/// Report an independent reproducible rebuild whose output hash differs from the attested
		/// `output_binary_hash`. The discrepancy is verified on-chain; the Build Service is slashed
		/// by `sigma_neg` of its stake and the proposal is rejected (bond returned to proposer).
		#[pallet::call_index(3)]
		#[pallet::weight(Weight::from_parts(120_000_000, 0)
			.saturating_add(T::DbWeight::get().reads_writes(6, 7)))]
		pub fn submit_counter_attestation(
			origin: OriginFor<T>,
			proposal_id: ProposalId,
			rebuilt_binary_hash: Hash32,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let mut proposal =
				Proposals::<T>::get(proposal_id).ok_or(Error::<T>::ProposalNotFound)?;
			let project_id = proposal.project_id;
			let project = Projects::<T>::get(project_id).ok_or(Error::<T>::ProjectNotFound)?;

			ensure!(
				Stakeholders::<T>::contains_key(project_id, &who),
				Error::<T>::NotStakeholder
			);
			let builder_acct = proposal.attestation.builder.clone();
			ensure!(who != builder_acct, Error::<T>::BuilderCannotChallenge);
			ensure!(
				rebuilt_binary_hash != proposal.attestation.output_binary_hash,
				Error::<T>::NoDiscrepancy
			);

			match proposal.status {
				ProposalStatus::Voting => {},
				ProposalStatus::Approved { enact_at } => {
					PendingEnactments::<T>::mutate(enact_at, |ids| ids.retain(|id| *id != proposal_id));
				},
				_ => return Err(Error::<T>::ProposalNotChallengeable.into()),
			}

			// Negligence slash of the Build Service.
			let mut builder = Stakeholders::<T>::get(project_id, &builder_acct)
				.ok_or(Error::<T>::NotBuildService)?;
			let to_slash = project.slashing.sigma_neg.mul_floor(builder.stake);
			let slashed = T::Currency::burn_held(
				&HoldReason::Stake.into(),
				&builder_acct,
				to_slash,
				Precision::BestEffort,
				Fortitude::Force,
			)?;
			builder.stake = builder.stake.saturating_sub(slashed);
			Stakeholders::<T>::insert(project_id, &builder_acct, builder.clone());
			RoleStake::<T>::mutate(project_id, Role::BuildService, |t| {
				*t = t.saturating_sub(slashed)
			});

			// Reject the proposal and return the good-faith proposer's bond.
			T::Currency::release(
				&HoldReason::ProposalBond.into(),
				&proposal.proposer,
				proposal.bond,
				Precision::BestEffort,
			)?;
			proposal.status = ProposalStatus::Rejected;
			let attested_binary_hash = proposal.attestation.output_binary_hash;
			Proposals::<T>::insert(proposal_id, proposal);
			CounterAttestations::<T>::insert(
				proposal_id,
				CounterAttestation {
					challenger: who.clone(),
					attested_binary_hash,
					rebuilt_binary_hash,
				},
			);

			Self::deposit_event(Event::CounterAttestationAccepted {
				proposal_id,
				challenger: who,
				attested_binary_hash,
				rebuilt_binary_hash,
			});
			Self::deposit_event(Event::BuildServiceSlashed {
				project_id,
				builder: builder_acct,
				amount: slashed,
				remaining_stake: builder.stake,
			});
			Self::deposit_event(Event::ProposalRejected { proposal_id });
			Ok(())
		}

		/// Close a proposal whose voting period elapsed without meeting the quorums, returning the
		/// proposer's bond.
		#[pallet::call_index(4)]
		#[pallet::weight(Weight::from_parts(50_000_000, 0)
			.saturating_add(T::DbWeight::get().reads_writes(2, 2)))]
		pub fn close_expired(origin: OriginFor<T>, proposal_id: ProposalId) -> DispatchResult {
			ensure_signed(origin)?;
			let mut proposal =
				Proposals::<T>::get(proposal_id).ok_or(Error::<T>::ProposalNotFound)?;
			ensure!(proposal.status == ProposalStatus::Voting, Error::<T>::ProposalNotVoting);
			let now = frame_system::Pallet::<T>::block_number();
			ensure!(now > proposal.voting_ends, Error::<T>::VotingPeriodNotOver);

			T::Currency::release(
				&HoldReason::ProposalBond.into(),
				&proposal.proposer,
				proposal.bond,
				Precision::BestEffort,
			)?;
			proposal.status = ProposalStatus::Expired;
			Proposals::<T>::insert(proposal_id, proposal);
			Self::deposit_event(Event::ProposalExpired { proposal_id });
			Ok(())
		}
	}

	impl<T: Config> Pallet<T> {
		/// Record a vote, update the role tally, and schedule enactment if Eq. 1 now holds.
		fn do_vote(
			proposal_id: ProposalId,
			project_id: ProjectId,
			project: &ProjectInfo<BalanceOf<T>, BlockNumberFor<T>>,
			who: &T::AccountId,
			voter: &Stakeholder<BalanceOf<T>>,
			aye: bool,
			now: BlockNumberFor<T>,
		) -> DispatchResult {
			Tallies::<T>::mutate(proposal_id, voter.role, |t| {
				if let Some(prev) = Votes::<T>::get(proposal_id, who) {
					if prev {
						t.ayes = t.ayes.saturating_sub(voter.stake);
					} else {
						t.nays = t.nays.saturating_sub(voter.stake);
					}
				}
				if aye {
					t.ayes = t.ayes.saturating_add(voter.stake);
				} else {
					t.nays = t.nays.saturating_add(voter.stake);
				}
			});
			Votes::<T>::insert(proposal_id, who, aye);
			Self::deposit_event(Event::Voted {
				proposal_id,
				voter: who.clone(),
				role: voter.role,
				aye,
				weight: voter.stake,
			});

			let met = Self::meets_quorums(project_id, project, proposal_id);
			let status = Proposals::<T>::get(proposal_id)
				.map(|p| p.status)
				.ok_or(Error::<T>::ProposalNotFound)?;
			match status {
				ProposalStatus::Voting if met => {
					let enact_at = now.saturating_add(project.policy.enactment_delay);
					PendingEnactments::<T>::try_mutate(enact_at, |ids| {
						ids.try_push(proposal_id).map_err(|_| Error::<T>::TooManyEnactments)
					})?;
					Self::set_status(proposal_id, ProposalStatus::Approved { enact_at });
					Self::deposit_event(Event::ProposalApproved { proposal_id, enact_at });
				},
				ProposalStatus::Approved { enact_at } if !met => {
					// A changed vote broke a quorum before enactment: fall back to voting.
					PendingEnactments::<T>::mutate(enact_at, |ids| {
						ids.retain(|id| *id != proposal_id)
					});
					Self::set_status(proposal_id, ProposalStatus::Voting);
					Self::deposit_event(Event::ProposalApprovalRevoked { proposal_id });
				},
				_ => {},
			}
			Ok(())
		}

		fn set_status(proposal_id: ProposalId, status: ProposalStatus<BlockNumberFor<T>>) {
			Proposals::<T>::mutate(proposal_id, |p| {
				if let Some(p) = p {
					p.status = status;
				}
			});
		}

		/// Eq. 1: for every required role `r`, `ayes_r / stake_r >= tau_r`.
		pub fn meets_quorums(
			project_id: ProjectId,
			project: &ProjectInfo<BalanceOf<T>, BlockNumberFor<T>>,
			proposal_id: ProposalId,
		) -> bool {
			project.policy.required_roles.iter().all(|rq| {
				let total: u128 = RoleStake::<T>::get(project_id, rq.role).saturated_into();
				if total == 0 {
					return false;
				}
				let ayes: u128 = Tallies::<T>::get(proposal_id, rq.role).ayes.saturated_into();
				// ayes / total >= quorum  <=>  ayes * ACCURACY >= quorum_parts * total
				ayes.saturating_mul(Perbill::ACCURACY as u128) >=
					(rq.quorum.deconstruct() as u128).saturating_mul(total)
			})
		}

		/// Append an approved proposal to the Official Release Log.
		fn enact(proposal_id: ProposalId, now: BlockNumberFor<T>) {
			let Some(mut proposal) = Proposals::<T>::get(proposal_id) else { return };
			if !matches!(proposal.status, ProposalStatus::Approved { .. }) {
				return;
			}
			let project_id = proposal.project_id;
			// Eq. 1 is evaluated against the tally standing at the enactment block.
			let Some(project) = Projects::<T>::get(project_id) else { return };
			if !Self::meets_quorums(project_id, &project, proposal_id) {
				Self::set_status(proposal_id, ProposalStatus::Voting);
				Self::deposit_event(Event::ProposalApprovalRevoked { proposal_id });
				return;
			}
			let index = ReleaseCount::<T>::get(project_id);
			ReleaseLog::<T>::insert(
				project_id,
				index,
				ReleaseEntry {
					proposal_id,
					version: proposal.version.clone(),
					source_commit_hash: proposal.attestation.source_commit_hash,
					output_binary_hash: proposal.attestation.output_binary_hash,
					attestation_cid: proposal.attestation_cid,
					enacted_at: now,
				},
			);
			ReleaseCount::<T>::insert(project_id, index.saturating_add(1));

			// Bond is returned on successful enactment. Best effort: never block enactment.
			let _ = T::Currency::release(
				&HoldReason::ProposalBond.into(),
				&proposal.proposer,
				proposal.bond,
				Precision::BestEffort,
			);
			proposal.status = ProposalStatus::Enacted;
			let version = proposal.version.clone();
			let output_binary_hash = proposal.attestation.output_binary_hash;
			Proposals::<T>::insert(proposal_id, proposal);

			Self::deposit_event(Event::ReleaseEnacted {
				project_id,
				proposal_id,
				index,
				version,
				output_binary_hash,
			});
		}
	}
}
