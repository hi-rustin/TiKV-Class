use crate::proto::raftpb::*;
use crate::raft::defs::{Action, State};
use crate::raft::errors;
use crate::raft::errors::Error;
use crate::raft::raft_peer::RaftPeer;
use crate::raft::raft_server::RaftSever;
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::channel::oneshot::{channel, Canceled};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;
use tokio::runtime::Runtime;

// Choose concurrency paradigm.
//
// You can either drive the raft state machine by the rpc framework,
//
// ```rust
// struct Node { raft: Arc<Mutex<Raft>> }
// ```
//
// or spawn a new thread runs the raft state machine and communicate via
// a channel.
//
// ```rust
// struct Node { sender: Sender<Msg> }
// ```
#[derive(Clone)]
pub struct Node {
    msg_sender: UnboundedSender<Action>,
    current_term: Arc<AtomicU64>,
    is_leader: Arc<AtomicBool>,
    dead: Arc<AtomicBool>,
}

impl Node {
    /// Create a new raft service.
    pub fn new(raft: RaftPeer) -> Node {
        let (sender, receiver) = unbounded::<Action>();
        let node_sender = sender.clone();
        let last_receive_time = Arc::new(Mutex::new(Instant::now()));
        let current_term = Arc::clone(&raft.current_term);
        let is_leader_for_node = Arc::clone(&raft.is_leader);
        let dead_for_node = Arc::clone(&raft.dead);
        let mut server = RaftSever {
            raft,
            action_sender: sender,
            action_receiver: Arc::new(Mutex::new(receiver)),
            last_receive_time,
        };
        thread::spawn(move || server.action_handler());
        Node {
            msg_sender: node_sender,
            current_term,
            is_leader: is_leader_for_node,
            dead: dead_for_node,
        }
    }

    /// the service using Raft (e.g. a k/v server) wants to start
    /// agreement on the next command to be appended to Raft's log. if this
    /// server isn't the leader, returns [`Error::NotLeader`]. otherwise start
    /// the agreement and return immediately. there is no guarantee that this
    /// command will ever be committed to the Raft log, since the leader
    /// may fail or lose an election. even if the Raft instance has been killed,
    /// this function should return gracefully.
    ///
    /// the first value of the tuple is the index that the command will appear
    /// at if it's ever committed. the second is the current term.
    ///
    /// This method must return without blocking on the raft.
    pub fn start<M>(&self, command: &M) -> errors::Result<(u64, u64)>
    where
        M: labcodec::Message,
    {
        let mut command_buf = vec![];
        labcodec::encode(command, &mut command_buf).map_err(Error::Encode)?;
        let (sender, receiver) = channel();
        if !self.msg_sender.is_closed() {
            self.msg_sender
                .clone()
                .unbounded_send(Action::Start(command_buf, sender))
                .map_err(|_| ())
                .unwrap_or_else(|_| ());
        } else {
            return Err(Error::NotLeader);
        }
        let mut runtime = Runtime::new().unwrap();
        if let Ok(res) = runtime.block_on(async {
            return receiver.await;
        }) {
            res
        } else {
            Err(Error::NotLeader)
        }
    }

    /// The current term of this peer.
    pub fn term(&self) -> u64 {
        self.current_term.load(Ordering::SeqCst)
    }

    /// Whether this peer believes it is the leader.
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::SeqCst)
    }

    /// The current state of this peer.
    pub fn get_state(&self) -> State {
        State {
            term: self.term(),
            is_leader: self.is_leader(),
        }
    }

    /// the tester calls kill() when a Raft instance won't be
    /// needed again. you are not required to do anything in
    /// kill(), but it might be convenient to (for example)
    /// turn off debug output from this instance.
    /// In Raft paper, a server crash is a PHYSICAL crash,
    /// A.K.A all resources are reset. But we are simulating
    /// a VIRTUAL crash in tester, so take care of background
    /// threads you generated with this Raft Node.
    pub fn kill(&self) {
        self.dead.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl RaftService for Node {
    async fn request_vote(&self, args: RequestVoteArgs) -> labrpc::Result<RequestVoteReply> {
        let (sender, receiver) = channel();
        if !self.msg_sender.is_closed() {
            self.msg_sender
                .clone()
                .unbounded_send(Action::RequestVote(args, sender))
                .map_err(|_| ())
                .unwrap_or_else(|_| ());
        }
        match receiver.await {
            Ok(reply) => Ok(reply),
            Err(_) => Err(labrpc::Error::Recv(Canceled)),
        }
    }
    async fn append_logs(&self, args: AppendLogsArgs) -> labrpc::Result<AppendLogsReply> {
        let (sender, receiver) = channel();
        if !self.msg_sender.is_closed() {
            self.msg_sender
                .clone()
                .unbounded_send(Action::AppendLogs(args, sender))
                .map_err(|_| ())
                .unwrap_or_else(|_| ());
        }
        match receiver.await {
            Ok(reply) => Ok(reply),
            Err(_) => Err(labrpc::Error::Recv(Canceled)),
        }
    }
}
