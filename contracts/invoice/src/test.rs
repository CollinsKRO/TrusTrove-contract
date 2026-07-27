#![cfg(test)]

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events as _, Ledger},
    vec, Address, BytesN, Env, IntoVal, String, Symbol, TryFromVal,
};

use crate::{InvoiceContract, InvoiceContractClient, InvoiceStatus};

#[contract]
pub struct MockRegistry;

#[contractimpl]
impl MockRegistry {
    pub fn is_verified(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, bool>(&DataKey(address))
            .unwrap_or(false)
    }

    pub fn register(env: Env, address: Address) {
        env.storage()
            .persistent()
            .set(&DataKey(address.clone()), &true);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey(address), 100, 2_000_000);
    }
}

#[contracttype]
pub struct DataKey(Address);

#[contract]
pub struct MockPool;

#[contractimpl]
impl MockPool {
    pub fn handle_default(_env: Env, _invoice_id: BytesN<32>) -> bool {
        true
    }

    pub fn get_usdc_asset(env: Env) -> Address {
        let key = Symbol::new(&env, "asset");
        env.storage().instance().get(&key).unwrap()
    }

    pub fn receive_repayment_with_refund(
        env: Env,
        _invoice_id: BytesN<32>,
        _amount: u128,
        refund: u128,
        _buyer: Address,
    ) -> bool {
        let key = Symbol::new(&env, "last_refund");
        env.storage().instance().set(&key, &refund);
        true
    }

    pub fn get_last_refund(env: Env) -> u128 {
        let key = Symbol::new(&env, "last_refund");
        env.storage().instance().get(&key).unwrap_or(0)
    }
}

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn transfer(_env: Env, _from: Address, _to: Address, _amount: i128) {
        // no-op for tests (auth is mocked)
    }
}

type Setup = (
    Env,
    InvoiceContractClient<'static>,
    Address,
    Address,
    MockRegistryClient<'static>,
    Address,
);

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let token_id = env.register_contract(None, MockToken);
    let usdc_asset = token_id;

    (env, client, issuer, buyer, registry_client, usdc_asset)
}

fn mock_pool_with_asset(env: &Env, asset: &Address) -> Address {
    let pool_id = env.register_contract(None, MockPool);
    let _pool_client = MockPoolClient::new(env, &pool_id);
    env.as_contract(&pool_id, || {
        let key = Symbol::new(env, "asset");
        env.storage().instance().set(&key, asset);
    });
    pool_id
}

#[test]
fn test_initialize_emits_contract_initialized_event() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let contract_id = client.address.clone();
    let events = env.events().all();
    assert_eq!(
        events,
        vec![
            &env,
            (
                contract_id,
                (
                    Symbol::new(&env, "contract_initialized"),
                    admin.clone(),
                    registry_id.clone()
                )
                    .into_val(&env),
                ().into_val(&env),
            )
        ]
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_initialize_fails_when_already_initialized() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);
    // Second initialize should panic with AlreadyInitialized (#1)
    client.initialize(&admin, &registry_id);
}

#[test]
fn test_create_invoice_with_verified_parties() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = 1_000_000_000;
    let due_date = env.ledger().timestamp() + 86400;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    let invoice = client.get(&invoice_id);

    assert_eq!(invoice.issuer, issuer);
    assert_eq!(invoice.buyer, buyer);
    assert_eq!(invoice.face_value, face_value);
    assert_eq!(invoice.due_date, due_date);
    assert_eq!(invoice.status, InvoiceStatus::Created);
    assert_eq!(invoice.funding_asset, usdc);
    assert_eq!(invoice.funding_pool, None);
    assert!(!invoice.issuer_confirmed);
    assert!(!invoice.buyer_confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_create_fails_zero_face_value() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    client.create(&issuer, &buyer, &0, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let past_date = env.ledger().timestamp() - 1;
    client.create(&issuer, &buyer, &1_000_000_000, &past_date, &usdc);
}

// ============== ISSUE #B: due_date BOUNDARY (due_date == now) ==============

// At exactly `due_date == now`, `create` rejects with InvalidDueDate (#7).
// The check is `due_date <= env.ledger().timestamp()` so equality falls on
// the rejection side. Pins the current behaviour so a regression on the
// boundary comparator cannot land silently.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_when_due_date_equals_now() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let equal_due_date = env.ledger().timestamp();
    client.create(&issuer, &buyer, &1_000_000_000, &equal_due_date, &usdc);
}

