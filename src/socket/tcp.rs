use core::fmt;

use Error;
use Managed;
use wire::{IpProtocol, IpAddress, IpEndpoint};
use wire::{TcpPacket, TcpRepr, TcpControl};
use socket::{Socket, IpRepr, IpPayload};

/// A TCP stream ring buffer.
#[derive(Debug)]
pub struct SocketBuffer<'a> {
    storage: Managed<'a, [u8]>,
    read_at: usize,
    length:  usize
}

impl<'a> SocketBuffer<'a> {
    /// Create a packet buffer with the given storage.
    pub fn new<T>(storage: T) -> SocketBuffer<'a>
            where T: Into<Managed<'a, [u8]>> {
        SocketBuffer {
            storage: storage.into(),
            read_at: 0,
            length:  0
        }
    }

    fn capacity(&self) -> usize {
        self.storage.len()
    }

    fn len(&self) -> usize {
        self.length
    }

    fn window(&self) -> usize {
        self.capacity() - self.len()
    }

    fn clamp_writer(&self, mut size: usize) -> (usize, usize) {
        let write_at = (self.read_at + self.length) % self.storage.len();
        // We can't enqueue more than there is free space.
        let free = self.storage.len() - self.length;
        if size > free { size = free }
        // We can't contiguously enqueue past the beginning of the storage.
        let until_end = self.storage.len() - write_at;
        if size > until_end { size = until_end }

        (write_at, size)
    }

    fn enqueue(&mut self, size: usize) -> &mut [u8] {
        let (write_at, size) = self.clamp_writer(size);
        self.length += size;
        &mut self.storage[write_at..write_at + size]
    }

    fn enqueue_slice(&mut self, data: &[u8]) {
        let data = {
            let mut dest = self.enqueue(data.len());
            let (data, rest) = data.split_at(dest.len());
            dest.copy_from_slice(data);
            rest
        };
        // Retry, in case we had a wraparound.
        let mut dest = self.enqueue(data.len());
        let (data, _) = data.split_at(dest.len());
        dest.copy_from_slice(data);
    }

    fn clamp_reader(&self, offset: usize, mut size: usize) -> (usize, usize) {
        let read_at = (self.read_at + offset) % self.storage.len();
        // We can't dequeue more than was queued.
        if size > self.length { size = self.length }
        // We can't contiguously dequeue past the end of the storage.
        let until_end = self.storage.len() - read_at;
        if size > until_end { size = until_end }

        (read_at, size)
    }

    #[allow(dead_code)] // only used in tests
    fn dequeue(&mut self, size: usize) -> &[u8] {
        let (read_at, size) = self.clamp_reader(0, size);
        self.read_at = (self.read_at + size) % self.storage.len();
        self.length -= size;
        &self.storage[read_at..read_at + size]
    }

    fn peek(&self, offset: usize, size: usize) -> &[u8] {
        if offset > self.length { panic!("peeking {} octets past free space", offset) }
        let (read_at, size) = self.clamp_reader(offset, size);
        &self.storage[read_at..read_at + size]
    }

    fn advance(&mut self, size: usize) {
        if size > self.length { panic!("advancing {} octets into free space", size) }
        self.read_at = (self.read_at + size) % self.storage.len();
        self.length -= size;
    }
}

impl<'a> Into<SocketBuffer<'a>> for Managed<'a, [u8]> {
    fn into(self) -> SocketBuffer<'a> {
        SocketBuffer::new(self)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &State::Closed      => write!(f, "CLOSED"),
            &State::Listen      => write!(f, "LISTEN"),
            &State::SynSent     => write!(f, "SYN_SENT"),
            &State::SynReceived => write!(f, "SYN_RECEIVED"),
            &State::Established => write!(f, "ESTABLISHED"),
            &State::FinWait1    => write!(f, "FIN_WAIT_1"),
            &State::FinWait2    => write!(f, "FIN_WAIT_2"),
            &State::CloseWait   => write!(f, "CLOSE_WAIT"),
            &State::Closing     => write!(f, "CLOSING"),
            &State::LastAck     => write!(f, "LAST_ACK"),
            &State::TimeWait    => write!(f, "TIME_WAIT")
        }
    }
}

#[derive(Debug)]
struct Retransmit {
    sent: bool // FIXME
}

impl Retransmit {
    fn new() -> Retransmit {
        Retransmit { sent: false }
    }

    fn reset(&mut self) {
        self.sent = false
    }

    fn check(&mut self) -> bool {
        let result = !self.sent;
        self.sent = true;
        result
    }
}

