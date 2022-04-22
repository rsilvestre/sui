// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
use std::collections::BTreeMap;

use move_core_types::{account_address::AccountAddress, ident_str};
use signature::Signer;

use sui_adapter::genesis;
use sui_types::crypto::get_key_pair;
use sui_types::crypto::Signature;

use sui_types::messages::Transaction;
use sui_types::object::{Object, GAS_VALUE_FOR_TESTING};

use super::*;
use crate::authority_client::LocalAuthorityClient;

pub fn authority_genesis_objects(
    authority_count: usize,
    objects_per_authority: Vec<Object>,
) -> Vec<Vec<Object>> {
    let mut objects = vec![];
    for _ in 0..authority_count {
        objects.push(objects_per_authority.clone());
    }
    objects
}

pub async fn init_local_authorities(
    genesis_objects: Vec<Vec<Object>>,
) -> AuthorityAggregator<LocalAuthorityClient> {
    let mut key_pairs = Vec::new();
    let mut voting_rights = BTreeMap::new();
    for _ in 0..genesis_objects.len() {
        let (_, key_pair) = get_key_pair();
        let authority_name = *key_pair.public_key_bytes();
        voting_rights.insert(authority_name, 1);
        key_pairs.push((authority_name, key_pair));
    }
    let committee = Committee::new(0, voting_rights);

    let mut clients = BTreeMap::new();
    for ((authority_name, secret), objects) in key_pairs.into_iter().zip(genesis_objects) {
        let client = LocalAuthorityClient::new_with_objects(
            committee.clone(),
            authority_name,
            secret,
            objects,
        )
        .await;
        clients.insert(authority_name, client);
    }
    AuthorityAggregator::new(committee, clients)
}

fn transfer_object_move_transaction(
    src: SuiAddress,
    secret: &dyn signature::Signer<Signature>,
    dest: SuiAddress,
    object_ref: ObjectRef,
    framework_obj_ref: ObjectRef,
    gas_object_ref: ObjectRef,
) -> Transaction {
    let args = vec![
        CallArg::ImmOrOwnedObject(object_ref),
        CallArg::Pure(bcs::to_bytes(&AccountAddress::from(dest)).unwrap()),
    ];

    to_transaction(
        TransactionData::new_move_call(
            src,
            framework_obj_ref,
            ident_str!("ObjectBasics").to_owned(),
            ident_str!("transfer").to_owned(),
            Vec::new(),
            gas_object_ref,
            args,
            GAS_VALUE_FOR_TESTING / 2,
        ),
        secret,
    )
}

pub fn crate_object_move_transaction(
    src: SuiAddress,
    secret: &dyn signature::Signer<Signature>,
    dest: SuiAddress,
    value: u64,
    framework_obj_ref: ObjectRef,
    gas_object_ref: ObjectRef,
) -> Transaction {
    // When creating an ObjectBasics object, we provide the value (u64) and address which will own the object
    let arguments = vec![
        CallArg::Pure(value.to_le_bytes().to_vec()),
        CallArg::Pure(bcs::to_bytes(&AccountAddress::from(dest)).unwrap()),
    ];

    to_transaction(
        TransactionData::new_move_call(
            src,
            framework_obj_ref,
            ident_str!("ObjectBasics").to_owned(),
            ident_str!("create").to_owned(),
            Vec::new(),
            gas_object_ref,
            arguments,
            GAS_VALUE_FOR_TESTING / 2,
        ),
        &*secret,
    )
}

fn delete_object_move_transaction(
    src: SuiAddress,
    secret: &dyn signature::Signer<Signature>,
    object_ref: ObjectRef,
    framework_obj_ref: ObjectRef,
    gas_object_ref: ObjectRef,
) -> Transaction {
    to_transaction(
        TransactionData::new_move_call(
            src,
            framework_obj_ref,
            ident_str!("ObjectBasics").to_owned(),
            ident_str!("delete").to_owned(),
            Vec::new(),
            gas_object_ref,
            vec![CallArg::ImmOrOwnedObject(object_ref)],
            GAS_VALUE_FOR_TESTING / 2,
        ),
        secret,
    )
}

fn set_object_move_transaction(
    src: SuiAddress,
    secret: &dyn signature::Signer<Signature>,
    object_ref: ObjectRef,
    value: u64,
    framework_obj_ref: ObjectRef,
    gas_object_ref: ObjectRef,
) -> Transaction {
    let args = vec![
        CallArg::ImmOrOwnedObject(object_ref),
        CallArg::Pure(bcs::to_bytes(&value).unwrap()),
    ];

    to_transaction(
        TransactionData::new_move_call(
            src,
            framework_obj_ref,
            ident_str!("ObjectBasics").to_owned(),
            ident_str!("set_value").to_owned(),
            Vec::new(),
            gas_object_ref,
            args,
            GAS_VALUE_FOR_TESTING / 2,
        ),
        secret,
    )
}

