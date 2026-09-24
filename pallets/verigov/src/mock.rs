use crate as pallet_verigov;
use codec::{Decode, DecodeWithMemTracking, Encode};
use frame_support::{derive_impl, traits::Hooks};
use scale_info::TypeInfo;
use sp_runtime::{
	testing::UintAuthorityId,
	traits::{Lazy, Verify},
	BuildStorage,
};

type Block = frame_system::mocking::MockBlock<Test>;
pub type AccountId = u64;
pub type Balance = u64;

#[frame_support::runtime]
mod runtime {
	#[runtime::runtime]
	#[runtime::derive(
		RuntimeCall,
		RuntimeEvent,
		RuntimeError,
		RuntimeOrigin,
		RuntimeFreezeReason,
		RuntimeHoldReason,
		RuntimeSlashReason,
		RuntimeLockId,
		RuntimeTask,
		RuntimeViewFunction
	)]
	pub struct Test;

	#[runtime::pallet_index(0)]
	pub type System = frame_system::Pallet<Test>;

	#[runtime::pallet_index(1)]
	pub type Balances = pallet_balances::Pallet<Test>;

	#[runtime::pallet_index(2)]
	pub type Verigov = pallet_verigov::Pallet<Test>;
}

#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
	type Block = Block;
	type AccountData = pallet_balances::AccountData<Balance>;
}

#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
	type Balance = Balance;
	type AccountStore = System;
}

/// Test signature: valid iff it was "made" by `signer` over exactly `msg`.
#[derive(Encode, Decode, DecodeWithMemTracking, Clone, Eq, PartialEq, Debug, TypeInfo)]
pub struct MockSignature(pub AccountId, pub Vec<u8>);

impl Verify for MockSignature {
	type Signer = UintAuthorityId;
	fn verify<L: Lazy<[u8]>>(&self, mut msg: L, signer: &AccountId) -> bool {
		*signer == self.0 && msg.get() == &self.1[..]
	}
}

impl pallet_verigov::Config for Test {
	type RuntimeEvent = RuntimeEvent;
	type Currency = Balances;
	type RuntimeHoldReason = RuntimeHoldReason;
	type Public = UintAuthorityId;
	type Signature = MockSignature;
}

// The acme-utils cast (paper, Section 5.6).
pub const ALICE: AccountId = 1;
pub const BOB: AccountId = 2;
pub const CAROL: AccountId = 3;
pub const DAN: AccountId = 4;
pub const EVE: AccountId = 5;
pub const FOO_AUDIT: AccountId = 6;
pub const FUZZ_IO: AccountId = 7;
pub const BUILD_SERVICE: AccountId = 8;
pub const OUTSIDER: AccountId = 9;

pub const INITIAL_BALANCE: Balance = 100_000;

pub fn new_test_ext() -> sp_io::TestExternalities {
	let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();
	pallet_balances::GenesisConfig::<Test> {
		balances: (1..=OUTSIDER).map(|a| (a, INITIAL_BALANCE)).collect(),
		..Default::default()
	}
	.assimilate_storage(&mut t)
	.unwrap();
	let mut ext = sp_io::TestExternalities::new(t);
	ext.execute_with(|| System::set_block_number(1));
	ext
}

/// Advance to block `n`, running `on_initialize` for every block on the way.
pub fn run_to_block(n: u64) {
	while System::block_number() < n {
		let next = System::block_number() + 1;
		System::set_block_number(next);
		Verigov::on_initialize(next);
	}
}
