## Summary

Implements the missing `contract_initialized` event emission from the `initialize()` function as requested in issue #204.

## Changes

### contracts/invoice/src/events.rs
- Added `contract_initialized(env: &Env, admin: &Address, registry_contract: &Address)` event function that emits a `contract_initialized` event with admin and registry contract addresses as topics.

### contracts/invoice/src/lib.rs
- Modified `initialize()` function to emit `events::contract_initialized(&env, &admin, &registry_contract)` after successful initialization.

### contracts/invoice/src/test.rs
- Added `test_initialize_emits_contract_initialized_event`: Happy-path test verifying the event is emitted with correct topics (admin, registry_contract) and empty data.
- Added `test_initialize_fails_when_already_initialized`: Negative test verifying re-initialization panics with `AlreadyInitialized` error.
- Updated `test_create_succeeds_when_due_date_one_second_in_future` to expect 2 events (contract_initialized + invoice_created).
- Updated `test_set_pool_contract_emits_event` and `test_set_expiry_window_emits_event` to check only the last event since setup() now emits contract_initialized.

## Testing

All checks pass:
- ✅ `cargo test -p trusttrove-invoice` (75 tests pass)
- ✅ `cargo fmt --all --check`
- ✅ `cargo clippy --all-targets -- -D warnings`
- ✅ No behaviour regression in existing tests

Closes #204