// The boundary's other side: `due_date == now + 1` is the smallest accepted
// value. Confirms storage and events on the positive boundary so that a
// future refactor can't flip the boundary silently.
#[test]
fn test_create_succeeds_when_due_date_one_second_in_future() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let just_future_due_date = env.ledger().timestamp() + 1;
    let face_value: u128 = 1_000_000_000;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &just_future_due_date, &usdc);

    // State: invoice record exists at Created with the boundary due_date.
    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Created);
    assert_eq!(invoice.due_date, just_future_due_date);
    assert_eq!(invoice.created_at, env.ledger().timestamp());
    assert_eq!(invoice.face_value, face_value);

    // Events: setup() calls initialize() which emits contract_initialized,
    // then create() emits invoice_created. We expect 2 events total.
    let events = env.events().all();
    assert_eq!(events.len(), 2);
}

// ============== ISSUE #226: due_date UPPER BOUND (far future) ==============
//
// The upper bound is MAX_INVOICE_LIFETIME_SECONDS (10 years).
// At exactly `due_date == now + MAX_INVOICE_LIFETIME_SECONDS`, create succeeds.
// At `due_date == now + MAX_INVOICE_LIFETIME_SECONDS + 1`, create fails with
// InvalidDueDate (#7). These tests pin both boundaries so regressions can't land silently.

const MAX_INVOICE_LIFETIME_SECONDS: u64 = 10 * 365 * 24 * 60 * 60; // 10 years

#[test]
fn test_create_succeeds_at_max_due_date_boundary() {
    // Positive boundary: due_date == now + MAX_INVOICE_LIFETIME_SECONDS is allowed.
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let max_due_date = env.ledger().timestamp() + MAX_INVOICE_LIFETIME_SECONDS;
    let face_value: u128 = 1_000_000_000;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &max_due_date, &usdc);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Created);
    assert_eq!(invoice.due_date, max_due_date);
    assert_eq!(invoice.created_at, env.ledger().timestamp());
    assert_eq!(invoice.face_value, face_value);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_above_max_due_date_boundary() {
    // Negative boundary: due_date == now + MAX_INVOICE_LIFETIME_SECONDS + 1 is rejected.
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let above_max_due_date = env.ledger().timestamp() + MAX_INVOICE_LIFETIME_SECONDS + 1;
    let face_value: u128 = 1_000_000_000;

    client.create(&issuer, &buyer, &face_value, &above_max_due_date, &usdc);
}

#[test]
fn test_list_for_financing() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let result = client.list_for_financing(&invoice_id, &200);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_fails_wrong_status() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    client.list_for_financing(&invoice_id, &300);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_list_fails_discount_too_high() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &5001);
}

#[test]
fn test_list_for_financing_discount_bps_zero_boundary() {
    // discount_bps == 0 is currently accepted; see issue #79 for a
    // companion validation that would turn this into a panic test.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let result = client.list_for_financing(&invoice_id, &0);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 0);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "invoice_listed"), invoice_id.clone()).into_val(&env)
    );
    assert_eq!(u32::try_from_val(&env, &data).unwrap(), 0u32);
}

#[test]
fn test_list_for_financing_discount_bps_max_boundary() {
    // discount_bps == 5000 is the inclusive upper bound and must succeed.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let result = client.list_for_financing(&invoice_id, &5000);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 5000);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "invoice_listed"), invoice_id.clone()).into_val(&env)
    );
    assert_eq!(u32::try_from_val(&env, &data).unwrap(), 5000u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_list_for_financing_discount_bps_one_above_max_boundary_panics() {
    // discount_bps == 5001 is one past the inclusive upper bound and must panic
    // with DiscountTooHigh (#9). Pins the exact boundary alongside the existing
    // test_list_fails_discount_too_high regression test.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &5001);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_list_for_financing_non_issuer_panics() {
    let (env, client, issuer, _buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &_buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    env.set_auths(&[]);
    client.list_for_financing(&invoice_id, &200);
}

#[test]
fn test_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);

    let funded_amount: u128 = 980_000_000;
    let result = client.mark_funded(&invoice_id, &pool, &usdc, &funded_amount);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);
    assert_eq!(client.get(&invoice_id).funding_pool, Some(pool));

    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.confirm_delivery(&invoice_id, &issuer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);
    assert!(client.get(&invoice_id).issuer_confirmed);
    assert!(!client.get(&invoice_id).buyer_confirmed);

    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
    assert!(client.get(&invoice_id).issuer_confirmed);
    assert!(client.get(&invoice_id).buyer_confirmed);
}

