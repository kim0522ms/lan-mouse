use super::*;
use crate::{
    config::ConfigClient,
    crypto::certificate_fingerprint,
    emulation::{Emulation, EmulationEvent},
    listen::LanMouseListener,
};
use std::sync::RwLock;
use tokio::{task::LocalSet, time::timeout};

#[tokio::test(flavor = "current_thread")]
async fn dtls_version_exchange_preserves_fork_capabilities_and_return_path() {
    LocalSet::new()
        .run_until(async {
            let client_cert = Certificate::generate_self_signed(["client".into()]).unwrap();
            let server_cert = Certificate::generate_self_signed(["server".into()]).unwrap();
            let authorized = Arc::new(RwLock::new(HashMap::from([(
                certificate_fingerprint(&client_cert),
                "client".into(),
            )])));
            let port = {
                let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                socket.local_addr().unwrap().port()
            };
            let listener = LanMouseListener::new(port, server_cert, authorized)
                .await
                .unwrap();
            let mut emulation =
                Emulation::new(Some(input_emulation::Backend::Dummy), listener, true, true);
            assert!(matches!(
                timeout(Duration::from_secs(5), emulation.event())
                    .await
                    .unwrap(),
                EmulationEvent::EmulationEnabled
            ));

            let manager = ClientManager::default();
            let handle = manager.add_with_config(ConfigClient {
                hostname: None,
                ips: HashSet::from(["127.0.0.1".parse().unwrap()]),
                port,
                pos: lan_mouse_ipc::Position::Left,
                active: true,
                enter_hook: None,
                leave_hook: None,
            });
            let mut connection = LanMouseConnection::new(client_cert, manager.clone());
            assert!(matches!(
                connection.send(ProtoEvent::Ping, handle).await,
                Err(LanMouseConnectionError::NotConnected)
            ));
            let peer_addr = timeout(Duration::from_secs(5), async {
                loop {
                    if let EmulationEvent::PeerHello { addr, commit } = emulation.event().await {
                        assert_eq!(commit, local_commit());
                        break addr;
                    }
                }
            })
            .await
            .expect("incoming Hello event");
            timeout(Duration::from_secs(5), async {
                while !manager.alive(handle)
                    || manager.get_state(handle).unwrap().1.peer_commit != Some(local_commit())
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("Hello reply and capability-bearing Pong");
            assert!(connection.supports_clipboard(handle));
            assert!(connection.supports_sync_lock(handle));

            // Exercise the real DTLS receive loop with malformed and obsolete packets.
            let addr = manager.active_addr(handle).unwrap();
            let wire = connection.conns.lock().await[&addr].clone();
            for packet in [&[0xff][..], &[0x0b, 1, 2][..], &[12][..]] {
                wire.send(packet).await.unwrap();
            }
            connection
                .send(
                    ProtoEvent::Enter {
                        pos: lan_mouse_proto::Position::Right,
                        capabilities: lan_mouse_proto::local_capabilities(true, true),
                    },
                    handle,
                )
                .await
                .unwrap();
            let (received_handle, event) = timeout(Duration::from_secs(5), connection.recv())
                .await
                .expect("Ack after invalid datagrams");
            assert_eq!(received_handle, handle);
            assert!(matches!(event, ProtoEvent::Ack { .. }));

            // This tests the lock wire path only: no Capture task runs a session lock.
            emulation.send_lock();
            let (_, event) = timeout(Duration::from_secs(5), connection.recv())
                .await
                .expect("fork Lock event");
            assert!(matches!(event, ProtoEvent::Lock));

            // Disable clipboard before returning, so this test never reads the OS clipboard.
            emulation.set_clipboard_sharing(false);
            emulation.set_sync_lock(false);
            timeout(Duration::from_secs(5), async {
                while connection.supports_clipboard(handle) || connection.supports_sync_lock(handle)
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("updated capability-bearing Pong");
            emulation.send_leave_event(peer_addr);
            let (_, event) = timeout(Duration::from_secs(5), connection.recv())
                .await
                .expect("return barrier Leave");
            assert!(matches!(event, ProtoEvent::Leave(_)));

            emulation.terminate().await;
            timeout(Duration::from_secs(5), async {
                while manager.active_addr(handle).is_some() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("disconnect cleanup");
            assert_eq!(manager.get_state(handle).unwrap().1.peer_commit, None);
            assert!(!connection.supports_clipboard(handle));
            assert!(!connection.supports_sync_lock(handle));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn receive_loop_keeps_datagram_boundaries_and_ignores_invalid_packets() {
    LocalSet::new()
        .run_until(async {
            let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
            let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let addr = peer.local_addr().unwrap();
            socket.connect(addr).await.unwrap();
            peer.connect(socket.local_addr().unwrap()).await.unwrap();
            let manager = ClientManager::default();
            let handle = manager.add_client();
            let capabilities = Rc::new(RefCell::new(HashMap::new()));
            let (tx, mut rx) = channel();
            let task = spawn_local(receive_loop(
                manager.clone(),
                handle,
                addr,
                socket,
                Default::default(),
                tx,
                Default::default(),
                capabilities.clone(),
            ));

            let clipboard = lan_mouse_proto::ClipboardData {
                transfer_id: 7,
                chunk_index: 0,
                chunk_count: 1,
                mime: lan_mouse_proto::CLIPBOARD_MIME_TEXT.into(),
                payload: vec![b'x'; 1024],
            };
            let (buf, len) = ProtoEvent::Clipboard(clipboard.clone()).into();
            peer.send(&buf[..len]).await.unwrap();
            let (_, event) = timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(event, ProtoEvent::Clipboard(data) if data == clipboard));
            // A short Hello following a large packet must not include stale buffer bytes.
            peer.send(b"\x0bf1b96bdc").await.unwrap();
            for packet in [&[0x0b, 0][..], &[0xff][..], &[12][..]] {
                peer.send(packet).await.unwrap();
            }
            let (buf, len) = ProtoEvent::Pong {
                alive: true,
                capabilities: CAP_CLIPBOARD | CAP_SYNC_LOCK,
            }
            .into();
            peer.send(&buf[..len]).await.unwrap();
            peer.send(&[0x81]).await.unwrap();
            let (_, event) = timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(event, ProtoEvent::Lock));
            assert_eq!(
                manager.get_state(handle).unwrap().1.peer_commit,
                Some(*b"f1b96bdc")
            );
            assert_eq!(
                capabilities.borrow().get(&handle),
                Some(&(CAP_CLIPBOARD | CAP_SYNC_LOCK))
            );
            assert!(manager.alive(handle));
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        })
        .await;
}