/// A Transmission Control Protocol data stream.
#[derive(Debug)]
pub struct TcpSocket<'a> {
    /// State of the socket.
    state:           State,
    /// Address passed to `listen()`. `listen_address` is set when `listen()` is called and
    /// used every time the socket is reset back to the `LISTEN` state.
    listen_address:  IpAddress,
    /// Current local endpoint. This is used for both filtering the incoming packets and
    /// setting the source address. When listening or initiating connection on/from
    /// an unspecified address, this field is updated with the chosen source address before
    /// any packets are sent.
    local_endpoint:  IpEndpoint,
    /// Current remote endpoint. This is used for both filtering the incoming packets and
    /// setting the destination address.
    remote_endpoint: IpEndpoint,
    /// The sequence number corresponding to the beginning of the transmit buffer.
    /// I.e. an ACK(local_seq_no+n) packet removes n bytes from the transmit buffer.
    local_seq_no:    i32,
    /// The sequence number corresponding to the beginning of the receive buffer.
    /// I.e. userspace reading n bytes adds n to remote_seq_no.
    remote_seq_no:   i32,
    /// The last sequence number sent.
    /// I.e. in an idle socket, local_seq_no+tx_buffer.len().
    remote_last_seq: i32,
    /// The last acknowledgement number sent.
    /// I.e. in an idle socket, remote_seq_no+rx_buffer.len().
    remote_last_ack: i32,
    /// The speculative remote window size.
    /// I.e. the actual remote window size minus the count of in-flight octets.
    remote_win_len:  usize,
    retransmit:      Retransmit,
    rx_buffer:       SocketBuffer<'a>,
    tx_buffer:       SocketBuffer<'a>
}

impl<'a> TcpSocket<'a> {
    /// Create a socket using the given buffers.
    pub fn new<T>(rx_buffer: T, tx_buffer: T) -> Socket<'a, 'static>
            where T: Into<SocketBuffer<'a>> {
        let rx_buffer = rx_buffer.into();
        if rx_buffer.capacity() > <u16>::max_value() as usize {
            panic!("buffers larger than {} require window scaling, which is not implemented",
                   <u16>::max_value())
        }