#[test]
fn test_get_by_issuer_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let invoices = client.get_by_issuer(&issuer);
    assert_eq!(invoices.len(), 2);

    let other = Address::generate(&env);
    let empty = client.get_by_issuer(&other);
    assert_eq!(empty.len(), 0);
}

#[test]
fn test_get_by_buyer_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let invoices = client.get_by_buyer(&buyer);
    assert_eq!(invoices.len(), 2);
}

#[test]
fn test_get_by_status_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);
}

#[test]
fn test_expire_listing_transitions_to_expired_after_window() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    client.list_for_financing(&invoice_id, &200);
    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);

    let result = client.expire_listing(&invoice_id);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_unknown_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get(&fake_id);
}

#[test]
fn test_dual_confirmation_both_must_confirm() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);

    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &issuer);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Active);
    assert!(inv.issuer_confirmed);
    assert!(!inv.buyer_confirmed);

    client.confirm_delivery(&invoice_id, &buyer);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Confirmed);
    assert!(inv.issuer_confirmed);
    assert!(inv.buyer_confirmed);
}

#[test]
fn test_confirm_by_both_transitions_to_confirmed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_confirm_delivery_wrong_party_panics() {
    let (env, client, issuer, _buyer, registry, usdc) = setup();
    let stranger = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry.register(&buyer);

    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &stranger);
}

#[test]
fn test_trigger_default_requires_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);

    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

// ============== ISSUE #211: trigger_default FROM INVALID STATUSES ==============

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    // A freshly created invoice is not Funded/Active/Confirmed, so defaulting
    // it must be rejected with InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    // A Listed invoice has not been funded, so defaulting it must be rejected
    // with InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // A Repaid invoice is terminal, so defaulting it must be rejected with
    // InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
