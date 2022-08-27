// Copyright (c) 2022 MASSA LABS <info@massa.net>

// RUST_BACKTRACE=1 cargo test test_one_handshake -- --nocapture --test-threads=1

use super::tools::{protocol_test, protocol_test_with_storage};
use massa_models::prehash::PreHashMap;
use massa_models::{self, Address, Amount, Slot};
use massa_models::{operation::OperationId, prehash::PreHashSet};
use massa_network_exports::{BlockInfoReply, NetworkCommand};
use massa_pool_exports::test_exports::MockPoolControllerMessage;
use massa_protocol_exports::tests::tools;
use massa_protocol_exports::{BlocksResults, ProtocolEvent};
use serial_test::serial;
use std::str::FromStr;
use std::time::Duration;

#[tokio::test]
#[serial]
async fn test_protocol_sends_valid_operations_it_receives_to_consensus() {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    protocol_command_sender,
                    protocol_manager,
                    mut protocol_pool_event_receiver| {
            // Create 1 node.
            let mut nodes = tools::create_and_connect_nodes(1, &mut network_controller).await;

            let creator_node = nodes.pop().expect("Failed to get node info.");

            // 1. Create operation 1 and 2
            let operation_1 = tools::create_operation_with_expire_period(&creator_node.keypair, 1);

            let operation_2 = tools::create_operation_with_expire_period(&creator_node.keypair, 1);

            let expected_operation_id_1 = operation_1.verify_integrity().unwrap();
            let expected_operation_id_2 = operation_2.verify_integrity().unwrap();

            // 3. Send operation to protocol.
            network_controller
                .send_operations(
                    creator_node.id,
                    vec![operation_1.clone(), operation_2.clone()],
                )
                .await;

            // Check protocol sends operations to consensus.
            let received_operations =
                match protocol_pool_event_receiver.wait_command(1000.into(), |evt| match evt {
                    evt @ MockPoolControllerMessage::AddOperations { .. } => Some(evt),
                    _ => None,
                }) {
                    Some(MockPoolControllerMessage::AddOperations { operations, .. }) => operations,
                    _ => panic!("Unexpected or no protocol pool event."),
                };

            let op_refs = received_operations.get_op_refs();
            // Check the event includes the expected operations.
            assert!(op_refs.contains(&expected_operation_id_1));
            assert!(op_refs.contains(&expected_operation_id_2));

            let ops_reader = received_operations.read_operations();
            // Check that the operations come with their serialized representations.
            assert_eq!(
                expected_operation_id_1,
                ops_reader
                    .get(&expected_operation_id_1)
                    .unwrap()
                    .verify_integrity()
                    .unwrap()
            );
            assert_eq!(
                expected_operation_id_2,
                ops_reader
                    .get(&expected_operation_id_2)
                    .unwrap()
                    .verify_integrity()
                    .unwrap()
            );

            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                protocol_pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn test_protocol_does_not_send_invalid_operations_it_receives_to_consensus() {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    protocol_command_sender,
                    protocol_manager,
                    mut pool_event_receiver| {
            // Create 1 node.
            let mut nodes = tools::create_and_connect_nodes(1, &mut network_controller).await;

            let creator_node = nodes.pop().expect("Failed to get node info.");

            // 1. Create an operation.
            let mut operation =
                tools::create_operation_with_expire_period(&creator_node.keypair, 1);

            // Change the fee, making the signature invalid.
            operation.content.fee = Amount::from_str("111").unwrap();

            // 3. Send operation to protocol.
            network_controller
                .send_operations(creator_node.id, vec![operation])
                .await;

            // Check protocol does not send operations to consensus.
            pool_event_receiver.wait_command(1000.into(), |evt| match evt {
                MockPoolControllerMessage::AddOperations { .. } => None,
                _ => Some(MockPoolControllerMessage::Any),
            });

            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn test_protocol_propagates_operations_to_active_nodes() {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    mut protocol_command_sender,
                    protocol_manager,
                    mut pool_event_receiver| {
            // Create 2 nodes.
            let nodes = tools::create_and_connect_nodes(2, &mut network_controller).await;

            // 1. Create an operation
            let operation = tools::create_operation_with_expire_period(&nodes[0].keypair, 1);

            // Send operation and wait for the protocol event,
            // just to be sure the nodes are connected before sending the propagate command.
            network_controller
                .send_operations(nodes[0].id, vec![operation.clone()])
                .await;

            pool_event_receiver.wait_command(1000.into(), |evt| match evt {
                MockPoolControllerMessage::AddOperations { .. } => {
                    panic!("Unexpected or no protocol event.")
                }
                _ => Some(MockPoolControllerMessage::Any),
            });

            let expected_operation_id = operation.verify_integrity().unwrap();

            let mut ops: PreHashSet<OperationId> = PreHashSet::default();
            ops.insert(expected_operation_id);
            protocol_command_sender
                .propagate_operations(ops)
                .await
                .unwrap();

            loop {
                match network_controller
                    .wait_command(1000.into(), |cmd| match cmd {
                        cmd @ NetworkCommand::SendOperationAnnouncements { .. } => Some(cmd),
                        _ => None,
                    })
                    .await
                {
                    Some(NetworkCommand::SendOperationAnnouncements { to_node, batch }) => {
                        assert_eq!(batch.len(), 1);
                        assert!(batch.contains(&expected_operation_id.prefix()));
                        assert_eq!(nodes[1].id, to_node);
                        break;
                    }
                    _ => panic!("Unexpected or no network command."),
                };
            }
            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn test_protocol_propagates_operations_only_to_nodes_that_dont_know_about_it() {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    mut protocol_command_sender,
                    protocol_manager,
                    mut pool_event_receiver| {
            // Create 1 nodes.
            let nodes = tools::create_and_connect_nodes(1, &mut network_controller).await;

            // 1. Create an operation
            let operation = tools::create_operation_with_expire_period(&nodes[0].keypair, 1);

            // Send operation and wait for the protocol event,
            // just to be sure the nodes are connected before sending the propagate command.
            network_controller
                .send_operations(nodes[0].id, vec![operation.clone()])
                .await;
            pool_event_receiver.wait_command(1000.into(), |evt| match evt {
                MockPoolControllerMessage::AddOperations { .. } => {
                    panic!("Unexpected or no protocol event.")
                }
                _ => Some(MockPoolControllerMessage::Any),
            });
            // create and connect a node that does not know about the endorsement
            let new_nodes = tools::create_and_connect_nodes(1, &mut network_controller).await;

            // wait for things to settle
            tokio::time::sleep(Duration::from_millis(250)).await;

            let expected_operation_id = operation.verify_integrity().unwrap();

            let mut ops: PreHashSet<OperationId> = PreHashSet::default();
            ops.insert(expected_operation_id);

            // send endorsement to protocol
            // it should be propagated only to the node that doesn't know about it
            protocol_command_sender
                .propagate_operations(ops)
                .await
                .unwrap();

            loop {
                match network_controller
                    .wait_command(1000.into(), |cmd| match cmd {
                        cmd @ NetworkCommand::SendOperationAnnouncements { .. } => Some(cmd),
                        _ => None,
                    })
                    .await
                {
                    Some(NetworkCommand::SendOperationAnnouncements { to_node, batch }) => {
                        assert_eq!(batch.len(), 1);
                        assert!(batch.contains(&expected_operation_id.prefix()));
                        assert_eq!(new_nodes[0].id, to_node);
                        break;
                    }
                    _ => panic!("Unexpected or no network command."),
                };
            }
            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
#[ignore]
async fn test_protocol_propagates_operations_only_to_nodes_that_dont_know_about_it_get_block_results(
) {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    mut protocol_command_sender,
                    protocol_manager,
                    protocol_pool_event_receiver| {
            // Create 1 node.
            let nodes = tools::create_and_connect_nodes(1, &mut network_controller).await;

            let address = Address::from_public_key(&nodes[0].id.0);
            let thread = address.get_thread(2);

            let operation = tools::create_operation_with_expire_period(&nodes[0].keypair, 1);
            let operation_id = operation.id;

            let block = tools::create_block_with_operations(
                &nodes[0].keypair,
                Slot::new(1, thread),
                vec![operation.clone()],
            );
            let expected_block_id = block.id;

            network_controller
                .send_ask_for_block(nodes[0].id, vec![(expected_block_id, Default::default())])
                .await;

            // Send the block as search results.
            let mut results: BlocksResults = PreHashMap::default();
            let mut ops: PreHashSet<OperationId> = PreHashSet::default();
            ops.insert(operation_id);
            results.insert(expected_block_id, Some((Some(ops), None)));

            // TODO: rewrite

            // Send the endorsement to protocol
            // it should not propagate to the node that already knows about it
            // because of the previously integrated block.
            let mut ops = PreHashSet::default();
            ops.insert(operation_id);
            protocol_command_sender
                .propagate_operations(ops)
                .await
                .unwrap();

            match network_controller
                .wait_command(1000.into(), |cmd| match cmd {
                    cmd @ NetworkCommand::SendOperationAnnouncements { .. } => Some(cmd),
                    _ => None,
                })
                .await
            {
                Some(NetworkCommand::SendOperationAnnouncements { to_node, batch }) => {
                    panic!(
                        "Unexpected propagated of operation to node {to_node} of {:?}.",
                        batch
                    );
                }
                None => {}
                Some(cmd) => panic!("Unexpected network command.{:?}", cmd),
            };

            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                protocol_pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
#[ignore]
async fn test_protocol_propagates_operations_only_to_nodes_that_dont_know_about_it_indirect_knowledge_via_header(
) {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    mut protocol_command_sender,
                    protocol_manager,
                    protocol_pool_event_receiver| {
            // Create 2 nodes.
            let nodes = tools::create_and_connect_nodes(2, &mut network_controller).await;

            let address = Address::from_public_key(&nodes[0].id.0);
            let thread = address.get_thread(2);

            let operation = tools::create_operation_with_expire_period(&nodes[0].keypair, 1);
            let operation_id = operation.id;

            let block = tools::create_block_with_operations(
                &nodes[0].keypair,
                Slot::new(1, thread),
                vec![operation.clone()],
            );

            // Node 2 sends block info with ops list, resulting in protocol using the info to determine
            // the node knows about the operations contained in the block.
            network_controller
                .send_block_info(
                    nodes[1].id,
                    vec![(
                        block.id,
                        BlockInfoReply::Info(vec![operation.id].into_iter().collect()),
                    )],
                )
                .await;

            // Node 1 sends block info with actual ops, resulting in protocol using the info to determine
            // the node knows about the operations contained in the block.
            network_controller
                .send_block_info(
                    nodes[0].id,
                    vec![(
                        block.id,
                        BlockInfoReply::Operations(vec![operation].into_iter().collect()),
                    )],
                )
                .await;

            // Send the operation to protocol
            // it should not propagate to the node that already knows about it
            // because of the previously received header.
            let mut ops = PreHashSet::default();
            ops.insert(operation_id);
            protocol_command_sender
                .propagate_operations(ops)
                .await
                .unwrap();

            match network_controller
                .wait_command(1000.into(), |cmd| match cmd {
                    cmd @ NetworkCommand::SendOperationAnnouncements { .. } => Some(cmd),
                    _ => None,
                })
                .await
            {
                Some(NetworkCommand::SendOperationAnnouncements { to_node, batch }) => {
                    panic!(
                        "Unexpected propagation of operation to node {to_node} of {:?}.",
                        batch
                    );
                }
                None => {}
                Some(cmd) => panic!("Unexpected network command.{:?}", cmd),
            };

            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                protocol_pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
#[ignore]
async fn test_protocol_propagates_operations_only_to_nodes_that_dont_know_about_it_indirect_knowledge_via_wrong_root_hash_header(
) {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    mut protocol_event_receiver,
                    mut protocol_command_sender,
                    protocol_manager,
                    protocol_pool_event_receiver| {
            // Create 3 nodes.
            let nodes = tools::create_and_connect_nodes(3, &mut network_controller).await;

            let address = Address::from_public_key(&nodes[0].id.0);
            let thread = address.get_thread(2);

            let operation = tools::create_operation_with_expire_period(&nodes[0].keypair, 1);

            let operation_2 = tools::create_operation_with_expire_period(&nodes[0].keypair, 1);
            let operation_id_2 = operation_2.id;

            let mut block = tools::create_block_with_operations(
                &nodes[0].keypair,
                Slot::new(1, thread),
                vec![operation.clone()],
            );

            // Change the root operation hash
            block.content.operations = vec![operation_2.clone()]
                .into_iter()
                .map(|op| op.id)
                .collect();

            // Node 2 sends block, not resulting in operations and endorsements noted in block info,
            // because of the invalid root hash.

            // Node 3 sends block, resulting in operations and endorsements noted in block info.
            // TODO: rewrite with block info.

            // Node 1 sends header, but the block is empty.
            network_controller
                .send_header(nodes[0].id, block.content.header.clone())
                .await;

            // Wait for the event to be sure that the node is connected.
            let _ = tools::wait_protocol_event(&mut protocol_event_receiver, 1000.into(), |evt| {
                match evt {
                    evt @ ProtocolEvent::ReceivedBlockHeader { .. } => Some(evt),
                    _ => None,
                }
            })
            .await;

            // Send the operation to protocol
            // it should propagate to the node because it isn't in the block.
            let mut ops = PreHashSet::default();
            ops.insert(operation_id_2);
            protocol_command_sender
                .propagate_operations(ops)
                .await
                .unwrap();

            match network_controller
                .wait_command(1000.into(), |cmd| match cmd {
                    cmd @ NetworkCommand::SendOperationAnnouncements { .. } => Some(cmd),
                    _ => None,
                })
                .await
            {
                Some(NetworkCommand::SendOperationAnnouncements { to_node, batch }) => {
                    assert_eq!(batch.len(), 1);
                    assert!(batch.contains(&operation_id_2.prefix()));
                    assert_eq!(nodes[0].id, to_node);
                }
                None => panic!("Operation not propagated."),
                Some(cmd) => panic!("Unexpected network command.{:?}", cmd),
            };

            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                protocol_pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
#[ignore]
async fn test_protocol_does_not_propagates_operations_when_receiving_those_inside_a_block() {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    protocol_command_sender,
                    protocol_manager,
                    mut pool_event_receiver| {
            // Create 2 nodes.
            let mut nodes = tools::create_and_connect_nodes(2, &mut network_controller).await;

            let creator_node = nodes.pop().expect("Failed to get node info.");

            // 1. Create an operation
            let operation = tools::create_operation_with_expire_period(&creator_node.keypair, 1);

            let address = Address::from_public_key(&creator_node.id.0);
            let thread = address.get_thread(2);

            // 2. Create a block coming from node creator_node, and including the operation.
            let block = tools::create_block_with_operations(
                &creator_node.keypair,
                Slot::new(1, thread),
                vec![operation.clone()],
            );

            // 4. Send block to protocol.
            network_controller
                .send_header(creator_node.id, block.content.header.clone())
                .await;

            // 5. Check that the operation included in the block is not propagated.

            match pool_event_receiver.wait_command(1000.into(), |evt| match evt {
                evt @ MockPoolControllerMessage::AddOperations { .. } => Some(evt),
                _ => None,
            }) {
                None => panic!("Protocol did not send operations to pool."),
                Some(MockPoolControllerMessage::AddOperations { operations }) => {
                    let expected_id = operation.verify_integrity().unwrap();
                    let op_refs = operations.get_op_refs();
                    assert!(op_refs.contains(&expected_id));
                    assert_eq!(op_refs.len(), 1);
                    let ops_reader = operations.read_operations();
                    assert_eq!(
                        expected_id,
                        ops_reader
                            .get(&expected_id)
                            .unwrap()
                            .verify_integrity()
                            .unwrap()
                    );
                }
                Some(_) => panic!("Unexpected protocol pool event."),
            }
            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn test_protocol_ask_operations_on_batch_received() {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    protocol_command_sender,
                    protocol_manager,
                    protocol_pool_event_receiver| {
            // Create 1 node.
            let mut nodes = tools::create_and_connect_nodes(1, &mut network_controller).await;

            let creator_node = nodes.pop().expect("Failed to get node info.");

            // 1. Create an operation
            let operation = tools::create_operation_with_expire_period(&creator_node.keypair, 1);

            let expected_operation_id = operation.verify_integrity().unwrap();
            // 3. Send operation batch to protocol.
            network_controller
                .send_operation_batch(
                    creator_node.id,
                    vec![expected_operation_id].iter().cloned().collect(),
                )
                .await;

            match network_controller
                .wait_command(1000.into(), |cmd| match cmd {
                    cmd @ NetworkCommand::AskForOperations { .. } => Some(cmd),
                    _ => None,
                })
                .await
            {
                Some(NetworkCommand::AskForOperations { to_node, wishlist }) => {
                    assert_eq!(wishlist.len(), 1);
                    assert!(wishlist.contains(&expected_operation_id.prefix()));
                    assert_eq!(to_node, creator_node.id);
                }
                _ => panic!("Unexpected or no network command."),
            };

            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                protocol_pool_event_receiver,
            )
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn test_protocol_on_ask_operations() {
    let protocol_config = &tools::PROTOCOL_CONFIG;
    protocol_test_with_storage(
        protocol_config,
        async move |mut network_controller,
                    protocol_event_receiver,
                    protocol_command_sender,
                    protocol_manager,
                    protocol_pool_event_receiver,
                    mut storage| {
            // Create 1 node.
            let mut nodes = tools::create_and_connect_nodes(2, &mut network_controller).await;

            let creator_node = nodes.pop().expect("Failed to get node info.");

            // 1. Create an operation
            let operation = tools::create_operation_with_expire_period(&creator_node.keypair, 1);

            let expected_operation_id = operation.verify_integrity().unwrap();

            // 2. Send operation
            network_controller
                .send_operations(creator_node.id, vec![operation.clone()])
                .await;

            // Store in shared storage.
            storage.store_operations(vec![operation.clone()]);

            // 3. A node asks for the operation.
            let asker_node = nodes.pop().expect("Failed to get the second node info.");

            network_controller
                .send_ask_for_operation(asker_node.id, vec![expected_operation_id])
                .await;

            // 4. Assert the operation is sent to the node.
            match network_controller
                .wait_command(1000.into(), |cmd| match cmd {
                    cmd @ NetworkCommand::SendOperations { .. } => Some(cmd),
                    _ => None,
                })
                .await
            {
                Some(NetworkCommand::SendOperations { node, operations }) => {
                    assert_eq!(asker_node.id, node);
                    assert!(!operations.is_empty())
                }
                _ => panic!("Unexpected or no network command."),
            };

            (
                network_controller,
                protocol_event_receiver,
                protocol_command_sender,
                protocol_manager,
                protocol_pool_event_receiver,
            )
        },
    )
    .await;
}