        Socket::Tcp(TcpSocket {
            state:           State::Closed,
            listen_address:  IpAddress::default(),
            local_endpoint:  IpEndpoint::default(),
            remote_endpoint: IpEndpoint::default(),
            local_seq_no:    0,
            remote_seq_no:   0,
            remote_last_seq: 0,
            remote_last_ack: 0,
            remote_win_len:  0,
            retransmit:      Retransmit::new(),
            tx_buffer:       tx_buffer.into(),
            rx_buffer:       rx_buffer.into()
        })
    }

    /// Return the connection state.
    #[inline(always)]
    pub fn state(&self) -> State {
        self.state
    }

    /// Return the local endpoint.
    #[inline(always)]
    pub fn local_endpoint(&self) -> IpEndpoint {
        self.local_endpoint
    }

    /// Return the remote endpoint.
    #[inline(always)]
    pub fn remote_endpoint(&self) -> IpEndpoint {
        self.remote_endpoint
    }

    fn set_state(&mut self, state: State) {
        if self.state != state {
            if self.remote_endpoint.addr.is_unspecified() {
                net_trace!("tcp:{}: state={}→{}",
                           self.local_endpoint, self.state, state);
            } else {
                net_trace!("tcp:{}:{}: state={}→{}",
                           self.local_endpoint, self.remote_endpoint, self.state, state);
            }
        }
        self.state = state
    }

    /// Start listening on the given endpoint.
    ///
    /// # Panics
    /// This function will panic if the socket is not in the CLOSED state.
    pub fn listen(&mut self, endpoint: IpEndpoint) {
        assert!(self.state == State::Closed);

        self.listen_address  = endpoint.addr;
        self.local_endpoint  = endpoint;
        self.remote_endpoint = IpEndpoint::default();
        self.set_state(State::Listen);
    }

    /// Enqueue a sequence of octets to be sent, and return a pointer to it.
    ///
    /// This function may return a slice smaller than the requested size in case
    /// there is not enough contiguous free space in the transmit buffer, down to
    /// an empty slice.
    pub fn send(&mut self, size: usize) -> &mut [u8] {
        let buffer = self.tx_buffer.enqueue(size);
        if buffer.len() > 0 {
            net_trace!("tcp:{}:{}: tx buffer: enqueueing {} octets",
                       self.local_endpoint, self.remote_endpoint, buffer.len());
        }
        buffer
    }

    /// Enqueue a sequence of octets to be sent, and fill it from a slice.
    ///
    /// This function returns the amount of bytes actually enqueued, which is limited
    /// by the amount of free space in the transmit buffer; down to zero.
    ///
    /// See also [send](#method.send).
    pub fn send_slice(&mut self, data: &[u8]) -> usize {
        let buffer = self.send(data.len());
        let data = &data[..buffer.len()];
        buffer.copy_from_slice(data);
        buffer.len()
    }

    /// Dequeue a sequence of received octets, and return a pointer to it.
    ///
    /// This function may return a slice smaller than the requested size in case
    /// there are not enough octets queued in the receive buffer, down to
    /// an empty slice.
    pub fn recv(&mut self, size: usize) -> &[u8] {
        let buffer = self.rx_buffer.dequeue(size);
        self.remote_seq_no += buffer.len() as i32;
        if buffer.len() > 0 {
            net_trace!("tcp:{}:{}: rx buffer: dequeueing {} octets",
                       self.local_endpoint, self.remote_endpoint, buffer.len());
        }
        buffer
    }

    /// Dequeue a sequence of received octets, and fill a slice from it.
    ///
    /// This function returns the amount of bytes actually dequeued, which is limited
    /// by the amount of free space in the transmit buffer; down to zero.
    ///
    /// See also [recv](#method.recv).
    pub fn recv_slice(&mut self, data: &mut [u8]) -> usize {
        let buffer = self.recv(data.len());
        let data = &mut data[..buffer.len()];
        data.copy_from_slice(buffer);
        buffer.len()
    }

    /// See [Socket::collect](enum.Socket.html#method.collect).
    pub fn collect(&mut self, ip_repr: &IpRepr, payload: &[u8]) -> Result<(), Error> {
        if ip_repr.protocol() != IpProtocol::Tcp { return Err(Error::Rejected) }

        let packet = try!(TcpPacket::new(payload));
        let repr = try!(TcpRepr::parse(&packet, &ip_repr.src_addr(), &ip_repr.dst_addr()));

        // Reject packets with a wrong destination.
        if self.local_endpoint.port != repr.dst_port { return Err(Error::Rejected) }
        if !self.local_endpoint.addr.is_unspecified() &&
           self.local_endpoint.addr != ip_repr.dst_addr() { return Err(Error::Rejected) }

        // Reject packets from a source to which we aren't connected.
        if self.remote_endpoint.port != 0 &&
           self.remote_endpoint.port != repr.src_port { return Err(Error::Rejected) }
        if !self.remote_endpoint.addr.is_unspecified() &&
           self.remote_endpoint.addr != ip_repr.src_addr() { return Err(Error::Rejected) }

        // Reject packets addressed to a closed socket.
        if self.state == State::Closed {
            net_trace!("tcp:{}:{}:{}: packet received by a closed socket",
                       self.local_endpoint, ip_repr.src_addr(), repr.src_port);
            return Err(Error::Malformed)
        }

        // Reject unacceptable acknowledgements.
        match (self.state, repr) {
            // The initial SYN (or whatever) cannot contain an acknowledgement.
            (State::Listen, TcpRepr { ack_number: Some(_), .. }) => {
                net_trace!("tcp:{}:{}: ACK received by a socket in LISTEN state",
                           self.local_endpoint, self.remote_endpoint);
                return Err(Error::Malformed)
            }
            (State::Listen, TcpRepr { ack_number: None, .. }) => (),
            // A reset received in response to initial SYN is acceptable if it acknowledges
            // the initial SYN.
            (State::SynSent, TcpRepr { control: TcpControl::Rst, ack_number: None, .. }) => {
                net_trace!("tcp:{}:{}: unacceptable RST (expecting RST|ACK) \
                            in response to initial SYN",
                           self.local_endpoint, self.remote_endpoint);
                return Err(Error::Malformed)
            }
            (State::SynSent, TcpRepr {
                control: TcpControl::Rst, ack_number: Some(ack_number), ..
            }) => {
                if ack_number != self.local_seq_no {
                    net_trace!("tcp:{}:{}: unacceptable RST|ACK in response to initial SYN",
                               self.local_endpoint, self.remote_endpoint);
                    return Err(Error::Malformed)
                }
            }
            // Every packet after the initial SYN must be an acknowledgement.
            (_, TcpRepr { ack_number: None, .. }) => {
                net_trace!("tcp:{}:{}: expecting an ACK",
                           self.local_endpoint, self.remote_endpoint);
                return Err(Error::Malformed)
            }
            // Every acknowledgement must be for transmitted but unacknowledged data.
            (state, TcpRepr { ack_number: Some(ack_number), .. }) => {
                let control_len = match state {
                    // In SYN_SENT or SYN_RECEIVED, we've just sent a SYN.
                    State::SynSent | State::SynReceived => 1,
                    // In FIN_WAIT_1 or LAST_ACK, we've just sent a FIN.
                    State::FinWait1 | State::LastAck => 1,
                    // In all other states we've already got acknowledgemetns for
                    // all of the control flags we sent.
                    _ => 0
                };
                let unacknowledged = self.tx_buffer.len() as i32 + control_len;
                if !(ack_number - self.local_seq_no >= 0 &&
                     ack_number - (self.local_seq_no + unacknowledged) <= 0) {
                    net_trace!("tcp:{}:{}: unacceptable ACK ({} not in {}..{})",
                               self.local_endpoint, self.remote_endpoint,
                               ack_number, self.local_seq_no, self.local_seq_no + unacknowledged);
                    return Err(Error::Malformed)
                }
            }
        }

        match (self.state, repr) {
            // In LISTEN and SYN_SENT states, we have not yet synchronized with the remote end.
            (State::Listen, _)  => (),
            (State::SynSent, _) => (),
            // In all other states, segments must occupy a valid portion of the receive window.
            // For now, do not try to reassemble out-of-order segments.
            (_, TcpRepr { seq_number, .. }) => {
                let next_remote_seq = self.remote_seq_no + self.rx_buffer.len() as i32;
                if seq_number - next_remote_seq > 0 {
                    net_trace!("tcp:{}:{}: unacceptable SEQ ({} not in {}..)",
                               self.local_endpoint, self.remote_endpoint,
                               seq_number, next_remote_seq);
                    return Err(Error::Malformed)
                } else if seq_number - next_remote_seq != 0 {
                    net_trace!("tcp:{}:{}: duplicate SEQ ({} in ..{})",
                               self.local_endpoint, self.remote_endpoint,
                               seq_number, next_remote_seq);
                    return Ok(())
                }
            }
        }

        // Validate and update the state.
        match (self.state, repr) {
            // RSTs are ignored in the LISTEN state.
            (State::Listen, TcpRepr { control: TcpControl::Rst, .. }) =>
                return Ok(()),

            // RSTs in SYN_RECEIVED flip the socket back to the LISTEN state.
            (State::SynReceived, TcpRepr { control: TcpControl::Rst, .. }) => {
                self.local_endpoint.addr = self.listen_address;
                self.remote_endpoint     = IpEndpoint::default();
                self.set_state(State::Listen);
                return Ok(())
            }

            // RSTs in any other state close the socket.
            (_, TcpRepr { control: TcpControl::Rst, .. }) => {
                self.local_endpoint  = IpEndpoint::default();
                self.remote_endpoint = IpEndpoint::default();
                self.set_state(State::Closed);
                return Ok(())
            }

            // SYN packets in the LISTEN state change it to SYN_RECEIVED.
            (State::Listen, TcpRepr {
                src_port, dst_port, control: TcpControl::Syn, seq_number, ack_number: None, ..
            }) => {
                self.local_endpoint  = IpEndpoint::new(ip_repr.dst_addr(), dst_port);
                self.remote_endpoint = IpEndpoint::new(ip_repr.src_addr(), src_port);
                self.local_seq_no    = -seq_number; // FIXME: use something more secure
                self.remote_last_seq = self.local_seq_no + 1;
                self.remote_seq_no   = seq_number + 1;
                self.set_state(State::SynReceived);
                self.retransmit.reset()
            }

            // ACK packets in the SYN_RECEIVED state change it to ESTABLISHED.
            (State::SynReceived, TcpRepr { control: TcpControl::None, .. }) => {
                self.local_seq_no   += 1;
                self.set_state(State::Established);
                self.retransmit.reset()
            }

            // ACK packets in ESTABLISHED state do nothing.
            (State::Established, TcpRepr { control: TcpControl::None, .. }) => (),

            // FIN packets in ESTABLISHED state indicate the remote side has closed.
            (State::Established, TcpRepr { control: TcpControl::Fin, .. }) => {
                self.remote_seq_no  += 1;
                self.set_state(State::CloseWait);
                self.retransmit.reset()
            }

            _ => {
                net_trace!("tcp:{}:{}: unexpected packet {}",
                           self.local_endpoint, self.remote_endpoint, repr);
                return Err(Error::Malformed)
            }
        }

        // Dequeue acknowledged octets.
        if let Some(ack_number) = repr.ack_number {
            let ack_length = ack_number - self.local_seq_no;
            if ack_length > 0 {
                net_trace!("tcp:{}:{}: tx buffer: dequeueing {} octets",
                           self.local_endpoint, self.remote_endpoint,
                           ack_length);
            }
            self.tx_buffer.advance(ack_length as usize);
            self.local_seq_no = ack_number;
        }

        // Enqueue payload octets, which is guaranteed to be in order, unless we already did.
        if repr.payload.len() > 0 {
            net_trace!("tcp:{}:{}: rx buffer: enqueueing {} octets",
                       self.local_endpoint, self.remote_endpoint, repr.payload.len());
            self.rx_buffer.enqueue_slice(repr.payload)
        }

        // Update window length.
        self.remote_win_len = repr.window_len as usize;

        Ok(())
    }

    /// See [Socket::dispatch](enum.Socket.html#method.dispatch).
    pub fn dispatch<F, R>(&mut self, emit: &mut F) -> Result<R, Error>
            where F: FnMut(&IpRepr, &IpPayload) -> Result<R, Error> {
        let ip_repr = IpRepr::Unspecified {
            src_addr: self.local_endpoint.addr,
            dst_addr: self.remote_endpoint.addr,
            protocol: IpProtocol::Tcp,
        };
        let mut repr = TcpRepr {
            src_port:   self.local_endpoint.port,
            dst_port:   self.remote_endpoint.port,
            control:    TcpControl::None,
            seq_number: self.local_seq_no,
            ack_number: None,
            window_len: self.rx_buffer.window() as u16,
            payload:    &[]
        };

        let mut should_send = false;
        match self.state {
            State::Closed | State::Listen => return Err(Error::Exhausted),

            State::SynReceived => {
                if !self.retransmit.check() { return Err(Error::Exhausted) }

                repr.control = TcpControl::Syn;
                net_trace!("tcp:{}:{}: sending SYN|ACK",
                           self.local_endpoint, self.remote_endpoint);
                should_send = true;
            }

            State::Established |
            State::CloseWait => {
                // See if we should send data to the remote end because:
                //   1. the retransmit timer has expired, or...
                let mut may_send = self.retransmit.check();
                //   2. we've got new data in the transmit buffer.
                let remote_next_seq = self.local_seq_no + self.tx_buffer.len() as i32;
                if self.remote_last_seq != remote_next_seq {
                    may_send = true;
                }

                if self.tx_buffer.len() > 0 && self.remote_win_len > 0 && may_send {
                    // We can send something, so let's do that.
                    let offset = self.remote_last_seq - self.local_seq_no;
                    let mut size = self.remote_win_len;
                    // Clamp to MSS. Currently we only support the default MSS value.
                    if size > 536 { size = 536 }
                    // Extract data from the buffer. This may return less than what we want,
                    // in case it's not possible to extract a contiguous slice.
                    let data = self.tx_buffer.peek(offset as usize, size);
                    // Send the extracted data.
                    net_trace!("tcp:{}:{}: tx buffer: peeking at {} octets",
                               self.local_endpoint, self.remote_endpoint, data.len());
                    repr.payload = data;
                    // Speculatively shrink the remote window. This will get updated the next
                    // time we receive a packet.
                    self.remote_win_len -= data.len();
                    // Advance the in-flight sequence number.
                    self.remote_last_seq += data.len() as i32;
                    should_send = true;
                }
            }

            _ => unreachable!()
        }

        let ack_number = self.remote_seq_no + self.rx_buffer.len() as i32;
        if !should_send && self.remote_last_ack != ack_number {
            // Acknowledge all data we have received, since it is all in order.
            net_trace!("tcp:{}:{}: sending ACK",
                       self.local_endpoint, self.remote_endpoint);
            should_send = true;
        }

        if should_send {
            repr.ack_number = Some(ack_number);
            self.remote_last_ack = ack_number;

            emit(&ip_repr, &repr)
        } else {
            Err(Error::Exhausted)
        }
    }
}