fn to_transaction(data: TransactionData, signer: &dyn Signer<Signature>) -> Transaction {
    let signature = Signature::new(&data, signer);
    Transaction::new(data, signature)
}

async fn do_transaction<A: AuthorityAPI>(authority: &A, transaction: &Transaction) {
    authority
        .handle_transaction(transaction.clone())
        .await
        .unwrap();
}

async fn extract_cert<A: AuthorityAPI>(
    authorities: &[&A],
    committee: &Committee,
    transaction_digest: TransactionDigest,
) -> CertifiedTransaction {
    let mut votes = vec![];
    let mut transaction: Option<SignedTransaction> = None;
    for authority in authorities {
        if let Ok(TransactionInfoResponse {
            signed_transaction: Some(signed),
            ..
        }) = authority
            .handle_transaction_info_request(TransactionInfoRequest::from(transaction_digest))
            .await
        {
            votes.push((
                signed.auth_signature.authority,
                signed.auth_signature.signature,
            ));
            if let Some(inner_transaction) = transaction {
                assert!(inner_transaction.data == signed.data);
            }
            transaction = Some(signed);
        }
    }

    let stake: usize = votes.iter().map(|(name, _)| committee.weight(name)).sum();
    assert!(stake >= committee.quorum_threshold());

    CertifiedTransaction::new_with_signatures(
        committee.epoch(),
        transaction.unwrap().to_transaction(),
        votes,
    )
}

async fn do_cert<A: AuthorityAPI>(
    authority: &A,
    cert: &CertifiedTransaction,
) -> TransactionEffects {
    authority
        .handle_confirmation_transaction(ConfirmationTransaction::new(cert.clone()))
        .await
        .unwrap()
        .signed_effects
        .unwrap()
        .effects
}

async fn get_latest_ref<A: AuthorityAPI>(authority: &A, object_id: ObjectID) -> ObjectRef {
    if let Ok(ObjectInfoResponse {
        requested_object_reference: Some(object_ref),
        ..
    }) = authority
        .handle_object_info_request(ObjectInfoRequest::latest_object_info_request(
            object_id, None,
        ))
        .await
    {
        return object_ref;
    }
    panic!("Object not found!");
}

#[tokio::test]
async fn test_map_reducer() {
    let authorities = init_local_authorities(authority_genesis_objects(4, vec![])).await;

    // Test: reducer errors get propagated up
    let res = authorities
        .quorum_map_then_reduce_with_timeout(
            0usize,
            |_name, _client| Box::pin(async move { Ok(()) }),
            |_accumulated_state, _authority_name, _authority_weight, _result| {
                Box::pin(
                    async move { Err(SuiError::TooManyIncorrectAuthorities { errors: vec![] }) },
                )
            },
            Duration::from_millis(1000),
        )
        .await;
    assert!(matches!(
        res,
        Err(SuiError::TooManyIncorrectAuthorities { .. })
    ));

    // Test: mapper errors do not get propagated up, reducer works
    let res = authorities
        .quorum_map_then_reduce_with_timeout(
            0usize,
            |_name, _client| {
                Box::pin(async move {
                    let res: Result<usize, SuiError> =
                        Err(SuiError::TooManyIncorrectAuthorities { errors: vec![] });
                    res
                })
            },
            |mut accumulated_state, _authority_name, _authority_weight, result| {
                Box::pin(async move {
                    assert!(matches!(
                        result,
                        Err(SuiError::TooManyIncorrectAuthorities { .. })
                    ));
                    accumulated_state += 1;
                    Ok(ReduceOutput::Continue(accumulated_state))
                })
            },
            Duration::from_millis(1000),
        )
        .await;
    assert_eq!(Ok(4), res);

    // Test: early end
    let res = authorities
        .quorum_map_then_reduce_with_timeout(
            0usize,
            |_name, _client| Box::pin(async move { Ok(()) }),
            |mut accumulated_state, _authority_name, _authority_weight, _result| {
                Box::pin(async move {
                    if accumulated_state > 2 {
                        Ok(ReduceOutput::End(accumulated_state))
                    } else {
                        accumulated_state += 1;
                        Ok(ReduceOutput::Continue(accumulated_state))
                    }
                })
            },
            Duration::from_millis(1000),
        )
        .await;
    assert_eq!(Ok(3), res);

    // Test: Global timeout works
    let res = authorities
        .quorum_map_then_reduce_with_timeout(
            0usize,
            |_name, _client| {
                Box::pin(async move {
                    // 10 mins
                    tokio::time::sleep(Duration::from_secs(10 * 60)).await;
                    Ok(())
                })
            },
            |_accumulated_state, _authority_name, _authority_weight, _result| {
                Box::pin(
                    async move { Err(SuiError::TooManyIncorrectAuthorities { errors: vec![] }) },
                )
            },
            Duration::from_millis(10),
        )
        .await;
    assert_eq!(Ok(0), res);

    // Test: Local timeout works
    let bad_auth = *authorities.committee.sample();
    let res = authorities
        .quorum_map_then_reduce_with_timeout(
            HashSet::new(),
            |_name, _client| {
                Box::pin(async move {
                    // 10 mins
                    if _name == bad_auth {
                        tokio::time::sleep(Duration::from_secs(10 * 60)).await;
                    }
                    Ok(())
                })
            },
            |mut accumulated_state, authority_name, _authority_weight, _result| {
                Box::pin(async move {
                    accumulated_state.insert(authority_name);
                    if accumulated_state.len() <= 3 {
                        Ok(ReduceOutput::Continue(accumulated_state))
                    } else {
                        Ok(ReduceOutput::ContinueWithTimeout(
                            accumulated_state,
                            Duration::from_millis(10),
                        ))
                    }
                })
            },
            // large delay
            Duration::from_millis(10 * 60),
        )
        .await;
    assert_eq!(res.as_ref().unwrap().len(), 3);
    assert!(!res.as_ref().unwrap().contains(&bad_auth));
}

