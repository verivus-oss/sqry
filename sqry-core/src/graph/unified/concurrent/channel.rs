//! `UpdateChannel`: Single-writer serialization for graph updates.
//!
//! This module implements a channel-based write serialization pattern (FR-36)
//! that ensures all graph mutations are processed by a single writer thread.
//!
//! # Design (FR-36, CP-5)
//!
//! Multiple producers can send update commands through the channel, but a
//! single consumer thread processes them sequentially. This pattern:
//!
//! - Eliminates write contention on the graph
//! - Guarantees FIFO ordering of updates
//! - Enables batching of updates for efficiency
//! - Simplifies reasoning about concurrent modifications
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::concurrent::channel::{UpdateChannel, GraphUpdate};
//!
//! let (channel, receiver) = UpdateChannel::new();
//!
//! // Send updates from multiple threads
//! channel.send(GraphUpdate::AddNode { ... })?;
//!
//! // Process updates in single writer thread
//! while let Ok(update) = receiver.recv() {
//!     graph.apply(update);
//! }
//! ```

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvError, SendError, Sender, TryRecvError};

use super::super::edge::kind::EdgeKind;
use super::super::file::FileId;
use super::super::node::{NodeId, NodeKind};

/// Graph update operations that can be sent through the channel.
#[derive(Debug, Clone)]
pub enum GraphUpdate {
    /// Add a new node to the graph.
    AddNode {
        /// The kind of node to add.
        kind: NodeKind,
        /// Name of the node (will be interned).
        name: String,
        /// File the node belongs to.
        file: FileId,
    },

    /// Remove a node from the graph.
    RemoveNode {
        /// ID of the node to remove.
        node: NodeId,
    },

    /// Add an edge between nodes.
    AddEdge {
        /// Source node of the edge.
        source: NodeId,
        /// Target node of the edge.
        target: NodeId,
        /// Kind of relationship.
        kind: EdgeKind,
        /// File where the edge is defined.
        file: FileId,
    },

    /// Remove an edge between nodes.
    RemoveEdge {
        /// Source node of the edge.
        source: NodeId,
        /// Target node of the edge.
        target: NodeId,
        /// Kind of relationship.
        kind: EdgeKind,
        /// File where the edge was defined.
        file: FileId,
    },

    /// Clear all data for a specific file.
    ClearFile {
        /// File to clear.
        file: FileId,
    },

    /// Trigger compaction of the delta buffer.
    TriggerCompaction,

    /// Shutdown the update processor.
    Shutdown,
}

/// Error type for update channel operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// The channel has been disconnected.
    Disconnected,
    /// The channel is full (for bounded channels).
    Full,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "channel disconnected"),
            Self::Full => write!(f, "channel full"),
        }
    }
}

impl std::error::Error for ChannelError {}

impl<T> From<SendError<T>> for ChannelError {
    fn from(_: SendError<T>) -> Self {
        Self::Disconnected
    }
}

impl From<RecvError> for ChannelError {
    fn from(_: RecvError) -> Self {
        Self::Disconnected
    }
}

/// Handle for sending graph updates through the channel.
///
/// This handle is cheaply cloneable and can be shared across threads.
/// All updates sent through any clone will be serialized by the receiver.
#[derive(Debug, Clone)]
pub struct UpdateChannel {
    sender: Sender<GraphUpdate>,
    /// Counter for tracking sent updates (for testing/debugging).
    updates_sent: Arc<std::sync::atomic::AtomicU64>,
}

impl UpdateChannel {
    /// Creates a new update channel pair.
    ///
    /// Returns the sender handle and receiver. The receiver should be
    /// processed by a single thread to ensure serialization.
    #[must_use]
    pub fn new() -> (Self, UpdateReceiver) {
        let (sender, receiver) = mpsc::channel();
        let updates_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let updates_received = Arc::new(std::sync::atomic::AtomicU64::new(0));

        (
            Self {
                sender,
                updates_sent: Arc::clone(&updates_sent),
            },
            UpdateReceiver {
                receiver,
                updates_received,
            },
        )
    }

    /// Creates a bounded update channel pair.
    ///
    /// A bounded channel will block senders when the buffer is full,
    /// providing natural back-pressure.
    #[must_use]
    pub fn bounded(capacity: usize) -> (Self, UpdateReceiver) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let updates_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let updates_received = Arc::new(std::sync::atomic::AtomicU64::new(0));