impl<'a> IpPayload for TcpRepr<'a> {
    fn buffer_len(&self) -> usize {
        self.buffer_len()
    }

    fn emit(&self, ip_repr: &IpRepr, payload: &mut [u8]) {
        let mut packet = TcpPacket::new(payload).expect("undersized payload");
        self.emit(&mut packet, &ip_repr.src_addr(), &ip_repr.dst_addr())
    }
}

#[cfg(test)]
mod test {
    use wire::IpAddress;
    use super::*;

    #[test]
    fn test_buffer() {
        let mut buffer = SocketBuffer::new(vec![0; 8]); // ........
        buffer.enqueue(6).copy_from_slice(b"foobar");   // foobar..
        assert_eq!(buffer.dequeue(3), b"foo");          // ...bar..
        buffer.enqueue(6).copy_from_slice(b"ba");       // ...barba
        buffer.enqueue(4).copy_from_slice(b"zho");      // zhobarba
        assert_eq!(buffer.dequeue(6), b"barba");        // zho.....
        assert_eq!(buffer.dequeue(8), b"zho");          // ........
        buffer.enqueue(8).copy_from_slice(b"gefug");    // ...gefug
    }

    #[test]
    fn test_buffer_wraparound() {
        let mut buffer = SocketBuffer::new(vec![0; 8]); // ........
        buffer.enqueue_slice(&b"foobar"[..]);           // foobar..
        assert_eq!(buffer.dequeue(3), b"foo");          // ...bar..
        buffer.enqueue_slice(&b"bazhoge"[..]);          // zhobarba
    }