#[tokio::test]
async fn test_get_all_owned_objects() {
    let (addr1, key1) = get_key_pair();
    let (addr2, _) = get_key_pair();
    let gas_object1 = Object::with_owner_for_testing(addr1);
    let gas_ref_1 = gas_object1.compute_object_reference();
    let gas_object2 = Object::with_owner_for_testing(addr2);
    let genesis_objects =
        authority_genesis_objects(4, vec![gas_object1.clone(), gas_object2.clone()]);
    let authorities = init_local_authorities(genesis_objects).await;
    let authority_clients: Vec<_> = authorities.authority_clients.values().collect();

    // Make a schedule of transactions
    let framework_obj_ref = genesis::get_framework_object_ref();
    let create1 =
        crate_object_move_transaction(addr1, &key1, addr1, 100, framework_obj_ref, gas_ref_1);

    // Submit to 3 authorities, but not 4th
    do_transaction(authority_clients[0], &create1).await;
    do_transaction(authority_clients[1], &create1).await;
    do_transaction(authority_clients[2], &create1).await;

    // Get a cert
    let cert1 = extract_cert(&authority_clients, &authorities.committee, create1.digest()).await;

    // Test 1: Before the cert is submitted no one knows of the new object.
    let (owned_object, _) = authorities
        .get_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(1, owned_object.len());
    assert!(owned_object.contains_key(&gas_ref_1));

    // Submit the cert to first authority.
    let effects = do_cert(authority_clients[0], &cert1).await;

    // Test 2: Once the cert is submitted one auth returns the new object,
    //         but now two versions of gas exist.
    let (owned_object, _) = authorities
        .get_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(3, owned_object.len());

    assert!(owned_object.contains_key(&effects.gas_object.0));
    assert!(owned_object.contains_key(&effects.created[0].0));
    let created_ref = effects.created[0].0;

    // Submit to next 3 authorities.
    do_cert(authority_clients[1], &cert1).await;
    do_cert(authority_clients[2], &cert1).await;
    do_cert(authority_clients[3], &cert1).await;

    // Make a delete transaction
    let gas_ref_del = get_latest_ref(authority_clients[0], gas_object1.id()).await;
    let delete1 =
        delete_object_move_transaction(addr1, &key1, created_ref, framework_obj_ref, gas_ref_del);

    // Get cert for delete transaction, and submit to first authority
    do_transaction(authority_clients[0], &delete1).await;
    do_transaction(authority_clients[1], &delete1).await;
    do_transaction(authority_clients[2], &delete1).await;
    let cert2 = extract_cert(&authority_clients, &authorities.committee, delete1.digest()).await;
    let _effects = do_cert(authority_clients[0], &cert2).await;

    // Test 3: dealing with deleted objects on some authorities
    let (owned_object, _) = authorities
        .get_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();
    // Since not all authorities know the object is deleted, we get back
    // the new gas object, the delete object and the old gas object.
    assert_eq!(3, owned_object.len());

    // Update rest of authorities
    do_cert(authority_clients[1], &cert2).await;
    do_cert(authority_clients[2], &cert2).await;
    do_cert(authority_clients[3], &cert2).await;

    // Test 4: dealing with deleted objects on all authorities
    let (owned_object, _) = authorities
        .get_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();

    // Just the gas object is returned
    assert_eq!(1, owned_object.len());
}