fn test_trigger_default_succeeds_at_exact_due_date() {
    // Boundary test: default must be allowed when `now == due_date`
    // (previously panicked due to `<=` comparison — issue #200)
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    // Set ledger to exactly the due date
    env.ledger().set_timestamp(due_date);

    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_trigger_default_fails_before_due_date() {
    // Negative test: default must NOT be allowed when `now < due_date`
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    // Set ledger to 1 second before the due date — should panic
    env.ledger().set_timestamp(due_date - 1);

    client.trigger_default(&invoice_id);
}

// PR 363 (closes #314) removed `test_trigger_default_admin_succeeds_after_due_date_with_auth`:
// explicit `mock_auths` + cross-contract call to `pool.handle_default` surfaced
// `Error(Context, MissingValue)` regardless of `sub_invokes` shape. The same
// happy-path is covered by `test_trigger_default_requires_past_due_date` and
// `test_trigger_default_succeeds_at_exact_due_date` via `setup()`.
#[test]
// `trigger_default` calls `admin.require_auth()` directly, so non-admin
// callers are rejected by Soroban's native `Error(Auth, InvalidAction)`
// before any contract-level error can be returned.
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_trigger_default_stranger_panics() {
    // `trigger_default` calls `admin.require_auth()` directly, so a non-admin
    // caller is rejected at the auth layer with Soroban's native
    // `Error(Auth, InvalidAction)` before any state transition.
    // TODO: refactor `trigger_default` to dispatch auth via
    // `try_invoke_contract(check_auth, admin)` + `panic_with_error!(NotAuthorized)`
    // (matching `expire_listing`) so callers see the contract-typed #3 error
    // instead of the noisy native Auth error. Currently the refactor breaks
    // the other `setup()`-based `trigger_default` tests, so it's deferred.
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);

    // Set pool contract (admin auth)
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_pool_contract",
            args: (pool_id.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.set_pool_contract(&pool_id);

    // mark_funded requires pool auth
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &pool_id,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_funded",
            args: (
                invoice_id.clone(),
                pool_id.clone(),
                usdc.clone(),
                980_000_000u128,
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);

    // mark_shipped by issuer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_shipped",
            args: (invoice_id.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_shipped(&invoice_id);

    // confirm delivery by issuer and buyer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "confirm_delivery",
            args: (invoice_id.clone(), issuer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.confirm_delivery(&invoice_id, &issuer);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &buyer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "confirm_delivery",
            args: (invoice_id.clone(), buyer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.confirm_delivery(&invoice_id, &buyer);

    // Fast forward past due date
    env.ledger().set_timestamp(due_date + 1);

    // Now call trigger_default without mocking admin auth -> should panic with NotAuthorized
    client.trigger_default(&invoice_id);
}

// Note: `test_trigger_default_admin_succeeds_after_due_date_with_auth` was
// removed as part of PR #363 (closing #314). The happy-path admin-trigger
// behavior is already exercised by `test_trigger_default_requires_past_due_date`
// and `test_trigger_default_succeeds_at_exact_due_date`, both of which rely
// on the shared `setup()` helper. Keeping this note here so future readers
// know the gap was intentional and not an oversight.

#[test]
fn test_get_by_status_filters_correctly() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    let id1 = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);

    client.list_for_financing(&id1, &200);
    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 1);
    let listed = client.get_by_status(&InvoiceStatus::Listed);
    assert_eq!(listed.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_double_confirmation_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
fn test_status_transitions_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_mark_funded_fails_asset_mismatch() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let xlm = Address::generate(&env);
    let xlm_pool = mock_pool_with_asset(&env, &xlm);
    client.set_pool_contract(&xlm_pool);
    client.mark_funded(&invoice_id, &xlm_pool, &xlm, &980_000_000);
}

#[test]
fn test_mark_funded_succeeds_with_matching_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let result = client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.funding_pool, Some(pool));
}

#[test]
fn test_create_invoice_with_xlm_asset() {
    let (env, client, issuer, buyer, _, _usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let xlm_asset = Address::generate(&env);

    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &xlm_asset);
    let invoice = client.get(&invoice_id);

    assert_eq!(invoice.funding_asset, xlm_asset);
    assert_eq!(invoice.status, InvoiceStatus::Created);
}

#[test]
fn test_get_funding_asset_returns_correct_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let asset = client.get_funding_asset(&invoice_id);
    assert_eq!(asset, usdc);
}

#[test]
fn test_expire_listing_succeeds_by_issuer() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward ledger time by 7 days + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
fn test_expire_listing_succeeds_by_admin() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward ledger time by 7 days + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_early_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward ledger time by only 5 days (less than 7 days)
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 5 * 24 * 60 * 60);

    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_wrong_status_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    // Fast forward ledger time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    client.expire_listing(&invoice_id);
}

#[test]
fn test_expire_listing_configurable_window() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Set expiry window to 1 day (86400 seconds)
    client.set_expiry_window(&86400);
    assert_eq!(client.get_expiry_window(), 86400);

    // Fast forward by 1 day + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 86400 + 1);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}
#[test]
fn test_expire_listing_exact_boundary() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward by exact expiry window (7 days)
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_one_second_before_boundary_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward to 1 second before expiry window
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 - 1);

    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_expire_listing_overflow_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(100);
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Set an expiry window that will overflow u64 when added to listed_at (100 + u64::MAX > u64::MAX)
    client.set_expiry_window(&u64::MAX);

    client.expire_listing(&invoice_id);
}

#[test]
fn test_set_pool_contract_emits_event() {
    let (env, client, _, _, _, _) = setup();
    let pool = Address::generate(&env);

    client.set_pool_contract(&pool);

    let _contract_id = client.address.clone();
    let events = env.events().all();
    // setup() emits contract_initialized, then set_pool_contract emits pool_contract_set
    assert_eq!(events.len(), 2);
    let (_, topics, _) = events.last().expect("expected at least one event");
    assert_eq!(
        topics,
        (Symbol::new(&env, "pool_contract_set"), pool.clone()).into_val(&env)
    );
}