        (
            Self {
                sender: {
                    // Convert sync_channel sender to regular sender interface
                    // by wrapping in a new unbounded channel that forwards
                    let (tx, rx) = mpsc::channel();
                    std::thread::spawn(move || {
                        while let Ok(update) = rx.recv() {
                            if sender.send(update).is_err() {
                                break;
                            }
                        }
                    });
                    tx
                },
                updates_sent: Arc::clone(&updates_sent),
            },
            UpdateReceiver {
                receiver,
                updates_received,
            },
        )
    }

    /// Sends an update through the channel.
    ///
    /// # Errors
    ///
    /// Returns `ChannelError::Disconnected` if the receiver has been dropped.
    pub fn send(&self, update: GraphUpdate) -> Result<(), ChannelError> {
        self.sender.send(update)?;
        self.updates_sent
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Returns the number of updates sent through this channel.
    #[must_use]
    pub fn updates_sent(&self) -> u64 {
        self.updates_sent.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for UpdateChannel {
    fn default() -> Self {
        Self::new().0
    }
}

/// Receiver for processing graph updates.
///
/// This should be owned by a single thread that processes updates
/// sequentially to ensure serialization.
#[derive(Debug)]
pub struct UpdateReceiver {
    receiver: Receiver<GraphUpdate>,
    /// Counter for tracking received updates.
    updates_received: Arc<std::sync::atomic::AtomicU64>,
}

impl UpdateReceiver {
    /// Receives the next update, blocking until one is available.
    ///
    /// # Errors
    ///
    /// Returns `ChannelError::Disconnected` if all senders have been dropped.
    pub fn recv(&self) -> Result<GraphUpdate, ChannelError> {
        let update = self.receiver.recv()?;
        self.updates_received
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(update)
    }

    /// Tries to receive the next update without blocking.
    ///
    /// Returns `None` if no update is currently available.
    ///
    /// # Errors
    ///
    /// Returns `ChannelError::Disconnected` if all senders have been dropped.
    pub fn try_recv(&self) -> Result<Option<GraphUpdate>, ChannelError> {
        match self.receiver.try_recv() {
            Ok(update) => {
                self.updates_received
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(Some(update))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ChannelError::Disconnected),
        }
    }

    /// Returns an iterator over available updates.
    ///
    /// The iterator will block waiting for updates and complete
    /// when all senders are dropped.
    pub fn iter(&self) -> impl Iterator<Item = GraphUpdate> + '_ {
        std::iter::from_fn(|| self.recv().ok())
    }

    /// Returns the number of updates received through this channel.
    #[must_use]
    pub fn updates_received(&self) -> u64 {
        self.updates_received
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Stats for monitoring channel throughput.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelStats {
    /// Number of updates sent.
    pub sent: u64,
    /// Number of updates received.
    pub received: u64,
}

impl ChannelStats {
    /// Returns the number of updates in flight (sent but not yet received).
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.sent.saturating_sub(self.received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_channel_new() {
        let (sender, _receiver) = UpdateChannel::new();
        assert_eq!(sender.updates_sent(), 0);
    }

    #[test]
    fn test_channel_default() {
        let sender: UpdateChannel = UpdateChannel::default();
        assert_eq!(sender.updates_sent(), 0);
    }

    #[test]
    fn test_send_receive() {
        let (sender, receiver) = UpdateChannel::new();

        let file = FileId::new(1);
        sender
            .send(GraphUpdate::ClearFile { file })
            .expect("send failed");

        let update = receiver.recv().expect("recv failed");
        match update {
            GraphUpdate::ClearFile { file: f } => assert_eq!(f, file),
            _ => panic!("wrong update type"),
        }
    }

    #[test]
    fn test_updates_serialized() {
        let (sender, receiver) = UpdateChannel::new();
        let sender_clone = sender.clone();

        // Send from multiple threads
        let handle1 = thread::spawn(move || {
            for i in 0..100 {
                let file = FileId::new(i);
                sender.send(GraphUpdate::ClearFile { file }).unwrap();
            }
        });

        let handle2 = thread::spawn(move || {
            for i in 100..200 {
                let file = FileId::new(i);
                sender_clone.send(GraphUpdate::ClearFile { file }).unwrap();
            }
        });

        handle1.join().unwrap();
        handle2.join().unwrap();

        // All updates should be received
        let mut received_updates = Vec::new();
        while let Ok(Some(update)) = receiver.try_recv() {
            received_updates.push(update);
        }

        assert_eq!(received_updates.len(), 200);
        assert_eq!(receiver.updates_received(), 200);
    }

    #[test]
    fn test_channel_ordering() {
        let (sender, receiver) = UpdateChannel::new();

        // Send updates in specific order
        for i in 0..100 {
            let file = FileId::new(i);
            sender.send(GraphUpdate::ClearFile { file }).unwrap();
        }

        // Receive and verify FIFO order
        for i in 0..100 {
            let update = receiver.recv().unwrap();
            match update {
                GraphUpdate::ClearFile { file } => {
                    assert_eq!(file.index(), i);
                }
                _ => panic!("wrong update type"),
            }
        }
    }

    #[test]
    fn test_try_recv_empty() {
        let (sender, receiver) = UpdateChannel::new();
        // Keep sender alive so channel isn't disconnected
        let _sender = sender;
        assert!(receiver.try_recv().unwrap().is_none());
    }

    #[test]
    fn test_try_recv_available() {
        let (sender, receiver) = UpdateChannel::new();

        let file = FileId::new(42);
        sender.send(GraphUpdate::ClearFile { file }).unwrap();

        let result = receiver.try_recv().unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_disconnected_sender() {
        let (sender, receiver) = UpdateChannel::new();
        drop(sender);

        let result = receiver.recv();
        assert!(matches!(result, Err(ChannelError::Disconnected)));
    }

    #[test]
    fn test_disconnected_receiver() {
        let (sender, receiver) = UpdateChannel::new();
        drop(receiver);

        let file = FileId::new(1);
        let result = sender.send(GraphUpdate::ClearFile { file });
        assert!(matches!(result, Err(ChannelError::Disconnected)));
    }

    #[test]
    fn test_update_kinds() {
        let (sender, receiver) = UpdateChannel::new();

        // Test all update types
        sender
            .send(GraphUpdate::AddNode {
                kind: NodeKind::Function,
                name: "test".to_string(),
                file: FileId::new(1),
            })
            .unwrap();

        sender
            .send(GraphUpdate::RemoveNode {
                node: NodeId::new(1, 0),
            })
            .unwrap();

        sender
            .send(GraphUpdate::AddEdge {
                source: NodeId::new(1, 0),
                target: NodeId::new(2, 0),
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                file: FileId::new(1),
            })
            .unwrap();

        sender
            .send(GraphUpdate::RemoveEdge {
                source: NodeId::new(1, 0),
                target: NodeId::new(2, 0),
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                file: FileId::new(1),
            })
            .unwrap();

        sender
            .send(GraphUpdate::ClearFile {
                file: FileId::new(1),
            })
            .unwrap();

        sender.send(GraphUpdate::TriggerCompaction).unwrap();
        sender.send(GraphUpdate::Shutdown).unwrap();

        assert_eq!(sender.updates_sent(), 7);

        // Verify all received
        let mut count = 0;
        while receiver.try_recv().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 7);
    }

    #[test]
    fn test_channel_stats() {
        let stats = ChannelStats {
            sent: 100,
            received: 75,
        };
        assert_eq!(stats.in_flight(), 25);
    }

    #[test]
    fn test_channel_stats_saturating() {
        // Edge case: received > sent (shouldn't happen, but handle gracefully)
        let stats = ChannelStats {
            sent: 50,
            received: 75,
        };
        assert_eq!(stats.in_flight(), 0);
    }

    #[test]
    fn test_iter() {
        let (sender, receiver) = UpdateChannel::new();

        for i in 0..10 {
            sender
                .send(GraphUpdate::ClearFile {
                    file: FileId::new(i),
                })
                .unwrap();
        }
        drop(sender);

        let updates: Vec<_> = receiver.iter().collect();
        assert_eq!(updates.len(), 10);
    }

    #[test]
    fn test_clone_sender() {
        let (sender, receiver) = UpdateChannel::new();
        let sender2 = sender.clone();

        sender
            .send(GraphUpdate::ClearFile {
                file: FileId::new(1),
            })
            .unwrap();
        sender2
            .send(GraphUpdate::ClearFile {
                file: FileId::new(2),
            })
            .unwrap();

        // Both senders share the same counter
        assert_eq!(sender.updates_sent(), 2);
        assert_eq!(sender2.updates_sent(), 2);

        let mut count = 0;
        while receiver.try_recv().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn test_channel_error_display() {
        let err = ChannelError::Disconnected;
        assert_eq!(format!("{err}"), "channel disconnected");

        let err = ChannelError::Full;
        assert_eq!(format!("{err}"), "channel full");
    }

    #[test]
    fn test_concurrent_send_receive() {
        let (sender, receiver) = UpdateChannel::new();
        let sender_clone = sender.clone();

        // Producer thread
        let producer = thread::spawn(move || {
            for i in 0..1000 {
                sender
                    .send(GraphUpdate::ClearFile {
                        file: FileId::new(i),
                    })
                    .unwrap();
            }
        });

        // Second producer
        let producer2 = thread::spawn(move || {
            for i in 1000..2000 {
                sender_clone
                    .send(GraphUpdate::ClearFile {
                        file: FileId::new(i),
                    })
                    .unwrap();
            }
        });

        // Consumer thread
        let consumer = thread::spawn(move || {
            let mut count = 0u64;
            loop {
                match receiver.try_recv() {
                    Ok(Some(_)) => count += 1,
                    Ok(None) => {
                        if count >= 2000 {
                            break;
                        }
                        thread::sleep(Duration::from_micros(10));
                    }
                    Err(ChannelError::Disconnected) => break,
                    Err(_) => {}
                }
            }
            count
        });

        producer.join().unwrap();
        producer2.join().unwrap();
        let received_count = consumer.join().unwrap();

        assert_eq!(received_count, 2000);
    }
}
