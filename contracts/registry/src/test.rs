#![cfg(test)]

extern crate alloc;

use crate::{RegistryContract, RegistryContractClient};
use alloc::format;
use soroban_sdk::{map, testutils::Address as _, Address, Env, String};

fn setup() -> (Env, RegistryContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);
    (env, client)
}

#[test]
fn test_initialize() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_register_issuer() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Acme Corp")
        )
    ];
    let result = client.register_issuer(&issuer, &metadata);
    assert!(result);
    let profile = client.get_profile(&issuer);
    assert_eq!(profile.role, crate::Role::Issuer);
    assert!(profile.verified);
}

#[test]
fn test_register_buyer() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    let metadata = map![&env];
    let result = client.register_buyer(&buyer, &metadata);
    assert!(result);
    let profile = client.get_profile(&buyer);
    assert_eq!(profile.role, crate::Role::Buyer);
    assert!(profile.verified);
}

#[test]
fn test_is_verified_returns_true_for_registered() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(client.is_verified(&issuer));
}

#[test]
fn test_is_verified_returns_false_for_unknown() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    assert!(!client.is_verified(&unknown));
}

#[test]
fn test_revoke_sets_verified_false() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(client.is_verified(&issuer));
    let result = client.revoke(&issuer);
    assert!(result);
    assert!(!client.is_verified(&issuer));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_duplicate_registration_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.register_issuer(&issuer, &map![&env]);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_double_initialize_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    client.initialize(&admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_get_profile_unknown_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    client.get_profile(&unknown);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_register_issuer_rejects_excess_metadata_entries() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let mut metadata = map![&env];
    for i in 0..=crate::MAX_METADATA_ENTRIES {
        metadata.set(
            String::from_str(&env, &format!("key_{i:02}")),
            String::from_str(&env, "value"),
        );
    }
    client.register_issuer(&issuer, &metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_register_buyer_rejects_oversized_metadata_key() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    let long_key = "k".repeat((crate::MAX_METADATA_KEY_LEN + 1) as usize);
    let metadata = map![
        &env,
        (
            String::from_str(&env, &long_key),
            String::from_str(&env, "value")
        )
    ];
    client.register_buyer(&buyer, &metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_register_issuer_rejects_oversized_metadata_value() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let long_value = "v".repeat((crate::MAX_METADATA_VALUE_LEN + 1) as usize);
    let metadata = map![
        &env,
        (
            String::from_str(&env, "key"),
            String::from_str(&env, &long_value)
        )
    ];
    client.register_issuer(&issuer, &metadata);
}

#[test]
fn test_register_buyer_accepts_metadata_at_limits() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    let value = "v".repeat(crate::MAX_METADATA_VALUE_LEN as usize);
    let mut metadata = map![&env];
    for i in 0..crate::MAX_METADATA_ENTRIES {
        metadata.set(
            String::from_str(&env, &format!("key_{i:02}")),
            String::from_str(&env, &value),
        );
    }
    assert!(client.register_buyer(&buyer, &metadata));
}