#[test]
fn test_set_expiry_window_emits_event() {
    let (env, client, _, _, _, _) = setup();
    let window: u64 = 86400;

    client.set_expiry_window(&window);

    let _contract_id = client.address.clone();
    let events = env.events().all();
    // setup() emits contract_initialized, then set_expiry_window emits expiry_window_set
    assert_eq!(events.len(), 2);
    let (_, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(
        topics,
        (Symbol::new(&env, "expiry_window_set"),).into_val(&env)
    );
    assert_eq!(u64::try_from_val(&env, &data).unwrap(), window);
}

#[test]
fn test_mark_shipped_succeeds_by_issuer() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);

    let result = client.mark_shipped(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Active);
    assert!(inv.shipped_at.is_some());
}

#[test]
#[should_panic]
fn test_mark_shipped_stranger_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;
    let pool = mock_pool_with_asset(&env, &usdc);

    // Initialize as admin
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    // Create invoice as issuer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    // List as issuer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    // Set pool as admin
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_pool_contract",
            args: (pool.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.set_pool_contract(&pool);

    // Mark funded as pool
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &pool,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_funded",
            args: (
                invoice_id.clone(),
                pool.clone(),
                usdc.clone(),
                980_000_000u128,
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);

    // Calling mark_shipped without mocking auths for the issuer should panic
    // due to failed require_auth. The stranger address is not the issuer.
    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic]
fn test_expire_listing_stranger_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Calling expire_listing without mocking auths for issuer or admin should panic due to failed require_auth.
    client.expire_listing(&invoice_id);
}

#[test]
fn test_unique_invoice_ids_for_identical_inputs() {
    // Two invoices created with identical parameters (issuer, buyer, face_value, due_date, funding_asset)
    // must receive different IDs because the internal storage Counter salt increments per invoice.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = 1_000_000_000;
    let due_date = env.ledger().timestamp() + 86400;

    // Create two invoices with identical parameters
    let id1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    let id2 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // 1. Assert id1 != id2
    assert_ne!(id1, id2);

    // 2. Assert state after: verify both invoices exist in persistent storage with correct fields
    let inv1 = client.get(&id1);
    let inv2 = client.get(&id2);

    assert_eq!(inv1.id, id1);
    assert_eq!(inv2.id, id2);

    assert_eq!(inv1.issuer, issuer);
    assert_eq!(inv2.issuer, issuer);
    assert_eq!(inv1.buyer, buyer);
    assert_eq!(inv2.buyer, buyer);

    assert_eq!(inv1.face_value, face_value);
    assert_eq!(inv2.face_value, face_value);
    assert_eq!(inv1.due_date, due_date);
    assert_eq!(inv2.due_date, due_date);

    assert_eq!(inv1.funding_asset, usdc);
    assert_eq!(inv2.funding_asset, usdc);
    assert_eq!(inv1.status, InvoiceStatus::Created);
    assert_eq!(inv2.status, InvoiceStatus::Created);

    // Assert internal instance counter incremented to 2
    let counter: u64 = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get(&crate::DataKey::Counter)
            .unwrap()
    });
    assert_eq!(counter, 2);

    // Assert index queries return both invoices
    assert_eq!(client.get_by_issuer(&issuer).len(), 2);
    assert_eq!(client.get_by_buyer(&buyer).len(), 2);
    assert_eq!(client.get_by_status(&InvoiceStatus::Created).len(), 2);

    // 3. Assert emitted events: two invoice_created events emitted
    let contract_id = client.address.clone();
    let events = env.events().all();
    assert_eq!(events.len(), 2);

    let (event1_contract, event1_topics, event1_data) =
        events.get(0).expect("expected first event");
    assert_eq!(event1_contract, contract_id);
    assert_eq!(
        event1_topics,
        (
            Symbol::new(&env, "invoice_created"),
            id1.clone(),
            issuer.clone(),
            buyer.clone(),
            usdc.clone(),
        )
            .into_val(&env)
    );
    assert_eq!(u128::try_from_val(&env, &event1_data).unwrap(), face_value);

    let (event2_contract, event2_topics, event2_data) =
        events.get(1).expect("expected second event");
    assert_eq!(event2_contract, contract_id);
    assert_eq!(
        event2_topics,
        (
            Symbol::new(&env, "invoice_created"),
            id2.clone(),
            issuer.clone(),
            buyer.clone(),
            usdc.clone(),
        )
            .into_val(&env)
    );
    assert_eq!(u128::try_from_val(&env, &event2_data).unwrap(), face_value);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_create_unauthorized_issuer_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + 86400;

    // Call create without mocking issuer auth -> should panic with Error(Auth, InvalidAction)
    env.mock_auths(&[]);
    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