    const LOCAL_IP:     IpAddress  = IpAddress::v4(10, 0, 0, 1);
    const REMOTE_IP:    IpAddress  = IpAddress::v4(10, 0, 0, 2);
    const LOCAL_PORT:   u16        = 80;
    const REMOTE_PORT:  u16        = 49500;
    const LOCAL_END:    IpEndpoint = IpEndpoint::new(LOCAL_IP, LOCAL_PORT);
    const REMOTE_END:   IpEndpoint = IpEndpoint::new(REMOTE_IP, REMOTE_PORT);
    const LOCAL_SEQ:    i32        = 10000;
    const REMOTE_SEQ:   i32        = -10000;

    const SEND_TEMPL: TcpRepr<'static> = TcpRepr {
        src_port: REMOTE_PORT, dst_port: LOCAL_PORT,
        control: TcpControl::None,
        seq_number: 0, ack_number: Some(0),
        window_len: 256, payload: &[]
    };
    const RECV_TEMPL:  TcpRepr<'static> = TcpRepr {
        src_port: LOCAL_PORT, dst_port: REMOTE_PORT,
        control: TcpControl::None,
        seq_number: 0, ack_number: Some(0),
        window_len: 64, payload: &[]
    };

    fn send(socket: &mut TcpSocket, repr: &TcpRepr) -> Result<(), Error> {
        trace!("send: {}", repr);
        let mut buffer = vec![0; repr.buffer_len()];
        let mut packet = TcpPacket::new(&mut buffer).unwrap();
        repr.emit(&mut packet, &REMOTE_IP, &LOCAL_IP);
        let ip_repr = IpRepr::Unspecified {
            src_addr: REMOTE_IP,
            dst_addr: LOCAL_IP,
            protocol: IpProtocol::Tcp
        };
        socket.collect(&ip_repr, &packet.into_inner()[..])
    }

    fn recv<F>(socket: &mut TcpSocket, mut f: F)
            where F: FnMut(Result<TcpRepr, Error>) {
        let mut buffer = vec![];
        let result = socket.dispatch(&mut |ip_repr, payload| {
            assert_eq!(ip_repr.protocol(), IpProtocol::Tcp);
            assert_eq!(ip_repr.src_addr(), LOCAL_IP);
            assert_eq!(ip_repr.dst_addr(), REMOTE_IP);

            buffer.resize(payload.buffer_len(), 0);
            payload.emit(&ip_repr, &mut buffer[..]);
            let packet = TcpPacket::new(&buffer[..]).unwrap();
            let repr = try!(TcpRepr::parse(&packet, &ip_repr.src_addr(), &ip_repr.dst_addr()));
            trace!("recv: {}", repr);
            Ok(f(Ok(repr)))
        });
        // Appease borrow checker.
        match result {
            Ok(()) => (),
            Err(e) => f(Err(e))
        }
    }

    macro_rules! send {
        ($socket:ident, [$( $repr:expr )*]) => ({
            $( send!($socket, $repr, Ok(())); )*
        });
        ($socket:ident, $repr:expr, $result:expr) =>
            (assert_eq!(send(&mut $socket, &$repr), $result))
    }

    macro_rules! recv {
        ($socket:ident, [$( $repr:expr )*]) => ({
            $( recv!($socket, Ok($repr)); )*
            recv!($socket, Err(Error::Exhausted))
        });
        ($socket:ident, $result:expr) =>
            (recv(&mut $socket, |repr| assert_eq!(repr, $result)))
    }

    fn init_logger() {
        extern crate log;
        use std::boxed::Box;

        struct Logger(());

        impl log::Log for Logger {
            fn enabled(&self, _metadata: &log::LogMetadata) -> bool {
                true
            }

            fn log(&self, record: &log::LogRecord) {
                println!("{}", record.args());
            }
        }

        let _ = log::set_logger(|max_level| {
            max_level.set(log::LogLevelFilter::Trace);
            Box::new(Logger(()))
        });

        println!("");
    }

    fn socket() -> TcpSocket<'static> {
        init_logger();

        let rx_buffer = SocketBuffer::new(vec![0; 64]);
        let tx_buffer = SocketBuffer::new(vec![0; 64]);
        match TcpSocket::new(rx_buffer, tx_buffer) {
            Socket::Tcp(socket) => socket,
            _ => unreachable!()
        }
    }

    // =========================================================================================//
    // Tests for the CLOSED state.
    // =========================================================================================//
    #[test]
    fn test_closed() {
        let mut s = socket();
        assert_eq!(s.state(), State::Closed);

        send!(s, TcpRepr {
            control: TcpControl::Syn,
            ..SEND_TEMPL
        }, Err(Error::Rejected));
    }

    // =========================================================================================//
    // Tests for the LISTEN state.
    // =========================================================================================//
    fn socket_listen() -> TcpSocket<'static> {
        let mut s = socket();
        s.state           = State::Listen;
        s.local_endpoint  = IpEndpoint::new(IpAddress::default(), LOCAL_PORT);
        s
    }

    #[test]
    fn test_listen_syn_no_ack() {
        let mut s = socket_listen();
        send!(s, TcpRepr {
            control: TcpControl::Syn,
            seq_number: REMOTE_SEQ,
            ack_number: Some(LOCAL_SEQ),
            ..SEND_TEMPL
        }, Err(Error::Malformed));
        assert_eq!(s.state, State::Listen);
    }

    #[test]
    fn test_listen_rst() {
        let mut s = socket_listen();
        send!(s, [TcpRepr {
            control: TcpControl::Rst,
            seq_number: REMOTE_SEQ,
            ack_number: None,
            ..SEND_TEMPL
        }]);
    }

    // =========================================================================================//
    // Tests for the SYN_RECEIVED state.
    // =========================================================================================//
    fn socket_syn_received() -> TcpSocket<'static> {
        let mut s = socket();
        s.state           = State::SynReceived;
        s.local_endpoint  = LOCAL_END;
        s.remote_endpoint = REMOTE_END;
        s.local_seq_no    = LOCAL_SEQ;
        s.remote_seq_no   = REMOTE_SEQ;
        s
    }

    #[test]
    fn test_syn_received_rst() {
        let mut s = socket_syn_received();
        send!(s, [TcpRepr {
            control: TcpControl::Rst,
            seq_number: REMOTE_SEQ,
            ack_number: Some(LOCAL_SEQ),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.state, State::Listen);
        assert_eq!(s.local_endpoint, IpEndpoint::new(IpAddress::Unspecified, LOCAL_END.port));
        assert_eq!(s.remote_endpoint, IpEndpoint::default());
    }

    // =========================================================================================//
    // Tests for the SYN_SENT state.
    // =========================================================================================//
    fn socket_syn_sent() -> TcpSocket<'static> {
        let mut s = socket();
        s.state           = State::SynSent;
        s.local_endpoint  = LOCAL_END;
        s.remote_endpoint = REMOTE_END;
        s.local_seq_no    = LOCAL_SEQ;
        s
    }

    #[test]
    fn test_syn_sent_rst() {
        let mut s = socket_syn_sent();
        send!(s, [TcpRepr {
            control: TcpControl::Rst,
            seq_number: REMOTE_SEQ,
            ack_number: Some(LOCAL_SEQ),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.state, State::Closed);
    }

    #[test]
    fn test_syn_sent_rst_no_ack() {
        let mut s = socket_syn_sent();
        send!(s, TcpRepr {
            control: TcpControl::Rst,
            seq_number: REMOTE_SEQ,
            ack_number: None,
            ..SEND_TEMPL
        }, Err(Error::Malformed));
        assert_eq!(s.state, State::SynSent);
    }

    #[test]
    fn test_syn_sent_rst_bad_ack() {
        let mut s = socket_syn_sent();
        send!(s, TcpRepr {
            control: TcpControl::Rst,
            seq_number: REMOTE_SEQ,
            ack_number: Some(1234),
            ..SEND_TEMPL
        }, Err(Error::Malformed));
        assert_eq!(s.state, State::SynSent);
    }

    // =========================================================================================//
    // Tests for the ESTABLISHED state.
    // =========================================================================================//
    fn socket_established() -> TcpSocket<'static> {
        let mut s = socket();
        s.state          = State::Established;
        s.local_endpoint  = LOCAL_END;
        s.remote_endpoint = REMOTE_END;
        s.local_seq_no    = LOCAL_SEQ + 1;
        s.remote_seq_no   = REMOTE_SEQ + 1;
        s.remote_last_seq = LOCAL_SEQ + 1;
        s.remote_last_ack = REMOTE_SEQ + 1;
        s.remote_win_len  = 128;
        s
    }

    #[test]
    fn test_established_recv() {
        let mut s = socket_established();
        send!(s, [TcpRepr {
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 1),
            payload: &b"abcdef"[..],
            ..SEND_TEMPL
        }]);
        recv!(s, [TcpRepr {
            seq_number: LOCAL_SEQ + 1,
            ack_number: Some(REMOTE_SEQ + 1 + 6),
            window_len: 58,
            ..RECV_TEMPL
        }]);
        assert_eq!(s.rx_buffer.dequeue(6), &b"abcdef"[..]);
    }

    #[test]
    fn test_established_send() {
        let mut s = socket_established();
        // First roundtrip after establishing.
        s.tx_buffer.enqueue_slice(b"abcdef");
        recv!(s, [TcpRepr {
            seq_number: LOCAL_SEQ + 1,
            ack_number: Some(REMOTE_SEQ + 1),
            payload: &b"abcdef"[..],
            ..RECV_TEMPL
        }]);
        assert_eq!(s.tx_buffer.len(), 6);
        send!(s, [TcpRepr {
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 1 + 6),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.tx_buffer.len(), 0);
        // Second roundtrip.
        s.tx_buffer.enqueue_slice(b"foobar");
        recv!(s, [TcpRepr {
            seq_number: LOCAL_SEQ + 1 + 6,
            ack_number: Some(REMOTE_SEQ + 1),
            payload: &b"foobar"[..],
            ..RECV_TEMPL
        }]);
        send!(s, [TcpRepr {
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 1 + 6 + 6),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.tx_buffer.len(), 0);
    }

    #[test]
    fn test_established_no_ack() {
        let mut s = socket_established();
        send!(s, TcpRepr {
            seq_number: REMOTE_SEQ + 1,
            ack_number: None,
            ..SEND_TEMPL
        }, Err(Error::Malformed));
    }

    #[test]
    fn test_established_bad_ack() {
        let mut s = socket_established();
        // Already acknowledged data.
        send!(s, TcpRepr {
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ - 1),
            ..SEND_TEMPL
        }, Err(Error::Malformed));
        assert_eq!(s.local_seq_no, LOCAL_SEQ + 1);
        // Data not yet transmitted.
        send!(s, TcpRepr {
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 10),
            ..SEND_TEMPL
        }, Err(Error::Malformed));
        assert_eq!(s.local_seq_no, LOCAL_SEQ + 1);
    }

    #[test]
    fn test_established_bad_seq() {
        let mut s = socket_established();
        // Data outside of receive window.
        send!(s, TcpRepr {
            seq_number: REMOTE_SEQ + 1 + 256,
            ack_number: Some(LOCAL_SEQ + 1),
            ..SEND_TEMPL
        }, Err(Error::Malformed));
        assert_eq!(s.remote_seq_no, REMOTE_SEQ + 1);
    }

    #[test]
    fn test_established_fin() {
        let mut s = socket_established();
        send!(s, [TcpRepr {
            control: TcpControl::Fin,
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 1),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.state, State::CloseWait);
        recv!(s, [TcpRepr {
            seq_number: LOCAL_SEQ + 1,
            ack_number: Some(REMOTE_SEQ + 1 + 1),
            ..RECV_TEMPL
        }]);
    }

    #[test]
    fn test_established_send_fin() {
        let mut s = socket_established();
        s.tx_buffer.enqueue_slice(b"abcdef");
        send!(s, [TcpRepr {
            control: TcpControl::Fin,
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 1),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.state, State::CloseWait);
        recv!(s, [TcpRepr {
            seq_number: LOCAL_SEQ + 1,
            ack_number: Some(REMOTE_SEQ + 1 + 1),
            payload: &b"abcdef"[..],
            ..RECV_TEMPL
        }]);
    }

    #[test]
    fn test_established_rst() {
        let mut s = socket_established();
        send!(s, [TcpRepr {
            control: TcpControl::Rst,
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 1),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.state, State::Closed);
    }

    // =========================================================================================//
    // Tests for transitioning through multiple states.
    // =========================================================================================//
    #[test]
    fn test_listen() {
        let mut s = socket();
        s.listen(IpEndpoint::new(IpAddress::default(), LOCAL_PORT));
        assert_eq!(s.state, State::Listen);
    }

    #[test]
    fn test_three_way_handshake() {
        let mut s = socket();
        s.state           = State::Listen;
        s.local_endpoint  = IpEndpoint::new(IpAddress::default(), LOCAL_PORT);

        send!(s, [TcpRepr {
            control: TcpControl::Syn,
            seq_number: REMOTE_SEQ,
            ack_number: None,
            ..SEND_TEMPL
        }]);
        assert_eq!(s.state(), State::SynReceived);
        assert_eq!(s.local_endpoint(), LOCAL_END);
        assert_eq!(s.remote_endpoint(), REMOTE_END);
        recv!(s, [TcpRepr {
            control: TcpControl::Syn,
            seq_number: LOCAL_SEQ,
            ack_number: Some(REMOTE_SEQ + 1),
            ..RECV_TEMPL
        }]);
        send!(s, [TcpRepr {
            seq_number: REMOTE_SEQ + 1,
            ack_number: Some(LOCAL_SEQ + 1),
            ..SEND_TEMPL
        }]);
        assert_eq!(s.state(), State::Established);
        assert_eq!(s.local_seq_no, LOCAL_SEQ + 1);
        assert_eq!(s.remote_seq_no, REMOTE_SEQ + 1);
    }
}