#[tokio::test]
async fn test_sync_all_owned_objects() {
    let (addr1, key1) = get_key_pair();
    let (addr2, _) = get_key_pair();
    let gas_object1 = Object::with_owner_for_testing(addr1);
    let gas_object2 = Object::with_owner_for_testing(addr1);
    let genesis_objects =
        authority_genesis_objects(4, vec![gas_object1.clone(), gas_object2.clone()]);
    let authorities = init_local_authorities(genesis_objects).await;
    let authority_clients: Vec<_> = authorities.authority_clients.values().collect();

    let framework_obj_ref = genesis::get_framework_object_ref();
    // Make a schedule of transactions
    let gas_ref_1 = get_latest_ref(authority_clients[0], gas_object1.id()).await;
    let create1 =
        crate_object_move_transaction(addr1, &key1, addr1, 100, framework_obj_ref, gas_ref_1);

    let gas_ref_2 = get_latest_ref(authority_clients[0], gas_object2.id()).await;
    let create2 =
        crate_object_move_transaction(addr1, &key1, addr1, 101, framework_obj_ref, gas_ref_2);

    // Submit to 3 authorities, but not 4th
    do_transaction(authority_clients[0], &create1).await;
    do_transaction(authority_clients[1], &create1).await;
    do_transaction(authority_clients[2], &create1).await;

    do_transaction(authority_clients[1], &create2).await;
    do_transaction(authority_clients[2], &create2).await;
    do_transaction(authority_clients[3], &create2).await;

    // Get a cert
    let cert1 = extract_cert(&authority_clients, &authorities.committee, create1.digest()).await;
    let cert2 = extract_cert(&authority_clients, &authorities.committee, create2.digest()).await;

    // Submit the cert to 1 authority.
    let new_ref_1 = do_cert(authority_clients[0], &cert1).await.created[0].0;
    let new_ref_2 = do_cert(authority_clients[3], &cert2).await.created[0].0;

    // Test 1: Once the cert is submitted one auth returns the new object,
    //         but now two versions of gas exist. Ie total 2x3 = 6.
    let (owned_object, _) = authorities
        .get_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(6, owned_object.len());

    // After sync we are back to having 4.
    let (owned_object, _) = authorities
        .sync_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(4, owned_object.len());

    // Now lets delete and move objects

    // Make a delete transaction
    let gas_ref_del = get_latest_ref(authority_clients[0], gas_object1.id()).await;
    let delete1 =
        delete_object_move_transaction(addr1, &key1, new_ref_1, framework_obj_ref, gas_ref_del);

    // Make a transfer transaction
    let gas_ref_trans = get_latest_ref(authority_clients[0], gas_object2.id()).await;
    let transfer1 = transfer_object_move_transaction(
        addr1,
        &key1,
        addr2,
        new_ref_2,
        framework_obj_ref,
        gas_ref_trans,
    );

    do_transaction(authority_clients[0], &delete1).await;
    do_transaction(authority_clients[1], &delete1).await;
    do_transaction(authority_clients[2], &delete1).await;

    do_transaction(authority_clients[1], &transfer1).await;
    do_transaction(authority_clients[2], &transfer1).await;
    do_transaction(authority_clients[3], &transfer1).await;

    let cert1 = extract_cert(&authority_clients, &authorities.committee, delete1.digest()).await;
    let cert2 = extract_cert(
        &authority_clients,
        &authorities.committee,
        transfer1.digest(),
    )
    .await;

    do_cert(authority_clients[0], &cert1).await;
    do_cert(authority_clients[3], &cert2).await;

    // Test 2: Before we sync we see 6 object, incl: (old + new gas) x 2, and 2 x old objects
    // after we see just 2 (one deleted one transferred.)
    let (owned_object, _) = authorities
        .get_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(6, owned_object.len());

    // After sync we are back to having 2.
    let (owned_object, _) = authorities
        .sync_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        2,
        owned_object
            .iter()
            .filter(|(o, _, _)| o.owner == addr1)
            .count()
    );
}