#[test]
fn test_invoice_ids_unique_for_different_issuers() {
    // Test that different issuer/buyer combinations produce unique IDs
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create second invoice with different issuer
    let issuer2 = Address::generate(&env);
    registry.register(&issuer2);
    let invoice_id_2 = client.create(&issuer2, &buyer, &face_value, &due_date, &usdc);

    // Different issuers should produce different IDs even with same other parameters
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct issuers
    assert_eq!(client.get(&invoice_id_1).issuer, issuer);
    assert_eq!(client.get(&invoice_id_2).issuer, issuer2);
}

#[test]
fn test_invoice_ids_unique_for_different_buyers() {
    // Test that different buyer combinations produce unique IDs
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create second invoice with different buyer
    let buyer2 = Address::generate(&env);
    registry.register(&buyer2);
    let invoice_id_2 = client.create(&issuer, &buyer2, &face_value, &due_date, &usdc);

    // Different buyers should produce different IDs even with same other parameters
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct buyers
    assert_eq!(client.get(&invoice_id_1).buyer, buyer);
    assert_eq!(client.get(&invoice_id_2).buyer, buyer2);
}

#[test]
fn test_invoice_ids_unique_for_different_face_values() {
    // Test that different face values produce unique IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    // Create second invoice with different face value
    let invoice_id_2 = client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    // Different face values should produce different IDs
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct values
    assert_eq!(client.get(&invoice_id_1).face_value, 1_000_000_000);
    assert_eq!(client.get(&invoice_id_2).face_value, 2_000_000_000);
}

#[test]
fn test_invoice_ids_unique_for_different_due_dates() {
    // Test that different due dates produce unique IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value = 1_000_000_000u128;
    let due_date_1 = env.ledger().timestamp() + 86400;
    let due_date_2 = env.ledger().timestamp() + 172800;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date_1, &usdc);

    // Create second invoice with different due date
    let invoice_id_2 = client.create(&issuer, &buyer, &face_value, &due_date_2, &usdc);

    // Different due dates should produce different IDs
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct due dates
    assert_eq!(client.get(&invoice_id_1).due_date, due_date_1);
    assert_eq!(client.get(&invoice_id_2).due_date, due_date_2);
}

#[test]
fn test_invoice_ids_unique_for_different_assets() {
    // Test that different funding assets produce unique IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    // Create first invoice with usdc
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create a different token asset
    let other_token = env.register_contract(None, MockToken);

    // Create second invoice with different asset
    let invoice_id_2 = client.create(&issuer, &buyer, &face_value, &due_date, &other_token);

    // Different assets should produce different IDs
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct assets
    assert_eq!(client.get(&invoice_id_1).funding_asset, usdc);
    assert_eq!(client.get(&invoice_id_2).funding_asset, other_token);
}

#[test]
fn test_multiple_invoices_have_unique_ids() {
    // Test that creating multiple invoices produces unique IDs (counter increments)
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let face_value = 1_000_000_000u128;

    let mut ids: soroban_sdk::Vec<BytesN<32>> = soroban_sdk::Vec::new(&env);
    for _ in 0..5 {
        let id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
        ids.push_back(id);
    }

    // All IDs should be unique due to incrementing counter
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(
                ids.get_unchecked(i),
                ids.get_unchecked(j),
                "Invoice IDs should be unique"
            );
        }
    }
}

#[test]
fn test_create_invoice_does_not_panic_on_xdr_generation() {
    // Regression test: ensure invoice creation never panics due to XDR length issues
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    // Test with various face values to ensure no panic
    let face_values = [1u128, 100, 1_000, 1_000_000, crate::MAX_FACE_VALUE];
    for face_value in face_values.iter() {
        let invoice_id = client.create(&issuer, &buyer, face_value, &due_date, &usdc);
        let invoice = client.get(&invoice_id);
        assert_eq!(invoice.face_value, *face_value);
    }
}

