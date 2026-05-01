use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, trace};

/// Extracts the server's `ufrag` from a STUN Binding Request packet.
/// 
/// In WebRTC, the STUN USERNAME attribute is formatted as `<server_ufrag>:<client_ufrag>`.
pub fn extract_ufrag(packet: &[u8]) -> Option<String> {
    if packet.len() < 20 {
        return None;
    }
    // STUN message type is 14 bits. Top 2 bits must be 00.
    if packet[0] & 0xC0 != 0 {
        return None;
    }
    // Magic cookie (0x2112A442)
    if packet[4..8] != [0x21, 0x12, 0xA4, 0x42] {
        return None;
    }

    let mut offset = 20;
    let len = packet.len();
    while offset + 4 <= len {
        let attr_type = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let attr_len = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        offset += 4;

        if offset + attr_len > len {
            break;
        }

        if attr_type == 0x0006 {
            // USERNAME attribute
            if let Ok(s) = std::str::from_utf8(&packet[offset..offset + attr_len]) {
                // The username is usually "<server_ufrag>:<client_ufrag>"
                return s.split(':').next().map(|s| s.to_string());
            }
        }

        // Attributes are padded to 4-byte boundaries
        offset += (attr_len + 3) & !3;
    }
    None
}

/// A global UDP multiplexer that listens on a single UDP port and routes
/// incoming packets to the appropriate WebRTC session based on STUN ufrags
/// and previously learned socket addresses.
pub struct UdpMux {
    socket: Arc<UdpSocket>,
    /// Map of `server_ufrag` -> Sender
    ufrag_map: RwLock<HashMap<String, mpsc::UnboundedSender<(Bytes, SocketAddr)>>>,
    /// Map of learned `SocketAddr` -> Sender (fast path for RTP/DTLS)
    addr_map: RwLock<HashMap<SocketAddr, mpsc::UnboundedSender<(Bytes, SocketAddr)>>>,
    /// Reverse map of `server_ufrag` -> Set of learned `SocketAddr`s, for cleanup.
    ufrag_addr_index: RwLock<HashMap<String, Vec<SocketAddr>>>,
}

impl UdpMux {
    /// Creates a new multiplexer by binding a single UDP socket.
    pub async fn new(bind_addr: &str) -> std::io::Result<Arc<Self>> {
        let socket = UdpSocket::bind(bind_addr).await?;
        let socket = Arc::new(socket);
        let mux = Arc::new(Self {
            socket: socket.clone(),
            ufrag_map: RwLock::new(HashMap::new()),
            addr_map: RwLock::new(HashMap::new()),
            ufrag_addr_index: RwLock::new(HashMap::new()),
        });

        // Spawn the receive loop
        let mux_clone = mux.clone();
        tokio::spawn(async move {
            mux_clone.receive_loop().await;
        });

        Ok(mux)
    }

    /// The single shared UDP socket, used by sessions to send packets out.
    pub fn socket(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }

    /// Registers a new WebRTC session with its local `ufrag`.
    pub async fn register(&self, ufrag: String, tx: mpsc::UnboundedSender<(Bytes, SocketAddr)>) {
        debug!("Registering ufrag: {}", ufrag);
        self.ufrag_map.write().await.insert(ufrag, tx);
    }

    /// Unregisters a WebRTC session and cleans up all its address mappings.
    pub async fn unregister(&self, ufrag: &str) {
        debug!("Unregistering ufrag: {}", ufrag);
        self.ufrag_map.write().await.remove(ufrag);
        // Clean up all learned addresses for this session using the reverse index.
        if let Some(addrs) = self.ufrag_addr_index.write().await.remove(ufrag) {
            let mut addr_map = self.addr_map.write().await;
            for addr in addrs {
                addr_map.remove(&addr);
            }
        }
    }

    async fn receive_loop(&self) {
        // 4096 bytes: standard MTU is 1500, but tunnel headers (GRE, VXLAN, etc.)
        // can push SRTP/DTLS packets above 2000 bytes. 4096 is a safe ceiling.
        let mut buf = vec![0u8; 4096];
        loop {
            match self.socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    let packet = Bytes::copy_from_slice(&buf[..len]);
                    self.route_packet(packet, addr).await;
                }
                Err(e) => {
                    error!("UdpMux receive error: {}", e);
                    // Just wait a bit to avoid tight loop on persistent errors
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    async fn route_packet(&self, packet: Bytes, addr: SocketAddr) {
        let mut addr_was_known = false;
        // Fast path: Check if we already know this address
        {
            let addrs = self.addr_map.read().await;
            if let Some(tx) = addrs.get(&addr) {
                addr_was_known = true;
                if tx.send((packet.clone(), addr)).is_ok() {
                    return;
                }
            }
        }

        // Slow path: Not in map, or sender is dead.
        // We must check if this is a STUN packet to learn the mapping.

        if let Some(ufrag) = extract_ufrag(&packet) {
            let ufrags = self.ufrag_map.read().await;
            if let Some(tx) = ufrags.get(&ufrag) {
                // We found the session! Send the packet.
                if tx.send((packet.clone(), addr)).is_ok() {
                    // Save the mapping for future packets (RTP, DTLS)
                    let ufrag_owned = ufrag.clone();
                    let tx_clone = tx.clone();
                    drop(ufrags); // drop ufrag_map read lock before taking write locks
                    self.addr_map.write().await.insert(addr, tx_clone);
                    self.ufrag_addr_index.write().await
                        .entry(ufrag_owned)
                        .or_default()
                        .push(addr);
                    trace!("Learned new address {} for ufrag {}", addr, ufrag);
                    return;
                }
            } else {
                tracing::debug!("Received STUN for unknown ufrag: {}", ufrag);
            }
        } else {
            // Not a STUN packet and unknown address. We drop it.
            tracing::trace!("Dropped non-STUN packet from unknown address {} (len={})", addr, packet.len());
        }

        // Only take the write lock if we know the address was present (dead sender path).
        if addr_was_known {
            self.addr_map.write().await.remove(&addr);
        }
    }
}