#[tokio::test]
async fn test_process_transaction() {
    let (addr1, key1) = get_key_pair();
    let gas_object1 = Object::with_owner_for_testing(addr1);
    let gas_object2 = Object::with_owner_for_testing(addr1);
    let genesis_objects =
        authority_genesis_objects(4, vec![gas_object1.clone(), gas_object2.clone()]);
    let authorities = init_local_authorities(genesis_objects).await;
    let authority_clients: Vec<_> = authorities.authority_clients.values().collect();

    let framework_obj_ref = genesis::get_framework_object_ref();

    // Make a schedule of transactions
    let gas_ref_1 = get_latest_ref(authority_clients[0], gas_object1.id()).await;
    let create1 =
        crate_object_move_transaction(addr1, &key1, addr1, 100, framework_obj_ref, gas_ref_1);

    do_transaction(authority_clients[0], &create1).await;
    do_transaction(authority_clients[1], &create1).await;
    do_transaction(authority_clients[2], &create1).await;

    // Get a cert
    let cert1 = extract_cert(&authority_clients, &authorities.committee, create1.digest()).await;

    // Submit the cert to 1 authority.
    let new_ref_1 = do_cert(authority_clients[0], &cert1).await.created[0].0;

    // Make a schedule of transactions
    let gas_ref_set = get_latest_ref(authority_clients[0], gas_object1.id()).await;
    let create2 =
        set_object_move_transaction(addr1, &key1, new_ref_1, 100, framework_obj_ref, gas_ref_set);

    // Test 1: When we call process transaction on the second transaction, the process_transaction
    // updates all authorities with latest objects, and then the transaction goes through
    // on all of them. Note that one authority has processed cert 1, and none cert2,
    // and auth 3 has no seen either.
    authorities
        .process_transaction(create2.clone(), Duration::from_secs(10))
        .await
        .unwrap();

    // The transaction still only has 3 votes, as only these are needed.
    let cert2 = extract_cert(&authority_clients, &authorities.committee, create2.digest()).await;
    assert_eq!(3, cert2.signatures.len());
}

#[tokio::test]
async fn test_process_certificate() {
    let (addr1, key1) = get_key_pair();
    let gas_object1 = Object::with_owner_for_testing(addr1);
    let gas_object2 = Object::with_owner_for_testing(addr1);
    let genesis_objects =
        authority_genesis_objects(4, vec![gas_object1.clone(), gas_object2.clone()]);
    let authorities = init_local_authorities(genesis_objects).await;
    let authority_clients: Vec<_> = authorities.authority_clients.values().collect();

    let framework_obj_ref = genesis::get_framework_object_ref();

    // Make a schedule of transactions
    let gas_ref_1 = get_latest_ref(authority_clients[0], gas_object1.id()).await;
    let create1 =
        crate_object_move_transaction(addr1, &key1, addr1, 100, framework_obj_ref, gas_ref_1);

    do_transaction(authority_clients[0], &create1).await;
    do_transaction(authority_clients[1], &create1).await;
    do_transaction(authority_clients[2], &create1).await;

    // Get a cert
    let cert1 = extract_cert(&authority_clients, &authorities.committee, create1.digest()).await;

    // Submit the cert to 1 authority.
    let new_ref_1 = do_cert(authority_clients[0], &cert1).await.created[0].0;
    do_cert(authority_clients[1], &cert1).await;
    do_cert(authority_clients[2], &cert1).await;

    // Check the new object is at version 1
    let new_object_version = authorities.get_latest_sequence_number(new_ref_1.0).await;
    assert_eq!(SequenceNumber::from(1), new_object_version);

    // Make a schedule of transactions
    let gas_ref_set = get_latest_ref(authority_clients[0], gas_object1.id()).await;
    let create2 =
        set_object_move_transaction(addr1, &key1, new_ref_1, 100, framework_obj_ref, gas_ref_set);

    do_transaction(authority_clients[0], &create2).await;
    do_transaction(authority_clients[1], &create2).await;
    do_transaction(authority_clients[2], &create2).await;

    let cert2 = extract_cert(&authority_clients, &authorities.committee, create2.digest()).await;

    // Test: process the certificate, including bring up to date authority 3.
    //       which is 2 certs behind.
    authorities
        .process_certificate(cert2, Duration::from_secs(10))
        .await
        .unwrap();

    let (owned_object, _) = authorities
        .get_all_owned_objects(addr1, Duration::from_secs(10))
        .await
        .unwrap();

    // As a result, we have 2 gas objects and 1 created object.
    assert_eq!(3, owned_object.len());
    // Check this is the latest version.
    let new_object_version = authorities.get_latest_sequence_number(new_ref_1.0).await;
    assert_eq!(SequenceNumber::from(2), new_object_version);
}