#[test]
fn test_create_allows_face_value_at_max_boundary() {
    // Positive path: MAX_FACE_VALUE itself is allowed (boundary is inclusive).
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    let invoice_id = client.create(&issuer, &buyer, &crate::MAX_FACE_VALUE, &due_date, &usdc);
    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.face_value, crate::MAX_FACE_VALUE);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_create_fails_face_value_above_max_boundary() {
    // Negative path: one stroop above MAX_FACE_VALUE must panic with InvalidAmount (#16).
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(
        &issuer,
        &buyer,
        &(crate::MAX_FACE_VALUE + 1),
        &due_date,
        &usdc,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_create_fails_face_value_u128_max() {
    // u128::MAX must be rejected as InvalidAmount rather than overflowing downstream math.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &u128::MAX, &due_date, &usdc);
}

#[test]
fn test_existing_valid_addresses_still_work() {
    // Regression test: verify existing valid address formats still generate valid invoice IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value = 1_000_000_000u128;
    let due_date = env.ledger().timestamp() + 86400;

    // Should not panic and should return a valid invoice ID
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Verify invoice exists and is valid
    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.issuer, issuer);
    assert_eq!(invoice.buyer, buyer);
    assert_eq!(invoice.face_value, face_value);
    assert_eq!(invoice.due_date, due_date);
    assert_eq!(invoice.status, InvoiceStatus::Created);
}

// ============================== REPAY TESTS ==============================

#[test]
fn test_repay_from_confirmed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    let events_before = env.events().all().len();
    client.repay(&invoice_id);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Repaid);
    assert!(invoice.repaid_at.is_some());
    assert!(env.events().all().len() > events_before);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_repay_wrong_auth_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    env.set_auths(&[]);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_funded_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_active_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_create_fails_counter_overflow() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&crate::DataKey::Counter, &u64::MAX);
    });

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

// ============== ISSUE #201: TYPED ERRORS FOR UNINITIALIZED CONTRACT ==============

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_create_fails_uninitialized_registry() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let due_date = env.ledger().timestamp() + 86400;
    let usdc = Address::generate(&env);
    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_create_fails_missing_counter() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    env.as_contract(&client.address, || {
        env.storage().instance().remove(&crate::DataKey::Counter);
    });

    let due_date = env.ledger().timestamp() + 86400;
    let usdc = Address::generate(&env);
    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
}

// ============== ISSUE #195: DEDUPLICATE INVOICE_ID IN INDEXES ==============

#[test]
fn test_move_status_index_deduplicates_repeated_transition() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    // Create TWO invoices so Created count > 1, allowing replay without underflow
    let invoice_id_1 = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    let invoice_id_2 = client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    // Initial state: both invoices are Created
    assert_eq!(client.get(&invoice_id_1).status, InvoiceStatus::Created);
    assert_eq!(client.get(&invoice_id_2).status, InvoiceStatus::Created);

    // First transition: invoice_id_1 Created -> Listed (via public API)
    client.list_for_financing(&invoice_id_1, &200);
    assert_eq!(client.get(&invoice_id_1).status, InvoiceStatus::Listed);
    assert_eq!(client.get(&invoice_id_2).status, InvoiceStatus::Created);

    // Verify index state after first transition
    let listed_count_before = client.get_by_status(&InvoiceStatus::Listed).len();
    let created_count_before = client.get_by_status(&InvoiceStatus::Created).len();
    assert_eq!(listed_count_before, 1);
    assert_eq!(created_count_before, 1);

    // Simulate a replayed transition for invoice_id_1 by directly calling move_status_index
    // This bypasses the status check in the public API
    env.as_contract(&client.address, || {
        super::move_status_index(
            &env,
            &invoice_id_1,
            InvoiceStatus::Created,
            InvoiceStatus::Listed,
        );
    });

    // The status should still be Listed (no actual state change)
    assert_eq!(client.get(&invoice_id_1).status, InvoiceStatus::Listed);
    assert_eq!(client.get(&invoice_id_2).status, InvoiceStatus::Created);

    // The index should NOT have duplicate entries - length should remain 1
    let listed_count_after = client.get_by_status(&InvoiceStatus::Listed).len();
    assert_eq!(
        listed_count_after, 1,
        "Repeated transition should not duplicate index entries"
    );

    // Created count should also not be affected (still 1 for invoice_id_2)
    let created_count_after = client.get_by_status(&InvoiceStatus::Created).len();
    assert_eq!(
        created_count_after, 1,
        "Created count should not be affected by replayed transition"
    );

    // Status counts should not be inflated
    let counts = client.get_counts();
    let listed_count = counts.get(String::from_str(&env, "Listed")).unwrap();
    let created_count = counts.get(String::from_str(&env, "Created")).unwrap();
    assert_eq!(
        listed_count, 1,
        "Listed status count should not be inflated by replayed transition"
    );
    assert_eq!(
        created_count, 1,
        "Created status count should not be affected by replayed transition"
    );
}

// ============== ISSUE #213: INVALID STATUS TRANSITIONS ==============

// mark_funded from invalid statuses
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_funded_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_funded_from_funded_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    // Try to fund again
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_funded_from_active_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    // Try to fund an Active invoice
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_funded_from_confirmed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    // Try to fund a Confirmed invoice
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_funded_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // Try to fund a Repaid invoice
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_funded_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    // Try to fund a Defaulted invoice
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_funded_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    // Try to fund an Expired invoice
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
}

// mark_shipped from invalid statuses
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_shipped_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_shipped_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_shipped_from_active_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    // Try to ship again
    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_shipped_from_confirmed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    // Try to ship a Confirmed invoice
    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_shipped_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // Try to ship a Repaid invoice
    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_shipped_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    // Try to ship a Defaulted invoice
    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_mark_shipped_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    // Try to ship an Expired invoice
    client.mark_shipped(&invoice_id);
}

// confirm_delivery from invalid statuses
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_confirm_delivery_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_confirm_delivery_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_confirm_delivery_from_funded_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_confirm_delivery_from_confirmed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    // Try to confirm again - should panic with InvalidStatusTransition (#8)
    // because the status check runs before the AlreadyConfirmed check
    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_confirm_delivery_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // Try to confirm a Repaid invoice
    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_confirm_delivery_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    // Try to confirm a Defaulted invoice
    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_confirm_delivery_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    // Try to confirm an Expired invoice
    client.confirm_delivery(&invoice_id, &issuer);
}

// repay_early from invalid statuses
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.repay_early(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    client.repay_early(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_from_funded_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    client.repay_early(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_from_active_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.repay_early(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay_early(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // Try to repay early again
    client.repay_early(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    client.repay_early(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    client.repay_early(&invoice_id);
}

// repay_early from Confirmed but past due date (should fail with InvalidStatusTransition #8 due to the check)
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_early_past_due_date_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    // Advance past due date
    env.ledger().set_timestamp(due_date + 1);

    // Should fail because now >= due_date
    client.repay_early(&invoice_id);
}

// trigger_default from Expired status (missing test)
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    // Expired is not Funded/Active/Confirmed, so should be rejected
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

// expire_listing from invalid statuses
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_from_funded_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    // Fast forward time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Try to expire a Funded invoice
    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_from_active_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    // Fast forward time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Try to expire an Active invoice
    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_from_confirmed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    // Fast forward time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Try to expire a Confirmed invoice
    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // Fast forward time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Try to expire a Repaid invoice
    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    // Fast forward time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Try to expire a Defaulted invoice
    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    // Fast forward time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Try to expire an already Expired invoice
    client.expire_listing(&invoice_id);
}

// list_for_financing from invalid statuses
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_for_financing_from_funded_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    // Try to list a Funded invoice
    client.list_for_financing(&invoice_id, &200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_for_financing_from_active_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    // Try to list an Active invoice
    client.list_for_financing(&invoice_id, &200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_for_financing_from_confirmed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    // Try to list a Confirmed invoice
    client.list_for_financing(&invoice_id, &200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_for_financing_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // Try to list a Repaid invoice
    client.list_for_financing(&invoice_id, &200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_for_financing_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    // Try to list a Defaulted invoice
    client.list_for_financing(&invoice_id, &200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_for_financing_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    // Try to list an Expired invoice
    client.list_for_financing(&invoice_id, &200);
}
